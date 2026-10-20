//! Display assets served through the aperture:// protocol: previews and tiles.
use super::{
    cache::{tree_size, CACHE_LIMIT},
    process::{input, run},
    pyramid::{extract_plane, field},
    Backend, Job, Phase,
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

pub(super) fn max_level(width: u32, height: u32) -> u32 {
    32 - width.max(height).saturating_sub(1).leading_zeros()
}

#[derive(Debug, PartialEq)]
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
    let max = max_level(width, height);
    if width == 0 || height == 0 || level > max {
        return None;
    }
    let factor = 1_u64 << (max - level);
    let level_width = (width as u64).div_ceil(factor);
    let level_height = (height as u64).div_ceil(factor);
    if x as u64 >= level_width.div_ceil(TILE_SIZE as u64)
        || y as u64 >= level_height.div_ceil(TILE_SIZE as u64)
    {
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
    let factor = 1_u64 << (max_level(state.width, state.height) - level);
    let width = (state.width as u64).div_ceil(factor) as u32;
    let height = (state.height as u64).div_ceil(factor) as u32;
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
        responder.respond(response(503, b"Viewer shutting down.".to_vec()));
        return;
    }
    let path = request.uri().path().to_string();
    let job = backend.current.lock().unwrap().clone();
    let Some(job) = job else {
        responder.respond(response(404, b"No image loaded.".to_vec()));
        return;
    };
    let state = job.state.lock().unwrap().clone();
    let asset = asset(&path, &state.id, state.width, state.height).or_else(|| {
        state
            .channel_mode
            .then(|| {
                channel_asset(
                    &path,
                    &state.id,
                    state.width,
                    state.height,
                    state.channels.len(),
                )
            })
            .flatten()
    });
    let Some(asset) = asset else {
        responder.respond(response(404, b"Invalid image asset.".to_vec()));
        return;
    };
    if backend.requests.fetch_add(1, Ordering::Relaxed) >= 16 {
        backend.requests.fetch_sub(1, Ordering::Relaxed);
        responder.respond(response(503, b"Too many image requests.".to_vec()));
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
                tauri::async_runtime::spawn_blocking(move || read_asset(&active, asset))
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
            Ok(bytes) => responder.respond(response(200, bytes)),
            Err((status, error)) => responder.respond(response(status, error.into_bytes())),
        }
    });
}

fn response(status: u16, bytes: Vec<u8>) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(
            "Content-Type",
            if status == 200 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
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
