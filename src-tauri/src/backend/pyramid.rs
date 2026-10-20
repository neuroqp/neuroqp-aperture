//! Display pyramid construction: header validation, previews and generated tiles.
use super::{
    cache::{cached_manifest, housekeeping, valid_cache_assets, valid_preview, write_manifest},
    process::{decoded_workspace, input, run, run_with_progress, uses_worker, worker_call},
    Job, Phase,
};
use crate::{
    channels::{self, Plane},
    native,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsString,
    fs,
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
};

pub(crate) fn field<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
}

pub(super) fn dimensions(header: &str) -> Result<(u32, u32), String> {
    let width = field(header, "width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let height = field(header, "height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err("Invalid or unsupported image dimensions.".into());
    }
    Ok((width, height))
}

/// The validated vipsheader fields that choose how a display is built.
#[derive(Debug, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    bands: u32,
    ushort: bool,
    tiff: bool,
    srgb: bool,
}

fn validate_header(header: &str) -> Result<Header, String> {
    let (width, height) = dimensions(header)?;
    let loader = field(header, "vips-loader");
    let tiff = loader == Some("tiffload");
    let bands = field(header, "bands")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|bands| {
            (1..=channels::MAX_CHANNELS).contains(bands) && (tiff || matches!(bands, 1 | 3 | 4))
        });
    let (Some(bands), Some(format @ ("uchar" | "ushort"))) = (bands, field(header, "format"))
    else {
        return Err("Supported pixels: unsigned 8/16-bit grayscale, RGB, RGBA, and TIFFs with up to 32 channels. Signed and floating-point pixels are not supported.".into());
    };
    if !matches!(
        loader,
        Some("jpegload" | "pngload" | "tiffload" | "magickload" | "bmp2vips" | "webpload")
    ) {
        return Err("This file does not decode as a supported image format.".into());
    }
    if loader == Some("magickload")
        && !field(header, "magick-format")
            .unwrap_or("")
            .starts_with("BMP")
    {
        return Err("Only BMP is supported through ImageMagick's decoder.".into());
    }
    let interpretation = field(header, "interpretation");
    if !matches!(
        interpretation,
        Some("b-w" | "grey16" | "srgb" | "rgb" | "rgb16" | "multiband")
    ) {
        return Err("Supported color interpretations: grayscale, RGB and sRGB. Convert other color spaces before opening.".into());
    }
    Ok(Header {
        width,
        height,
        bands,
        ushort: format == "ushort",
        tiff,
        srgb: interpretation == Some("srgb"),
    })
}

