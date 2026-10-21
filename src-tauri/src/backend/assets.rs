//! Display assets served through the aperture:// protocol: previews and tiles.
use super::{
    cache::{tree_size, CACHE_LIMIT},
    process::{input, run},
    pyramid::{extract_plane, field},
    Backend, ImageState, Job, Phase,
};
use std::{
    ffi::OsString,
    fs,
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
    time::Duration,
};
use tauri::Manager;

const TILE_SIZE: u32 = 256;

pub(crate) fn max_level(width: u32, height: u32) -> u32 {
    32 - width.max(height).saturating_sub(1).leading_zeros()
}

/// Image size at a DeepZoom level; level `max_level` is full resolution.
fn level_size(width: u32, height: u32, level: u32) -> (u32, u32) {
    let factor = 1_u64 << (max_level(width, height) - level);
    (
        (width as u64).div_ceil(factor) as u32,
        (height as u64).div_ceil(factor) as u32,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Asset {
    Preview,
    Tile {
        level: u32,
        x: u32,
        y: u32,
    },
    ChannelPreview {
        channel: u32,
    },
    ChannelTile {
        channel: u32,
        level: u32,
        x: u32,
        y: u32,
    },
}

fn asset(path: &str, id: &str, width: u32, height: u32) -> Option<Asset> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if parts.first().copied() != Some(id) {
        return None;
    }
    if parts == [id, "preview.jpg"] {
        return Some(Asset::Preview);
    }
    if parts.len() != 4 || parts[1] != "tiles" {
        return None;
    }
    let level: u32 = parts[2].parse().ok()?;
    let (x, y) = parts[3].strip_suffix(".jpg")?.split_once('_')?;
    let (x, y): (u32, u32) = (x.parse().ok()?, y.parse().ok()?);
    if width == 0 || height == 0 || level > max_level(width, height) {
        return None;
    }
    let (level_width, level_height) = level_size(width, height, level);
    if x >= level_width.div_ceil(TILE_SIZE) || y >= level_height.div_ceil(TILE_SIZE) {
        return None;
    }
    Some(Asset::Tile { level, x, y })
}

pub(crate) fn channel_asset(
    path: &str,
    id: &str,
    width: u32,
    height: u32,
    channel_count: usize,
) -> Option<Asset> {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if parts.len() < 4 || parts[0] != id || parts[1] != "channels" {
        return None;
    }
    let channel: u32 = parts[2].parse().ok()?;
    if channel as usize >= channel_count {
        return None;
    }
    if parts.len() == 4 && parts[3] == "preview.png" {
        return Some(Asset::ChannelPreview { channel });
    }
    if parts.len() != 6 || parts[3] != "tiles" {
        return None;
    }
    let tile = parts[5].strip_suffix(".png")?;
    match asset(
        &format!("/{id}/tiles/{}/{tile}.jpg", parts[4]),
        id,
        width,
        height,
    )? {
        Asset::Tile { level, x, y } => Some(Asset::ChannelTile {
            channel,
            level,
            x,
            y,
        }),
        _ => None,
    }
}

/// Route a request path. `.u16` channel assets are the raw-sample form of the
/// cached 16-bit PNGs; the flag selects that response body.
fn requested(path: &str, state: &ImageState) -> Option<(Asset, bool)> {
    if let Some(asset) = asset(path, &state.id, state.width, state.height) {
        return Some((asset, false));
    }
    if !state.channel_mode {
        return None;
    }
    let (path, samples) = match path.strip_suffix(".u16") {
        Some(stem) if state.sample_bits == 16 => (format!("{stem}.png"), true),
        Some(_) => return None,
        None => (path.to_owned(), false),
    };
    let asset = channel_asset(
        &path,
        &state.id,
        state.width,
        state.height,
        state.channels.len(),
    )?;
    Some((asset, samples))
}

/// Exact tile bounds, including smaller edge tiles; previews fit 1024 pixels.
fn expected_size(asset: Asset, width: u32, height: u32, actual: (u32, u32)) -> bool {
    match asset {
        Asset::ChannelTile { level, x, y, .. } | Asset::Tile { level, x, y } => {
            let (level_width, level_height) = level_size(width, height, level);
            actual
                == (
                    TILE_SIZE.min(level_width - x * TILE_SIZE),
                    TILE_SIZE.min(level_height - y * TILE_SIZE),
                )
        }
        Asset::ChannelPreview { .. } | Asset::Preview => {
            (1..=width.min(1024)).contains(&actual.0) && (1..=height.min(1024)).contains(&actual.1)
        }
    }
}

/// Decode a cached 16-bit grayscale PNG into the `.u16` body: u32 LE width,
/// u32 LE height, then row-major u16 LE samples. Any other PNG is rejected.
pub(crate) fn u16_body(png: &[u8], valid: impl Fn(u32, u32) -> bool) -> Result<Vec<u8>, String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .map_err(|e| format!("Invalid channel PNG: {e}"))?;
    let info = reader.info();
    let (width, height) = (info.width, info.height);
    if info.color_type != png::ColorType::Grayscale || info.bit_depth != png::BitDepth::Sixteen {
        return Err("Channel PNG is not 16-bit grayscale.".into());
    }
    if !valid(width, height) {
        return Err("Channel PNG has unexpected dimensions.".into());
    }
    let length = reader
        .output_buffer_size()
        .filter(|length| *length as u64 == 2 * u64::from(width) * u64::from(height))
        .ok_or("Channel PNG has an unexpected sample layout.")?;
    // Decode in place after the prefix; PNG stores big-endian samples.
    let mut body = Vec::with_capacity(8 + length);
    body.extend(width.to_le_bytes());
    body.extend(height.to_le_bytes());
    body.resize(8 + length, 0);
    reader
        .next_frame(&mut body[8..])
        .map_err(|e| format!("Invalid channel PNG: {e}"))?;
    for sample in body[8..].as_chunks_mut::<2>().0 {
        sample.swap(0, 1);
    }
    Ok(body)
}

/// Read a display asset, waiting while it is generated and following the cache
/// directory when promotion publishes it. Channel assets are lossless PNGs.
pub(crate) fn read_asset(job: &Arc<Job>, asset: Asset) -> Result<Vec<u8>, (u16, String)> {
    let channel = match asset {
        Asset::Preview | Asset::Tile { .. } => None,
        Asset::ChannelPreview { channel } | Asset::ChannelTile { channel, .. } => Some(channel),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            return Err((410, "Image replaced or cancelled.".into()));
        }
        let state = job.state.lock().unwrap().clone();
        let failed = || (500, state.error.clone().unwrap_or_default());
        // Channel assets stop at a failed load; others still serve finished files.
        if channel.is_some() && state.phase == Phase::Error {
            return Err(failed());
        }
        // Windows cannot move a directory while a descendant file is open.
        // Keep path selection and each short read inside the promotion guard.
        let access = job.cache_access.lock().unwrap();
        let dir = job.dir.lock().unwrap().clone();
        let base = match channel {
            Some(channel) => dir.join("channels").join(channel.to_string()),
            None => dir.clone(),
        };
        let path = match asset {
            Asset::Preview => base.join("preview.jpg"),
            Asset::ChannelPreview { .. } => base.join("preview.png"),
            Asset::Tile { level, x, y } | Asset::ChannelTile { level, x, y, .. } => {
                if !job.native.lock().unwrap().is_empty() {
                    if state.phase == Phase::Error {
                        return Err(failed());
                    }
                    drop(access);
                    return native_tile(job, channel, level, x, y).map_err(|error| {
                        // A failed composite detail tile fails the whole image.
                        if channel.is_none() {
                            job.update(|s| {
                                s.phase = Phase::Error;
                                s.error = Some(error.clone());
                                s.detail = "Cannot load image detail.".into();
                            });
                        }
                        (500, error)
                    });
                }
                let extension = if channel.is_some() { "png" } else { "jpg" };
                base.join("tiles_files")
                    .join(level.to_string())
                    .join(format!("{x}_{y}.{extension}"))
            }
        };
        // libvips' direct DeepZoom saver uses .jpeg; older caches use .jpg.
        if let Ok(bytes) = fs::read(&path).or_else(|error| {
            if path.extension().is_some_and(|ext| ext == "jpg")
                && error.kind() == std::io::ErrorKind::NotFound
            {
                fs::read(path.with_extension("jpeg"))
            } else {
                Err(error)
            }
        }) {
            // dzsave writes each tile directly; only expose complete image streams.
            let complete = if channel.is_some() {
                bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.ends_with(b"IEND\xaeB`\x82")
            } else {
                bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
            };
            if complete {
                return Ok(bytes);
            }
        }
        drop(access);
        if *job.dir.lock().unwrap() != dir {
            continue;
        }
        if state.phase == Phase::Error {
            return Err(failed());
        }
        let kind = if channel.is_some() {
            "Channel tile"
        } else {
            "Tile"
        };
        if state.phase == Phase::Ready {
            return Err((404, format!("{kind} unavailable.")));
        }
        if std::time::Instant::now() > deadline {
            return Err((504, format!("{kind} generation timed out.")));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Render one tile from the nearest native level at least as large. Channel
/// tiles are resampled before their band is extracted as a lossless PNG.
fn native_tile(
    job: &Job,
    channel: Option<u32>,
    level: u32,
    x: u32,
    y: u32,
) -> Result<Vec<u8>, String> {
    let state = job.state.lock().unwrap().clone();
    let (width, height) = level_size(state.width, state.height, level);
    let source = {
        let levels = job.native.lock().unwrap();
        levels
            .iter()
            .rev()
            .find(|source| source.width >= width && source.height >= height)
            .unwrap_or(&levels[0])
            .clone()
    };
    let band = channel
        .map(|channel| {
            job.planes
                .lock()
                .unwrap()
                .get(channel as usize)
                .map(|plane| plane.band)
                .ok_or("Invalid image channel.")
        })
        .transpose()?;
    let tiles = |dir: &Path| match channel {
        Some(channel) => dir
            .join("channels")
            .join(channel.to_string())
            .join("native_tiles"),
        None => dir.join("native_tiles"),
    };
    let extension = if band.is_some() { "png" } else { "jpg" };
    let name = format!("{level}-{x}-{y}.{extension}");
    {
        let _access = job.cache_access.lock().unwrap();
        let tiles = tiles(&job.dir.lock().unwrap());
        fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
        if let Ok(bytes) = fs::read(tiles.join(&name)) {
            return Ok(bytes);
        }
    }
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let resized = job.work.join(format!("tile-{number}.v"));
    let partial = job.work.join(format!("tile-{number}.partial.{extension}"));
    let output = match band {
        Some(_) => resized.clone().into_os_string(),
        None => OsString::from(format!(
            "{}[Q=85,strip,background=52 54 51]",
            partial.display()
        )),
    };
    let result = (|| {
        // Affine's output area keeps the global sampling grid and exact edge sizes.
        // Demand-driven native TIFF loading reads the requested source region only.
        run(
            job,
            "vips",
            &[
                "affine".into(),
                input(job, &source.options),
                output,
                format!(
                    "{} 0 0 {}",
                    width as f64 / source.width as f64,
                    height as f64 / source.height as f64
                )
                .into(),
                format!(
                    "--oarea={} {} {} {}",
                    x * TILE_SIZE,
                    y * TILE_SIZE,
                    TILE_SIZE.min(width - x * TILE_SIZE),
                    TILE_SIZE.min(height - y * TILE_SIZE)
                )
                .into(),
                "--interpolate=bilinear".into(),
                "--extend=copy".into(),
            ],
        )?;
        if let Some(band) = band {
            let header = run(
                job,
                "vipsheader",
                &["-a".into(), resized.clone().into_os_string()],
            )?;
            extract_plane(
                job,
                resized.clone().into_os_string(),
                band,
                &partial,
                field(&header, "format") == Some("ushort"),
            )?;
        }
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        if tree_size(&job.root) > CACHE_LIMIT {
            return Err("Display cache limit reached.".into());
        }
        let _access = job.cache_access.lock().unwrap();
        let tiles = tiles(&job.dir.lock().unwrap());
        fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
        let destination = tiles.join(&name);
        fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
        fs::read(destination).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(resized);
    let _ = fs::remove_file(partial);
    result
}

/// Serve one aperture:// request from the current image off the WebView thread.
pub(crate) fn respond(
    app: tauri::AppHandle,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let backend = app.state::<Backend>();
    if backend.closing.load(Ordering::Relaxed) {
        responder.respond(response(503, b"Viewer shutting down.".to_vec(), false));
        return;
    }
    let path = request.uri().path().to_string();
    let job = backend.current.lock().unwrap().clone();
    let Some(job) = job else {
        responder.respond(response(404, b"No image loaded.".to_vec(), false));
        return;
    };
    let state = job.state.lock().unwrap().clone();
    let Some((asset, samples)) = requested(&path, &state) else {
        responder.respond(response(404, b"Invalid image asset.".to_vec(), false));
        return;
    };
    // At most 16 requests, so wide_display_bytes bounds `.u16` decode buffers.
    if backend.requests.fetch_add(1, Ordering::Relaxed) >= 16 {
        backend.requests.fetch_sub(1, Ordering::Relaxed);
        responder.respond(response(503, b"Too many image requests.".to_vec(), false));
        return;
    }
    let slots = backend.tile_slots.clone();
    tauri::async_runtime::spawn(async move {
        // Preview delivery must not queue behind unfinished pyramid tiles.
        let permit = if matches!(&asset, Asset::Preview | Asset::ChannelPreview { .. }) {
            Ok(None)
        } else {
            job.tile_permit(slots).await.map(Some)
        };
        let result = match permit.as_ref() {
            _ if job.cancel.load(Ordering::Relaxed) => {
                Err((410, "Image replaced or cancelled.".into()))
            }
            Ok(_) => {
                let active = job.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let bytes = read_asset(&active, asset)?;
                    if !samples {
                        return Ok(bytes);
                    }
                    u16_body(&bytes, |width, height| {
                        expected_size(asset, state.width, state.height, (width, height))
                    })
                    .map_err(|error| (500, error))
                })
                .await
                .unwrap_or_else(|e| Err((500, e.to_string())))
            }
            Err(error) => Err(error.clone()),
        };
        drop(permit);
        app.state::<Backend>()
            .requests
            .fetch_sub(1, Ordering::Relaxed);
        let result = if job.cancel.load(Ordering::Relaxed) {
            Err((410, "Image replaced or cancelled.".into()))
        } else {
            result
        };
        match result {
            Ok(bytes) => responder.respond(response(200, bytes, samples)),
            Err((status, error)) => responder.respond(response(status, error.into_bytes(), false)),
        }
    });
}

fn response(status: u16, bytes: Vec<u8>, samples: bool) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(
            "Content-Type",
            if status == 200 && samples {
                "application/octet-stream"
            } else if status == 200 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "image/png"
            } else if status == 200 {
                "image/jpeg"
            } else {
                "text/plain; charset=utf-8"
            },
        )
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .body(bytes)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{pyramid::dimensions, tests::wait_ready, NEXT_ID};
    use std::process::Command;

    #[test]
    fn stored_asset_reads_wait_for_promotion_and_use_the_published_directory() {
        let root = std::env::temp_dir().join(format!(
            "aperture-read-promotion-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");
        fs::write(&source, []).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        backend.shutdown();
        let job = backend.current.lock().unwrap().clone().unwrap();
        job.cancel.store(false, Ordering::Relaxed);
        job.state.lock().unwrap().phase = Phase::Ready;
        for (number, asset, relative, old, published_bytes) in [
            (
                0,
                Asset::Tile {
                    level: 0,
                    x: 0,
                    y: 0,
                },
                "tiles_files/0/0_0.jpg",
                vec![0xff, 0xd8, 1, 0xff, 0xd9],
                vec![0xff, 0xd8, 2, 0xff, 0xd9],
            ),
            (
                1,
                Asset::ChannelPreview { channel: 0 },
                "channels/0/preview.png",
                b"\x89PNG\r\n\x1a\noldIEND\xaeB`\x82".to_vec(),
                b"\x89PNG\r\n\x1a\nnewIEND\xaeB`\x82".to_vec(),
            ),
        ] {
            let work = root.join(format!("work-{number}"));
            let published = root.join(format!("published-{number}"));
            for (dir, bytes) in [(&work, &old), (&published, &published_bytes)] {
                let path = dir.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
            *job.dir.lock().unwrap() = work;
            let guard = job.cache_access.lock().unwrap();
            let (started_send, started_receive) = std::sync::mpsc::channel();
            let (done_send, done_receive) = std::sync::mpsc::channel();
            let reading = job.clone();
            let reader = thread::spawn(move || {
                started_send.send(()).unwrap();
                done_send.send(read_asset(&reading, asset)).unwrap();
            });
            started_receive.recv().unwrap();
            let blocked = done_receive.recv_timeout(Duration::from_millis(50));
            *job.dir.lock().unwrap() = published;
            drop(guard);
            let result = match blocked {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    done_receive.recv_timeout(Duration::from_secs(5)).unwrap()
                }
                result => panic!("Asset read crossed the held promotion guard: {result:?}"),
            };
            assert_eq!(result.unwrap(), published_bytes);
            reader.join().unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tile_bounds_and_traversal() {
        assert_eq!(max_level(65, 49), 7);
        assert_eq!(max_level(1, 1), 0);
        assert_eq!(
            asset("/id/tiles/7/0_0.jpg", "id", 65, 49),
            Some(Asset::Tile {
                level: 7,
                x: 0,
                y: 0
            })
        );
        for path in [
            "/id/tiles/7/1_0.jpg",
            "/id/tiles/8/0_0.jpg",
            "/id/tiles/7/../source.jpg",
            "/old/preview.jpg",
            "/id/tiles/7/4294967295_0.jpg",
        ] {
            assert!(asset(path, "id", 65, 49).is_none(), "{path}");
        }
        assert_eq!(
            asset("/id/tiles/0/0_0.jpg", "id", 65, 49),
            Some(Asset::Tile {
                level: 0,
                x: 0,
                y: 0
            })
        );
    }

    fn encoded(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        let samples = color.samples() * width as usize * height as usize;
        let data: Vec<u8> = match depth {
            png::BitDepth::Sixteen => (0..samples as u16)
                .flat_map(|i| (i * 257 + 1).to_be_bytes())
                .collect(),
            _ => (0..samples as u8).collect(),
        };
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
        bytes
    }

    #[test]
    fn u16_body_is_length_prefixed_little_endian_and_rejects_other_pngs() {
        use png::{BitDepth, ColorType};
        let gray16 = encoded(3, 2, ColorType::Grayscale, BitDepth::Sixteen);
        let body = u16_body(&gray16, |width, height| (width, height) == (3, 2)).unwrap();
        assert_eq!(body.len(), 8 + 2 * 3 * 2);
        assert_eq!(body[..8], [3, 0, 0, 0, 2, 0, 0, 0]);
        let samples: Vec<u16> = body[8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| u16::from_le_bytes(*sample))
            .collect();
        assert_eq!(samples, [1, 258, 515, 772, 1029, 1286]);
        // Wrong size, 8-bit depth and RGB16 are errors, never reinterpreted.
        assert!(u16_body(&gray16, |width, height| (width, height) == (2, 3))
            .unwrap_err()
            .contains("dimensions"));
        for png in [
            encoded(3, 2, ColorType::Grayscale, BitDepth::Eight),
            encoded(3, 2, ColorType::Rgb, BitDepth::Sixteen),
        ] {
            assert!(u16_body(&png, |_, _| true)
                .unwrap_err()
                .contains("16-bit grayscale"));
        }
        assert!(u16_body(b"not a png", |_, _| true).is_err());
        // Tiles match asset bounds exactly, including smaller edge tiles.
        let tile = |level, x, y| Asset::ChannelTile {
            channel: 0,
            level,
            x,
            y,
        };
        assert!(expected_size(tile(9, 1, 1), 300, 260, (44, 4)));
        assert!(!expected_size(tile(9, 1, 1), 300, 260, (256, 256)));
        assert!(expected_size(tile(9, 0, 0), 300, 260, (256, 256)));
        assert!(expected_size(tile(8, 0, 0), 300, 260, (150, 130)));
        let preview = Asset::ChannelPreview { channel: 0 };
        assert!(expected_size(preview, 3000, 20, (1024, 7)));
        assert!(!expected_size(preview, 3000, 20, (1025, 7)));
        assert!(!expected_size(preview, 3000, 20, (1024, 21)));
    }

    #[test]
    fn u16_routes_exist_only_for_16_bit_channel_images() {
        let (root, backend, job) = crate::backend::tests::workspace_fixture();
        let mut state = job.state.lock().unwrap().clone();
        state.id = "id".into();
        (state.width, state.height) = (300, 260);
        state.channels = vec![
            crate::channels::Channel {
                name: "Channel 1".into(),
                color: "#ffffff".into(),
                domain: [0, 65535],
            };
            2
        ];
        state.channel_mode = true;
        state.sample_bits = 16;
        assert_eq!(
            requested("/id/channels/1/preview.u16", &state),
            Some((Asset::ChannelPreview { channel: 1 }, true))
        );
        assert_eq!(
            requested("/id/channels/0/tiles/9/1_1.u16", &state),
            Some((
                Asset::ChannelTile {
                    channel: 0,
                    level: 9,
                    x: 1,
                    y: 1
                },
                true
            ))
        );
        assert_eq!(
            requested("/id/channels/0/preview.png", &state),
            Some((Asset::ChannelPreview { channel: 0 }, false))
        );
        for path in [
            "/id/channels/2/preview.u16",
            "/id/channels/0/tiles/9/2_0.u16",
            "/id/channels/0/preview.png.u16",
            "/id/channels/0/tiles/9/0_0.u16.png",
            "/other/channels/0/preview.u16",
            "/id/preview.u16",
        ] {
            assert!(requested(path, &state).is_none(), "{path}");
        }
        state.sample_bits = 8;
        assert!(requested("/id/channels/0/preview.u16", &state).is_none());
        assert!(requested("/id/channels/0/preview.png", &state).is_some());
        state.channel_mode = false;
        assert!(requested("/id/channels/0/preview.png", &state).is_none());
        assert!(requested("/id/tiles/9/0_0.jpg", &state).is_some());
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_reduced_ifd_pyramid_uses_existing_levels_without_building_tiles() {
        if !cfg!(windows) && Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-page-pyramid-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/rgb-odd-subifd-pyramid.tif");
        let source = root.join("reduced pages [pyramid] 日本語.tif");
        crate::tiff_annotations::pyramid_fixture_as_pages(&fixture, &source).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        let state = wait_ready(&backend);
        assert!(state.detail.contains("existing TIFF pyramid"), "{state:?}");
        let job = backend.current.lock().unwrap().clone().unwrap();
        let levels = job.native.lock().unwrap();
        assert!(levels.len() > 1);
        assert!(levels
            .iter()
            .skip(1)
            .all(|level| level.options.contains("page=") && !level.options.contains("subifd=")));
        drop(levels);
        assert!(!job.dir.lock().unwrap().join("tiles_files").exists());
        assert!(read_asset(
            &job,
            Asset::Tile {
                level: 12,
                x: 8,
                y: 4
            }
        )
        .is_ok());
        // A decoder that exited while idle is replaced for the next native tile.
        let decoder = job.decoder.lock().unwrap().clone();
        if let Some(decoder) = &decoder {
            decoder.stop();
        }
        assert!(read_asset(
            &job,
            Asset::Tile {
                level: 12,
                x: 0,
                y: 0
            }
        )
        .is_ok());
        if let Some(decoder) = decoder {
            assert!(!Arc::ptr_eq(
                &decoder,
                job.decoder.lock().unwrap().as_ref().unwrap()
            ));
        }
        // A decoder that crashes during a native tile is retried once.
        #[cfg(unix)]
        if job.decoder.lock().unwrap().is_some() {
            let (crashing, work) = crate::decoder::tests::helper("pass");
            let crashing = Arc::new(crashing);
            *job.decoder.lock().unwrap() = Some(crashing.clone());
            assert!(read_asset(
                &job,
                Asset::Tile {
                    level: 12,
                    x: 1,
                    y: 0
                }
            )
            .is_ok());
            assert!(crashing.stopped());
            let replacement = job.decoder.lock().unwrap().clone().unwrap();
            assert!(!Arc::ptr_eq(&crashing, &replacement));
            assert!(!replacement.stopped());
            fs::remove_dir_all(work).unwrap();
        }
        backend.shutdown();
        drop(job);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_16_bit_channel_tiles_keep_exact_samples() {
        if Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-native-16-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        // A tiled SubIFD pyramid whose samples are 100·x + 7.
        let source = root.join("gray16 pyramid.tif");
        let step = |args: &[&str]| {
            assert!(Command::new("vips")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success());
        };
        step(&["xyz", "xyz.v", "600", "400"]);
        step(&["extract_band", "xyz.v", "x.v", "0"]);
        step(&["linear", "x.v", "scaled.v", "100", "7"]);
        step(&["cast", "scaled.v", "samples.v", "ushort"]);
        step(&[
            "copy",
            "samples.v",
            "gray16 pyramid.tif[tile,pyramid,subifd,tile-width=64,tile-height=64]",
        ]);
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        let state = wait_ready(&backend);
        assert!(state.channel_mode);
        assert_eq!(state.sample_bits, 16);
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert!(job.native.lock().unwrap().len() > 1);
        assert!(!job
            .dir
            .lock()
            .unwrap()
            .join("channels/0/tiles_files")
            .exists());
        for (level, x, y) in [(10, 2, 1), (9, 1, 0), (0, 0, 0)] {
            let tile = Asset::ChannelTile {
                channel: 0,
                level,
                x,
                y,
            };
            let png = read_asset(&job, tile).unwrap();
            let body = u16_body(&png, |width, height| {
                expected_size(tile, 600, 400, (width, height))
            })
            .unwrap();
            if level == 10 {
                let width = u32::from_le_bytes(body[..4].try_into().unwrap());
                assert_eq!(width, 88);
                for (index, sample) in body[8..].as_chunks::<2>().0.iter().enumerate() {
                    let column = 512 + index as u32 % width;
                    assert_eq!(u32::from(u16::from_le_bytes(*sample)), 100 * column + 7);
                }
            }
        }
        backend.shutdown();
        drop(job);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_pyramid_reuse_and_odd_edge_geometry() {
        if Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-native-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let backend = Backend::new(root.clone()).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/rgb-odd-subifd-pyramid.tif");
        backend.open(&source).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let state = backend.snapshot().unwrap();
            if state.phase != Phase::Loading {
                assert_eq!(state.phase, Phase::Ready, "{:?}", state);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert!(job.native.lock().unwrap().len() > 1);
        assert!(!job.dir.lock().unwrap().join("tiles_files").exists());
        assert!(job.input.exists());
        for (level, x, y, expected) in [(12, 8, 4, (3, 1)), (11, 4, 2, (2, 1)), (0, 0, 0, (1, 1))] {
            let bytes = read_asset(&job, Asset::Tile { level, x, y }).unwrap();
            let image = root.join(format!("check-{level}.jpg"));
            fs::write(&image, bytes).unwrap();
            let output = Command::new("vipsheader")
                .arg("-a")
                .arg(&image)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                dimensions(std::str::from_utf8(&output.stdout).unwrap()).unwrap(),
                expected
            );
        }
        backend.open(&source).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let state = backend.snapshot().unwrap();
            if state.phase != Phase::Loading {
                assert_eq!(state.phase, Phase::Ready);
                assert!(state.detail.contains("Reused"));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        let second = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(second.process_number.load(Ordering::Relaxed), 0);
        assert!(second.input.exists());
        assert!(read_asset(
            &second,
            Asset::Tile {
                level: 12,
                x: 0,
                y: 0
            }
        )
        .is_ok());
        backend.shutdown();
        drop(job);
        drop(second);
        fs::remove_dir_all(root).unwrap();
    }
}