pub(crate) fn load(job: &Arc<Job>) -> Result<(), String> {
    let complete = job.root.join(&job.cache_key);
    let manifest = {
        let _access = job.cache_access.lock().unwrap();
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let manifest = cached_manifest(job, &complete);
        if let Some(manifest) = &manifest {
            *job.native.lock().unwrap() = manifest.native.clone();
            *job.planes.lock().unwrap() = manifest.planes.clone();
            if complete.join("complete").is_file() || job.preview_only {
                *job.dir.lock().unwrap() = complete.clone();
            } else {
                for index in 0..manifest.channels.len() {
                    let target = job.work.join("channels").join(index.to_string());
                    fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                    fs::copy(
                        complete
                            .join("channels")
                            .join(index.to_string())
                            .join("preview.png"),
                        target.join("preview.png"),
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        manifest
    };
    if let Some(manifest) = manifest {
        let fully_ready = complete.join("complete").is_file();
        job.update(|state| {
            state.width = manifest.width;
            state.height = manifest.height;
            state.orientation = manifest.orientation;
            state.channels = manifest.channels;
            state.channel_mode = manifest.channel_mode;
            state.dimensions = manifest.dimensions;
            state.calibration = manifest.calibration;
            state.display_ranges = manifest.display_ranges;
            state.projection = manifest.projection;
            state.preview_ready = true;
            state.tiles_ready = fully_ready;
            state.phase = if fully_ready || job.preview_only {
                Phase::Ready
            } else {
                Phase::Loading
            };
            state.progress = Some(if fully_ready { 1.0 } else { 0.2 });
            state.detail = "Reused display preview. Source pixels unchanged.".into();
        });
        fs::write(complete.join("access"), b"").map_err(|e| e.to_string())?;
        if fully_ready || job.preview_only {
            return housekeeping(job);
        }
    }
    let frame_channels = if let Some(selection) = job.selection {
        let acquisition = job.acquisition.as_ref().ok_or("Missing acquisition")?;
        let extracted = if job.projection == crate::frames::Projection::None {
            acquisition.extract_display(&job.input, selection, job.background, &job.cancel)?
        } else {
            acquisition.extract_projection(
                &job.input,
                selection,
                job.projection,
                &job.cancel,
                |fraction| job.report_progress(fraction),
            )?
        };
        let geometry = acquisition.geometry(selection)?;
        job.update(|state| {
            state.width = geometry.width;
            state.height = geometry.height;
            state.dimensions = Some(extracted.dimensions);
            state.channels = extracted.channels.clone();
            state.calibration = extracted.calibration;
            state.display_ranges = extracted.display_ranges;
            state.detail = "Selected frame decoded. Building display preview…".into();
        });
        Some(extracted.channels)
    } else {
        None
    };
    let header = run(job, "vipsheader", &["-a".into(), input(job, "")])?;
    let pixels = validate_header(&header)?;
    let (width, height) = (pixels.width, pixels.height);
    if !pixels.tiff {
        job.frame_memory.fetch_max(
            decoded_workspace(
                width,
                height,
                pixels.bands.into(),
                if pixels.ushort { 2 } else { 1 },
            )?,
            Ordering::Relaxed,
        );
    }
    let tiffinfo = if pixels.tiff {
        #[cfg(unix)]
        let info = run(job, "tiffinfo", &[job.input.clone().into_os_string()]).ok();
        #[cfg(windows)]
        let info = Some(worker_call(
            job,
            &crate::vips_worker::Request::TiffChannelInfo {
                input: job
                    .input
                    .to_str()
                    .ok_or("Image decoder path is not UTF-8")?
                    .into(),
            },
            None,
        )?);
        info
    } else {
        None
    };
    let layout = if let Some(channels) = frame_channels {
        channels::Layout {
            planes: (0..channels.len() as u32)
                .map(|band| Plane { page: 0, band })
                .collect(),
            channels,
            channel_mode: true,
        }
    } else {
        channels::discover(
            &header,
            &|options| run(job, "vipsheader", &["-a".into(), input(job, options)]),
            tiffinfo.as_deref(),
        )?
    };
    *job.planes.lock().unwrap() = layout.planes;
    let pages = field(&header, "n-pages").unwrap_or("1");
    job.update(|state| {
        state.channels = layout.channels;
        state.channel_mode = layout.channel_mode;
        state.width = width;
        state.height = height;
        state.orientation = field(&header, "orientation")
            .and_then(|s| s.parse().ok())
            .filter(|n| (1..=8).contains(n))
            .unwrap_or(1);
        state.detail = format!(
            "{width} × {height} · {}-bit · first image page of {pages}. Building preview…",
            if pixels.ushort { 16 } else { 8 }
        );
    });
    let headers = RefCell::new(HashMap::<String, Result<String, String>>::new());
    let inspect = |options: &str| {
        headers
            .borrow_mut()
            .entry(options.to_string())
            .or_insert_with(|| run(job, "vipsheader", &["-a".into(), input(job, options)]))
            .clone()
    };
    // Existing display bytes remain compatible. A legacy cache needs metadata
    // discovery once, then gains the manifest without rereading its source pixels.
    let legacy = complete.join("complete").is_file() && !complete.join("manifest.json").exists();
    let levels = if layout.channel_mode || legacy {
        native::discover(&header, &inspect, tiffinfo.as_deref())
    } else {
        native::discover_preview(&header, &inspect, tiffinfo.as_deref())
    };
    // Channel-page hyperstacks cannot reuse page-zero resolutions for other pages.
    *job.native.lock().unwrap() = if layout.channel_mode
        && job
            .planes
            .lock()
            .unwrap()
            .iter()
            .any(|plane| plane.page != 0)
    {
        Vec::new()
    } else {
        levels
    };
    if legacy {
        let reused = {
            let _access = job.cache_access.lock().unwrap();
            if job.cancel.load(Ordering::Relaxed) {
                return Err("Loading cancelled.".into());
            }
            let state = job.state.lock().unwrap().clone();
            let native = !job.native.lock().unwrap().is_empty();
            if valid_cache_assets(&complete, state.channels.len(), state.channel_mode, native) {
                write_manifest(job, &complete)?;
                *job.dir.lock().unwrap() = complete.clone();
                true
            } else {
                false
            }
        };
        if reused {
            job.update(|state| {
                state.phase = Phase::Ready;
                state.preview_ready = true;
                state.tiles_ready = true;
                state.progress = Some(1.0);
                state.detail = "Reused display cache. Source pixels unchanged.".into();
            });
            fs::write(complete.join("access"), b"").map_err(|e| e.to_string())?;
            return housekeeping(job);
        }
    }
    if layout.channel_mode {
        return load_channels(job, width, height, pixels.ushort);
    }
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let dir = job.dir.lock().unwrap().clone();
    let preview_partial = dir.join("preview.partial.jpg");
    // Explicit TIFF page=0 prevents unrelated pages from entering the displayed scene.
    // vips resize (rather than thumbnail) avoids automatic page-pyramid selection.
    let scale = (1024.0 / width.max(height) as f64).min(1.0);
    let options = if pixels.tiff {
        "[page=0,n=1,access=sequential]"
    } else {
        "[access=sequential]"
    };
    let preview_input = {
        let levels = job.native.lock().unwrap();
        levels
            .iter()
            .find(|level| level.width.max(level.height) <= 1024)
            .or_else(|| levels.last())
            .cloned()
    };
    let (preview_options, preview_scale) = match preview_input {
        Some(level) => (
            level.options,
            (1024.0 / level.width.max(level.height) as f64).min(1.0),
        ),
        None => (options.to_string(), scale),
    };
    // Large, ordinary gray/RGB8 strips use full-coverage area reduction. Other
    // layouts retain libvips, and full-resolution detail always uses libvips.
    let raw_preview = uses_worker(job)
        && (pixels.bands == 1 || (!pixels.ushort && pixels.bands == 3 && pixels.srgb))
        && u64::from(width) * u64::from(height) >= 16 * 1024 * 1024
        && preview_options == options
        && worker_call(
            job,
            &crate::vips_worker::Request::RawPreview {
                input: job
                    .input
                    .to_str()
                    .ok_or("Image decoder path is not UTF-8")?
                    .into(),
                output: preview_partial
                    .to_str()
                    .ok_or("Image decoder path is not UTF-8")?
                    .into(),
                max_edge: 1024,
                threads: thread::available_parallelism().map_or(1, |n| n.get()).min(
                    if u64::from(width) * u64::from(height) >= 256 * 1024 * 1024 {
                        16
                    } else {
                        8
                    },
                ),
            },
            Some((0.0, 0.2)),
        )? == "used";
    if !raw_preview {
        run_with_progress(
            job,
            "vips",
            &[
                "resize".into(),
                input(job, &preview_options),
                OsString::from(format!(
                    "{}[Q=85,strip,background=52 54 51]",
                    preview_partial.display()
                )),
                preview_scale.to_string().into(),
            ],
            Some((0.0, 0.2)),
        )?;
    }
    fs::rename(preview_partial, dir.join("preview.jpg")).map_err(|e| e.to_string())?;
    job.update(|s| {
        s.preview_ready = true;
        s.tiles_ready = false;
        s.progress = Some(0.2);
        s.detail = "Preview available. Sharper tiles are loading…".into();
    });
    *job.native.lock().unwrap() = native::discover(&header, &inspect, tiffinfo.as_deref());
    housekeeping(job)?;
    job.update(|state| state.tiles_ready = true);
    if job.native.lock().unwrap().is_empty() {
        // The direct JPEG saver avoids constructing a new libvips graph per tile.
        // Ushort keeps its existing shrink-then-convert order for pixel parity.
        let jpeg = if uses_worker(job) && !pixels.ushort && pixels.bands == 3 && pixels.srgb {
            "--Q=85"
        } else {
            "--suffix=.jpg[Q=85,strip,background=52 54 51]"
        };
        run_with_progress(
            job,
            "vips",
            &[
                "dzsave".into(),
                input(job, options),
                dir.join("tiles").into_os_string(),
                "--tile-size=256".into(),
                "--overlap=0".into(),
                jpeg.into(),
                "--background=52 54 51".into(),
                "--depth=onepixel".into(),
            ],
            Some((0.2, 0.99)),
        )?;
    }
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    write_manifest(job, &dir)?;
    fs::write(dir.join("complete"), b"v2").map_err(|e| e.to_string())?;
    fs::write(dir.join("access"), b"").map_err(|e| e.to_string())?;
    // Serialize promotion/eviction/cache selection. A replaced worker must never
    // remove a cache selected by the next image job.
    let access = job.cache_access.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let mut published = job.dir.lock().unwrap();
    // Keep a job-local source alias and temp directory for on-demand native tiles.
    let staging = job
        .root
        .join(format!("promote-{}", job.state.lock().unwrap().id));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    for name in [
        "preview.jpg",
        "manifest.json",
        "complete",
        "access",
        "tiles_files",
        "tiles.dzi",
    ] {
        let source = dir.join(name);
        if source.exists() {
            if name == "preview.jpg" {
                fs::hard_link(source, staging.join(name)).map_err(|e| e.to_string())?;
            } else {
                fs::rename(source, staging.join(name)).map_err(|e| e.to_string())?;
            }
        }
    }
    if complete.exists() {
        fs::remove_dir_all(&complete).map_err(|e| e.to_string())?;
    }
    fs::rename(staging, &complete).map_err(|e| e.to_string())?;
    *published = complete;
    drop(published);
    drop(access);
    job.update(|s| {
        s.phase = Phase::Ready;
        s.progress = Some(1.0);
        s.detail = if job.native.lock().unwrap().is_empty() {
            "Display pyramid ready. Source pixels unchanged."
        } else {
            "Using existing TIFF pyramid; detail loads on demand."
        }
        .into();
    });
    Ok(())
}

// Lossless display planes use a fixed 16-to-8-bit shift across every level.
pub(super) fn extract_plane(
    job: &Job,
    source: OsString,
    band: u32,
    destination: &Path,
    ushort: bool,
) -> Result<(), String> {
    let raw = destination.with_extension("raw.v");
    let normalized = destination.with_extension("normalized.v");
    let result = (|| {
        run(
            job,
            "vips",
            &[
                "extract_band".into(),
                source,
                raw.clone().into_os_string(),
                band.to_string().into(),
                "--n=1".into(),
            ],
        )?;
        let pixels = if ushort {
            run(
                job,
                "vips",
                &[
                    "cast".into(),
                    raw.clone().into_os_string(),
                    normalized.clone().into_os_string(),
                    "uchar".into(),
                    "--shift".into(),
                ],
            )?;
            &normalized
        } else {
            &raw
        };
        // PNG's grey16 interpretation otherwise expands uchar values back to 16 bits.
        run(
            job,
            "vips",
            &[
                "copy".into(),
                pixels.as_os_str().into(),
                destination.as_os_str().into(),
                "--interpretation=b-w".into(),
            ],
        )?;
        Ok(())
    })();
    let _ = fs::remove_file(raw);
    let _ = fs::remove_file(normalized);
    result
}

fn load_channels(job: &Arc<Job>, width: u32, height: u32, ushort: bool) -> Result<(), String> {
    let (preview_start, preview_end) = if job.projection == crate::frames::Projection::None {
        (0.0, 0.2)
    } else {
        (0.6, 0.7)
    };
    let planes = job.planes.lock().unwrap().clone();
    let native = !job.native.lock().unwrap().is_empty();
    let complete = job.root.join(&job.cache_key);
    let dir = job.dir.lock().unwrap().clone();
    let level = {
        let levels = job.native.lock().unwrap();
        levels
            .iter()
            .find(|level| level.width.max(level.height) <= 1024)
            .or_else(|| levels.last())
            .cloned()
    };
    let previews_ready = (0..planes.len()).all(|index| {
        valid_preview(
            &dir.join("channels")
                .join(index.to_string())
                .join("preview.png"),
            true,
        )
    });
    if !previews_ready {
        if uses_worker(job) {
            let (options, scale): (Vec<_>, Vec<_>) = planes
                .iter()
                .map(|plane| match &level {
                    Some(level) => (
                        level.options.clone(),
                        (1024.0 / level.width.max(level.height) as f64).min(1.0),
                    ),
                    None => (
                        format!("[page={},n=1,access=sequential]", plane.page),
                        (1024.0 / width.max(height) as f64).min(1.0),
                    ),
                })
                .unzip();
            worker_call(
                job,
                &crate::vips_worker::Request::ChannelPreviews {
                    input: job
                        .input
                        .to_str()
                        .ok_or("Image decoder path is not UTF-8")?
                        .into(),
                    options,
                    scale,
                    planes: planes.clone(),
                    ushort,
                    directory: dir
                        .to_str()
                        .ok_or("Image decoder path is not UTF-8")?
                        .into(),
                },
                Some((preview_start, preview_end)),
            )?;
        } else {
            for (index, plane) in planes.iter().enumerate() {
                let channel = dir.join("channels").join(index.to_string());
                fs::create_dir_all(&channel).map_err(|e| e.to_string())?;
                let (options, scale) = match &level {
                    Some(level) => (
                        level.options.clone(),
                        (1024.0 / level.width.max(level.height) as f64).min(1.0),
                    ),
                    None => (
                        format!("[page={},n=1,access=sequential]", plane.page),
                        (1024.0 / width.max(height) as f64).min(1.0),
                    ),
                };
                let resized = job.work.join(format!("preview-{index}.v"));
                run(
                    job,
                    "vips",
                    &[
                        "resize".into(),
                        input(job, &options),
                        resized.clone().into_os_string(),
                        scale.to_string().into(),
                    ],
                )?;
                let partial = channel.join("preview.partial.png");
                extract_plane(
                    job,
                    resized.clone().into_os_string(),
                    plane.band,
                    &partial,
                    ushort,
                )?;
                let _ = fs::remove_file(resized);
                fs::rename(partial, channel.join("preview.png")).map_err(|e| e.to_string())?;
            }
        }
    }
    job.update(|state| {
        state.preview_ready = true;
        state.tiles_ready = false;
        state.progress = Some(preview_end);
        state.detail = format!(
            "{} channel previews available. {} Sharper tiles are loading…",
            planes.len(),
            if state.read_only {
                "Selected frame; browsing only."
            } else {
                "First Z/T plane."
            }
        );
    });
    housekeeping(job)?;
    if !native && !job.preview_only {
        for (index, plane) in planes.iter().enumerate() {
            let start = preview_end + (0.99 - preview_end) * index as f64 / planes.len() as f64;
            let end = preview_end + (0.99 - preview_end) * (index + 1) as f64 / planes.len() as f64;
            let source = input(job, &format!("[page={},n=1,access=sequential]", plane.page));
            let destination = dir.join("channels").join(index.to_string()).join("tiles");
            if uses_worker(job) {
                worker_call(
                    job,
                    &crate::vips_worker::Request::ChannelPyramid {
                        input: source.to_str().ok_or("Invalid image path.")?.into(),
                        output: destination.to_str().ok_or("Invalid pyramid path.")?.into(),
                        band: plane.band,
                        ushort,
                    },
                    Some((start, end)),
                )?;
            } else {
                let normalized = job.work.join(format!("channel-{index}.v"));
                extract_plane(job, source, plane.band, &normalized, ushort)?;
                let result = run(
                    job,
                    "vips",
                    &[
                        "dzsave".into(),
                        normalized.clone().into_os_string(),
                        destination.into_os_string(),
                        "--tile-size=256".into(),
                        "--overlap=0".into(),
                        "--suffix=.png[strip]".into(),
                        "--depth=onepixel".into(),
                    ],
                );
                let _ = fs::remove_file(normalized);
                result?;
            }
            job.report_progress(end);
        }
    }
    write_manifest(job, &dir)?;
    let marker = if job.preview_only {
        "previews"
    } else {
        "complete"
    };
    fs::write(dir.join(marker), b"channels-v1").map_err(|e| e.to_string())?;
    fs::write(dir.join("access"), b"").map_err(|e| e.to_string())?;
    let access = job.cache_access.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let mut published = job.dir.lock().unwrap();
    let staging = job
        .root
        .join(format!("promote-{}", job.state.lock().unwrap().id));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    for name in ["channels", "manifest.json", marker, "access"] {
        fs::rename(dir.join(name), staging.join(name)).map_err(|e| e.to_string())?;
    }
    if complete.exists() {
        fs::remove_dir_all(&complete).map_err(|e| e.to_string())?;
    }
    fs::rename(staging, &complete).map_err(|e| e.to_string())?;
    *published = complete;
    drop(published);
    drop(access);
    job.update(|state| {
        state.phase = Phase::Ready;
        state.tiles_ready = !job.preview_only;
        state.progress = Some(1.0);
        state.detail = format!(
            "{} channels ready. {} Source pixels unchanged.",
            planes.len(),
            if state.read_only {
                "Selected frame; browsing only."
            } else {
                "First Z/T plane."
            }
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        assets::{channel_asset, max_level, read_asset, Asset},
        tests::{channel_fixture, wait_ready},
        Backend, ImageState, NEXT_ID,
    };
    use std::{
        process::Command,
        sync::Mutex,
        time::{Duration, Instant},
    };

    #[test]
    fn two_and_four_band_tiffs_keep_all_interleaved_channels() {
        if !cfg!(windows) && Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-small-channels-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        for bands in [2, 4] {
            let source = root.join(format!("channels [{bands}] 日本語.tif"));
            channel_fixture(&source, bands, 1, "");
            backend.open(&source).unwrap();
            let state = wait_ready(&backend);
            assert!(state.channel_mode);
            assert_eq!(state.channels.len(), bands as usize);
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn five_channel_pixels_are_distinct_lossless_display_planes_and_sources_stay_unchanged() {
        if Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-channel-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let mut backend = Backend::new(root.join("cache")).unwrap();
        let events = Arc::new(Mutex::new(Vec::<ImageState>::new()));
        let recorded = events.clone();
        backend.set_notifier(move |state| recorded.lock().unwrap().push(state.clone()));
        for (name, bands, pages, description) in [
            ("bands.tif", 5, 1, ""),
            (
                "imagej.tif",
                1,
                10,
                "ImageJ=1.54\nimages=10\nchannels=5\nslices=2\nframes=1\n",
            ),
        ] {
            let fixture = root.join(name);
            channel_fixture(&fixture, bands, pages, description);
            let original = fs::read(&fixture).unwrap();
            backend.open(&fixture).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let state = backend.snapshot().unwrap();
                if state.phase != Phase::Loading {
                    assert_eq!(state.phase, Phase::Ready, "{state:?}");
                    assert!(state.channel_mode);
                    assert_eq!(state.channels.len(), 5);
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
            let job = backend.current.lock().unwrap().clone().unwrap();
            let id = job.state.lock().unwrap().id.clone();
            assert!(
                !job.dir.lock().unwrap().join("preview.jpg").exists(),
                "channel previews must not encode an unused composite JPEG"
            );
            while !events
                .lock()
                .unwrap()
                .iter()
                .any(|state| state.id == id && state.phase == Phase::Ready)
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            let loading: Vec<_> = events
                .lock()
                .unwrap()
                .iter()
                .filter(|state| state.id == id && state.phase == Phase::Loading)
                .cloned()
                .collect();
            let fractions: Vec<_> = loading.iter().filter_map(|state| state.progress).collect();
            assert!(
                loading.first().unwrap().progress.is_none(),
                "metadata is indeterminate"
            );
            assert!(
                fractions.windows(2).all(|pair| pair[0] <= pair[1]),
                "progress regressed: {fractions:?}"
            );
            assert!(
                fractions.iter().all(|fraction| *fraction < 1.0),
                "100% is reserved for ready"
            );
            assert!(fractions.contains(&0.2), "preview completes the first 20%");
            assert!(
                fractions.contains(&0.35),
                "first of five pyramids advances overall work to 35%"
            );
            assert!(
                fractions.contains(&0.95),
                "final pyramid leaves final writes pending"
            );
            let before = events.lock().unwrap().len();
            job.report_progress(0.2);
            job.report_progress(0.99);
            assert_eq!(
                events.lock().unwrap().len(),
                before,
                "ready ignores ancillary progress"
            );
            for channel in 0..5 {
                for asset in [
                    Asset::ChannelPreview { channel },
                    Asset::ChannelTile {
                        channel,
                        level: max_level(12, 9),
                        x: 0,
                        y: 0,
                    },
                ] {
                    let png = read_asset(&job, asset).unwrap();
                    let check = root.join(format!("check-{channel}.png"));
                    fs::write(&check, png).unwrap();
                    let output = run(
                        &job,
                        "vips",
                        &[
                            "getpoint".into(),
                            check.into_os_string(),
                            "1".into(),
                            "1".into(),
                        ],
                    )
                    .unwrap();
                    assert_eq!(output.trim().parse::<u32>().unwrap(), (channel + 1) * 5);
                }
            }
            let id = backend.snapshot().unwrap().id;
            assert!(
                channel_asset(&format!("/{id}/channels/5/preview.png"), &id, 12, 9, 5).is_none()
            );
            assert!(
                channel_asset(&format!("/{id}/channels/0/tiles/4/1_0.png"), &id, 12, 9, 5)
                    .is_none()
            );
            assert!(
                channel_asset(&format!("/{id}/channels/0/tiles/4/0_0.png"), &id, 12, 9, 5)
                    .is_some()
            );
            assert_eq!(fs::read(fixture).unwrap(), original);
            backend.open(&root.join(name)).unwrap();
            loop {
                let state = backend.snapshot().unwrap();
                if state.phase != Phase::Loading {
                    assert!(state.detail.contains("Reused"), "{state:?}");
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                backend
                    .current
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .process_number
                    .load(Ordering::Relaxed),
                0,
                "channel cache without a JPEG must reuse its manifest directly"
            );
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reject_signed_float_and_excessive_channels() {
        let header="width: 12\nheight: 13\nbands: 1\nformat: ushort\nvips-loader: tiffload\ninterpretation: grey16\n";
        assert_eq!(
            validate_header(header).unwrap(),
            Header {
                width: 12,
                height: 13,
                bands: 1,
                ushort: true,
                tiff: true,
                srgb: false,
            }
        );
        assert!(validate_header(&header.replace("ushort", "float")).is_err());
        assert!(validate_header(&header.replace("bands: 1", "bands: 5")).is_ok());
        assert!(validate_header(&header.replace("bands: 1", "bands: 33")).is_err());
        assert!(validate_header(&header.replace("width: 12", "width: 0")).is_err());
    }
    #[test]
    fn actual_decoder_cache_and_cancellation() {
        if Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let backend = Backend::new(root.clone()).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/gray16-tiled-bigtiff.tif");
        let wait = || {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let state = backend.snapshot().unwrap();
                if state.phase != Phase::Loading {
                    return state;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
        };
        let corrupt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/corrupt.tif");
        backend.open(&corrupt).unwrap();
        let failed = wait();
        assert_eq!(failed.phase, Phase::Error);
        assert_eq!(
            failed.error.as_deref(),
            Some("Could not read this image. It may be damaged or use an unsupported format.")
        );
        let first = backend.open(&fixture).unwrap();
        let state = wait();
        assert_eq!(state.phase, Phase::Ready, "{:?}", state);
        assert_eq!((state.width, state.height), (65, 49));
        let job = backend.current.lock().unwrap().clone().unwrap();
        let bytes = read_asset(
            &job,
            Asset::Tile {
                level: 7,
                x: 0,
                y: 0,
            },
        )
        .unwrap();
        assert!(bytes.starts_with(&[255, 216]));
        // A native-only cache cannot satisfy generated tiles when native
        // metadata support disappears after changing the installed tools.
        fs::remove_dir_all(job.dir.lock().unwrap().join("tiles_files")).unwrap();
        backend.open(&fixture).unwrap();
        let rebuilt = wait();
        assert_eq!(rebuilt.phase, Phase::Ready);
        assert!(!rebuilt.detail.contains("Reused"));
        let rebuilt_job = backend.current.lock().unwrap().clone().unwrap();
        assert!(read_asset(
            &rebuilt_job,
            Asset::Tile {
                level: 7,
                x: 0,
                y: 0
            }
        )
        .is_ok());
        let second = backend.open(&fixture).unwrap();
        assert_ne!(first.id, second.id);
        let state = wait();
        assert_eq!(state.phase, Phase::Ready);
        assert!(state.detail.contains("Reused"));
        assert!(job.cancel.load(Ordering::Relaxed));
        assert_eq!(read_asset(&job, Asset::Preview).unwrap_err().0, 410);
        let live = backend.current.lock().unwrap().clone().unwrap();
        let sleeper = thread::spawn(move || run(&live, "sleep", &["30".into()]));
        thread::sleep(Duration::from_millis(100));
        let cancelled = std::time::Instant::now();
        backend.cancel();
        assert!(sleeper.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(cancelled.elapsed() < Duration::from_secs(2));
        assert_eq!(backend.snapshot().unwrap().phase, Phase::Cancelled);
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }
}
