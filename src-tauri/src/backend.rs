use crate::{
    channels::{self, Channel, Plane},
    native::{self, NativeLevel},
};
use serde::Serialize;
use std::{
    collections::hash_map::DefaultHasher,
    ffi::OsString,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const TILE_SIZE: u32 = 256;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageState {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub orientation: u8,
    pub phase: String,
    pub preview_ready: bool,
    pub tiles_ready: bool,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub detail: String,
    pub channels: Vec<Channel>,
    pub channel_mode: bool,
}

pub struct Job {
    pub state: Mutex<ImageState>,
    pub cancel: AtomicBool,
    pub dir: Mutex<PathBuf>,
    pub input: PathBuf,
    pub source: PathBuf,
    /// Initial metadata remains the sourceIndex basis through repeated saves.
    pub annotations: Mutex<Option<crate::annotation_io::AnnotationBasis>>,
    work: PathBuf,
    pub native: Mutex<Vec<NativeLevel>>,
    planes: Mutex<Vec<Plane>>,
    root: PathBuf,
    cache_key: String,
    process_number: AtomicU64,
}

pub struct Backend {
    pub current: Mutex<Option<Arc<Job>>>,
    root: PathBuf,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    pub requests: AtomicU64,
    pub closing: AtomicBool,
    pub tile_slots: Arc<tokio::sync::Semaphore>,
}

impl Backend {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        // Snapshot stale work at startup; deletion runs off the window thread.
        let stale: Vec<_> = fs::read_dir(&root)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("work-") || name.starts_with("promote-")
            })
            .map(|entry| entry.path())
            .collect();
        let cleanup = thread::spawn(move || {
            for path in stale {
                let _ = fs::remove_dir_all(path);
            }
        });
        Ok(Self {
            current: Mutex::new(None),
            root,
            workers: Mutex::new(vec![cleanup]),
            closing: AtomicBool::new(false),
            requests: AtomicU64::new(0),
            tile_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }

    pub fn open(&self, path: &Path) -> Result<ImageState, String> {
        if self.closing.load(Ordering::Relaxed) {
            return Err("Viewer shutting down.".into());
        }
        let source = fs::canonicalize(path).map_err(|e| format!("Cannot open image: {e}"))?;
        let metadata = fs::metadata(&source).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Choose a regular image file.".into());
        }
        let extension = source
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "jpg" | "jpeg" | "png" | "bmp" | "tif" | "tiff" | "webp"
        ) {
            return Err("Supported formats: JPEG, PNG, BMP, WebP, TIFF and BigTIFF.".into());
        }
        let mut hasher = DefaultHasher::new();
        "aperture-display-v4-channels".hash(&mut hasher);
        source.hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
                .hash(&mut hasher);
        }
        metadata
            .modified()
            .map_err(|e| e.to_string())?
            .hash(&mut hasher);
        let cache_key = format!("{:016x}", hasher.finish());
        let id = format!(
            "{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let dir = self.root.join(format!("work-{id}"));
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        fs::create_dir(dir.join("tmp")).map_err(|e| e.to_string())?;
        let input = dir.join(format!("source.{extension}"));
        // Loader options use brackets, so never pass a user filename to libvips.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &input).map_err(|e| e.to_string())?;
        #[cfg(not(unix))]
        return Err("This proof of concept currently supports Linux only.".into());
        let initial = ImageState {
            id,
            name: path
                .file_name()
                .unwrap_or(source.file_name().unwrap())
                .to_string_lossy()
                .into_owned(),
            width: 0,
            height: 0,
            orientation: 1,
            phase: "loading".into(),
            preview_ready: false,
            tiles_ready: false,
            progress: None,
            error: None,
            detail: "Reading image metadata…".into(),
            channels: Vec::new(),
            channel_mode: false,
        };
        let job = Arc::new(Job {
            state: Mutex::new(initial.clone()),
            cancel: AtomicBool::new(false),
            dir: Mutex::new(dir.clone()),
            input,
            source,
            annotations: Mutex::new(None),
            work: dir,
            native: Mutex::new(Vec::new()),
            planes: Mutex::new(Vec::new()),
            root: self.root.clone(),
            cache_key,
            process_number: AtomicU64::new(0),
        });
        let mut current = self.current.lock().unwrap();
        if let Some(old) = current.replace(job.clone()) {
            old.cancel();
        }
        drop(current);
        let mut workers = self.workers.lock().unwrap();
        workers.retain(|worker| !worker.is_finished());
        workers.push(thread::spawn(move || {
            if let Err(error) = load(&job) {
                let mut state = job.state.lock().unwrap();
                if !job.cancel.load(Ordering::Relaxed) {
                    state.phase = "error".into();
                    state.error = Some(error);
                    state.detail = "Image loading failed.".into();
                }
            }
            if job.cancel.load(Ordering::Relaxed) || job.state.lock().unwrap().phase == "error" {
                let dir = job.dir.lock().unwrap().clone();
                if dir
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("work-")
                {
                    let _ = fs::remove_dir_all(dir);
                }
            }
        }));
        Ok(initial)
    }

    pub fn snapshot(&self) -> Option<ImageState> {
        self.current
            .lock()
            .unwrap()
            .as_ref()
            .map(|job| job.state.lock().unwrap().clone())
    }
    pub fn cancel(&self) {
        if let Some(job) = self.current.lock().unwrap().as_ref() {
            job.cancel();
        }
    }
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.cancel();
        for worker in self.workers.lock().unwrap().drain(..) {
            let _ = worker.join();
        }
        while self.requests.load(Ordering::Relaxed) != 0 {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Job {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        state.phase = "cancelled".into();
        state.detail = "Loading cancelled.".into();
        state.progress = None;
    }
    fn update(&self, action: impl FnOnce(&mut ImageState)) {
        let mut state = self.state.lock().unwrap();
        if !self.cancel.load(Ordering::Relaxed) {
            action(&mut state);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.work);
    }
}

pub fn field<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
}

fn dimensions(header: &str) -> Result<(u32, u32), String> {
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

fn validate_header(header: &str) -> Result<(u32, u32), String> {
    let dims = dimensions(header)?;
    if !matches!(field(header, "format"), Some("uchar" | "ushort"))
        || !field(header, "bands")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|bands| {
                (1..=channels::MAX_CHANNELS).contains(&bands)
                    && (field(header, "vips-loader") == Some("tiffload")
                        || matches!(bands, 1 | 3 | 4))
            })
    {
        return Err("Supported pixels: unsigned 8/16-bit grayscale, RGB, RGBA, and TIFFs with up to 32 channels. Signed and floating-point pixels are not supported.".into());
    }
    if !matches!(
        field(header, "vips-loader"),
        Some("jpegload" | "pngload" | "tiffload" | "magickload" | "bmp2vips" | "webpload")
    ) {
        return Err("This file does not decode as a supported image format.".into());
    }
    if field(header, "vips-loader") == Some("magickload")
        && !field(header, "magick-format")
            .unwrap_or("")
            .starts_with("BMP")
    {
        return Err("Only BMP is supported through ImageMagick's decoder.".into());
    }
    if !matches!(
        field(header, "interpretation"),
        Some("b-w" | "grey16" | "srgb" | "rgb" | "rgb16" | "multiband")
    ) {
        return Err("Supported color interpretations: grayscale, RGB and sRGB. Convert other color spaces before opening.".into());
    }
    Ok(dims)
}

fn input(job: &Job, options: &str) -> OsString {
    OsString::from(format!("{}{}", job.input.display(), options))
}

fn load(job: &Arc<Job>) -> Result<(), String> {
    prune(&job.root, CACHE_LIMIT)?;
    let header = run(job, "vipsheader", &["-a".into(), input(job, "")])?;
    let (width, height) = validate_header(&header)?;
    let tiffinfo = if field(&header, "vips-loader") == Some("tiffload") {
        run(job, "tiffinfo", &[job.input.clone().into_os_string()]).ok()
    } else {
        None
    };
    let layout = channels::discover(
        &header,
        &|options| run(job, "vipsheader", &["-a".into(), input(job, options)]),
        tiffinfo.as_deref(),
    )?;
    *job.planes.lock().unwrap() = layout.planes;
    let pages = field(&header, "n-pages").unwrap_or("1");
    let pixel = field(&header, "format").unwrap_or("uchar");
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
            if pixel == "ushort" { 16 } else { 8 }
        );
    });
    let levels = native::discover(
        &header,
        &|options| run(job, "vipsheader", &["-a".into(), input(job, options)]),
        tiffinfo.as_deref(),
    );
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
    if layout.channel_mode {
        return load_channels(job, width, height, pixel == "ushort");
    }
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let complete = job.root.join(&job.cache_key);
    if complete.join("complete").is_file()
        && complete.join("preview.jpg").is_file()
        && (!job.native.lock().unwrap().is_empty() || complete.join("tiles_files").is_dir())
    {
        fs::write(complete.join("access"), b"").map_err(|e| e.to_string())?;
        *job.dir.lock().unwrap() = complete;
        job.update(|s| {
            s.phase = "ready".into();
            s.preview_ready = true;
            s.tiles_ready = true;
            s.progress = Some(1.0);
            s.detail = "Reused display cache. Source pixels unchanged.".into();
        });
        return Ok(());
    }
    prune(&job.root, CACHE_LIMIT / 2)?;
    let dir = job.dir.lock().unwrap().clone();
    let preview_partial = dir.join("preview.partial.jpg");
    // Explicit TIFF page=0 prevents unrelated pages from entering the displayed scene.
    // vips resize (rather than thumbnail) avoids automatic page-pyramid selection.
    let scale = (1024.0 / width.max(height) as f64).min(1.0);
    let options = if field(&header, "vips-loader") == Some("tiffload") {
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
    run(
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
    )?;
    fs::rename(preview_partial, dir.join("preview.jpg")).map_err(|e| e.to_string())?;
    job.update(|s| {
        s.preview_ready = true;
        s.tiles_ready = true;
        s.detail = "Preview available. Sharper tiles are loading…".into();
    });
    if job.native.lock().unwrap().is_empty() {
        run(
            job,
            "vips",
            &[
                "dzsave".into(),
                input(job, options),
                dir.join("tiles").into_os_string(),
                "--tile-size=256".into(),
                "--overlap=0".into(),
                "--suffix=.jpg[Q=85,strip,background=52 54 51]".into(),
                "--background=52 54 51".into(),
                "--depth=onepixel".into(),
            ],
        )?;
    }
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    fs::write(dir.join("complete"), b"v2").map_err(|e| e.to_string())?;
    fs::write(dir.join("access"), b"").map_err(|e| e.to_string())?;
    // Keep a job-local source alias and temp directory for on-demand native tiles.
    let staging = job
        .root
        .join(format!("promote-{}", job.state.lock().unwrap().id));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    for name in [
        "preview.jpg",
        "complete",
        "access",
        "tiles_files",
        "tiles.dzi",
    ] {
        let source = dir.join(name);
        if source.exists() {
            fs::rename(source, staging.join(name)).map_err(|e| e.to_string())?;
        }
    }
    if complete.exists() {
        fs::remove_dir_all(&complete).map_err(|e| e.to_string())?;
    }
    fs::rename(staging, &complete).map_err(|e| e.to_string())?;
    *job.dir.lock().unwrap() = complete;
    job.update(|s| {
        s.phase = "ready".into();
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
fn extract_plane(
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
    let planes = job.planes.lock().unwrap().clone();
    let native = !job.native.lock().unwrap().is_empty();
    let complete = job.root.join(&job.cache_key);
    if complete.join("complete").is_file()
        && planes.iter().enumerate().all(|(index, _)| {
            let channel = complete.join("channels").join(index.to_string());
            channel.join("preview.png").is_file()
                && (native || channel.join("tiles_files").is_dir())
        })
    {
        fs::write(complete.join("access"), b"").map_err(|e| e.to_string())?;
        *job.dir.lock().unwrap() = complete;
        job.update(|state| {
            state.phase = "ready".into();
            state.preview_ready = true;
            state.tiles_ready = true;
            state.progress = Some(1.0);
            state.detail = format!(
                "Reused {}-channel display cache. First Z/T plane.",
                planes.len()
            );
        });
        return Ok(());
    }
    prune(&job.root, CACHE_LIMIT / 2)?;
    let dir = job.dir.lock().unwrap().clone();
    for (index, plane) in planes.iter().enumerate() {
        let channel = dir.join("channels").join(index.to_string());
        fs::create_dir_all(&channel).map_err(|e| e.to_string())?;
        let level = {
            let levels = job.native.lock().unwrap();
            levels
                .iter()
                .find(|level| level.width.max(level.height) <= 1024)
                .or_else(|| levels.last())
                .cloned()
        };
        let (options, scale) = match level {
            Some(level) => (
                level.options,
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
        if index == 0 {
            // Display range histogram retains the first channel preview.
            run(
                job,
                "vips",
                &[
                    "copy".into(),
                    channel.join("preview.png").into_os_string(),
                    dir.join("preview.jpg").into_os_string(),
                ],
            )?;
        }
    }
    job.update(|state| {
        state.preview_ready = true;
        state.tiles_ready = !native;
        state.detail = format!(
            "{} channel previews available. First Z/T plane; sharper tiles are loading…",
            planes.len()
        );
    });
    if !native {
        for (index, plane) in planes.iter().enumerate() {
            let normalized = job.work.join(format!("channel-{index}.v"));
            extract_plane(
                job,
                input(job, &format!("[page={},n=1,access=sequential]", plane.page)),
                plane.band,
                &normalized,
                ushort,
            )?;
            let result = run(
                job,
                "vips",
                &[
                    "dzsave".into(),
                    normalized.clone().into_os_string(),
                    dir.join("channels")
                        .join(index.to_string())
                        .join("tiles")
                        .into_os_string(),
                    "--tile-size=256".into(),
                    "--overlap=0".into(),
                    "--suffix=.png[strip]".into(),
                    "--depth=onepixel".into(),
                ],
            );
            let _ = fs::remove_file(normalized);
            result?;
            job.update(|state| {
                state.progress = Some((index + 1) as f64 / planes.len() as f64);
            });
        }
    }
    fs::write(dir.join("complete"), b"channels-v1").map_err(|e| e.to_string())?;
    fs::write(dir.join("access"), b"").map_err(|e| e.to_string())?;
    let staging = job
        .root
        .join(format!("promote-{}", job.state.lock().unwrap().id));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    for name in ["preview.jpg", "channels", "complete", "access"] {
        fs::rename(dir.join(name), staging.join(name)).map_err(|e| e.to_string())?;
    }
    if complete.exists() {
        fs::remove_dir_all(&complete).map_err(|e| e.to_string())?;
    }
    fs::rename(staging, &complete).map_err(|e| e.to_string())?;
    *job.dir.lock().unwrap() = complete;
    job.update(|state| {
        state.phase = "ready".into();
        state.tiles_ready = true;
        state.progress = Some(1.0);
        state.detail = format!(
            "{} channels ready. First Z/T plane. Source pixels unchanged.",
            planes.len()
        );
    });
    Ok(())
}

pub fn channel_asset(
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

fn read_channel_asset(job: &Arc<Job>, asset: Asset) -> Result<Vec<u8>, (u16, String)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            return Err((410, "Image replaced or cancelled.".into()));
        }
        let state = job.state.lock().unwrap().clone();
        if state.phase == "error" {
            return Err((500, state.error.unwrap_or_default()));
        }
        let dir = job.dir.lock().unwrap().clone();
        let path = match asset {
            Asset::ChannelPreview { channel } => dir
                .join("channels")
                .join(channel.to_string())
                .join("preview.png"),
            Asset::ChannelTile {
                channel,
                level,
                x,
                y,
            } => {
                if !job.native.lock().unwrap().is_empty() {
                    return native_channel_tile(job, channel, level, x, y)
                        .map_err(|error| (500, error));
                }
                dir.join("channels")
                    .join(channel.to_string())
                    .join("tiles_files")
                    .join(level.to_string())
                    .join(format!("{x}_{y}.png"))
            }
            _ => unreachable!(),
        };
        if let Ok(bytes) = fs::read(path) {
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.ends_with(b"IEND\xaeB`\x82") {
                return Ok(bytes);
            }
        }
        if state.phase == "ready" {
            return Err((404, "Channel tile unavailable.".into()));
        }
        if std::time::Instant::now() > deadline {
            return Err((504, "Channel tile generation timed out.".into()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn native_channel_tile(
    job: &Job,
    channel: u32,
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
    let plane = job
        .planes
        .lock()
        .unwrap()
        .get(channel as usize)
        .cloned()
        .ok_or("Invalid image channel.")?;
    let tiles = job
        .dir
        .lock()
        .unwrap()
        .join("channels")
        .join(channel.to_string())
        .join("native_tiles");
    fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
    let destination = tiles.join(format!("{level}-{x}-{y}.png"));
    if let Ok(bytes) = fs::read(&destination) {
        return Ok(bytes);
    }
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let resized = job.work.join(format!("channel-tile-{number}.v"));
    let partial = job.work.join(format!("channel-tile-{number}.png"));
    let result = (|| {
        run(
            job,
            "vips",
            &[
                "affine".into(),
                input(job, &source.options),
                resized.clone().into_os_string(),
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
        let header = run(
            job,
            "vipsheader",
            &["-a".into(), resized.clone().into_os_string()],
        )?;
        extract_plane(
            job,
            resized.clone().into_os_string(),
            plane.band,
            &partial,
            field(&header, "format") == Some("ushort"),
        )?;
        fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
        fs::read(&destination).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(resized);
    let _ = fs::remove_file(partial);
    result
}

// Each decode lives in a killable process. libvips' operation cache is bounded;
// codec memory is not a hard RSS limit and should be measured on representative files.
pub(crate) fn run(job: &Job, program: &str, args: &[OsString]) -> Result<String, String> {
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let dir = job.work.clone();
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let stdout_path = dir.join(format!("stdout-{number}"));
    let stderr_path = dir.join(format!("stderr-{number}"));
    let stdout = File::create(&stdout_path).map_err(|e| e.to_string())?;
    let stderr = File::create(&stderr_path).map_err(|e| e.to_string())?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env("TMPDIR", dir.join("tmp"))
        .env("VIPS_CONCURRENCY", "2");
    if program == "vips" || program == "vipsheader" {
        command.args([
            "--vips-concurrency=2",
            "--vips-cache-max=0",
            "--vips-cache-max-memory=67108864",
            "--vips-disc-threshold=32m",
        ]);
    }
    let mut child = command.stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr)).spawn().map_err(|e| format!("Cannot run {program}: {e}. Install libvips tools (and libtiff tools for existing page pyramids)."))?;
    let mut ticks = 0;
    let status = loop {
        if job.cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(stdout_path);
            let _ = fs::remove_file(stderr_path);
            return Err("Loading cancelled.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
        if ticks % 5 == 0 && tree_size(&job.root) > CACHE_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err("The 2 GiB display cache limit was reached. This image needs more temporary disk space than this proof of concept allows.".into());
        }
        ticks += 1;
        thread::sleep(Duration::from_millis(100));
    };
    let mut output = String::new();
    File::open(&stdout_path)
        .map_err(|e| e.to_string())?
        .take(65536)
        .read_to_string(&mut output)
        .map_err(|e| e.to_string())?;
    if !status.success() {
        eprintln!("{program} exited with {status}:");
        if let Ok(mut errors) = File::open(&stderr_path) {
            let _ = std::io::copy(&mut errors, &mut std::io::stderr().lock());
        }
    }
    let _ = fs::remove_file(stdout_path);
    let _ = fs::remove_file(stderr_path);
    if tree_size(&job.root) > CACHE_LIMIT {
        return Err("Display cache limit reached.".into());
    }
    if status.success() {
        Ok(output)
    } else {
        Err("Could not read this image. It may be damaged or use an unsupported format.".into())
    }
}

pub fn max_level(width: u32, height: u32) -> u32 {
    32 - width.max(height).saturating_sub(1).leading_zeros()
}

#[derive(Debug, PartialEq)]
pub enum Asset {
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

pub fn asset(path: &str, id: &str, width: u32, height: u32) -> Option<Asset> {
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

pub fn read_asset(job: &Arc<Job>, asset: Asset) -> Result<Vec<u8>, (u16, String)> {
    if matches!(
        asset,
        Asset::ChannelPreview { .. } | Asset::ChannelTile { .. }
    ) {
        return read_channel_asset(job, asset);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            return Err((410, "Image replaced or cancelled.".into()));
        }
        let state = job.state.lock().unwrap().clone();
        let dir = job.dir.lock().unwrap().clone();
        let path = match asset {
            Asset::Preview => dir.join("preview.jpg"),
            Asset::ChannelPreview { .. } | Asset::ChannelTile { .. } => unreachable!(),
            Asset::Tile { level, x, y } => {
                if !job.native.lock().unwrap().is_empty() {
                    if state.phase == "error" {
                        return Err((500, state.error.unwrap_or_default()));
                    }
                    return native_tile(job, level, x, y).map_err(|error| {
                        job.update(|s| {
                            s.phase = "error".into();
                            s.error = Some(error.clone());
                            s.detail = "Cannot load image detail.".into();
                        });
                        (500, error)
                    });
                }
                dir.join("tiles_files")
                    .join(level.to_string())
                    .join(format!("{x}_{y}.jpg"))
            }
        };
        if let Ok(bytes) = fs::read(path) {
            // dzsave writes each tile directly; only expose complete JPEG streams.
            if bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) {
                return Ok(bytes);
            }
        }
        if state.phase == "error" {
            return Err((500, state.error.unwrap_or_default()));
        }
        if state.phase == "ready" {
            return Err((404, "Tile unavailable.".into()));
        }
        if std::time::Instant::now() > deadline {
            return Err((504, "Tile generation timed out.".into()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn native_tile(job: &Job, level: u32, x: u32, y: u32) -> Result<Vec<u8>, String> {
    let state = job.state.lock().unwrap().clone();
    let max = max_level(state.width, state.height);
    let factor = 1_u64 << (max - level);
    let target_width = (state.width as u64).div_ceil(factor) as u32;
    let target_height = (state.height as u64).div_ceil(factor) as u32;
    let levels = job.native.lock().unwrap();
    let source = levels
        .iter()
        .rev()
        .find(|v| v.width >= target_width && v.height >= target_height)
        .unwrap_or(&levels[0])
        .clone();
    drop(levels);
    let dir = job.dir.lock().unwrap().clone();
    let tiles = dir.join("native_tiles");
    fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
    let destination = tiles.join(format!("{level}-{x}-{y}.jpg"));
    if let Ok(bytes) = fs::read(&destination) {
        return Ok(bytes);
    }
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let partial = job.work.join(format!("tile-{number}.partial.jpg"));
    let tile_width = TILE_SIZE.min(target_width - x * TILE_SIZE);
    let tile_height = TILE_SIZE.min(target_height - y * TILE_SIZE);
    // Affine's output area keeps the global sampling grid and exact edge sizes.
    // Demand-driven native TIFF loading reads the requested source region only.
    let result = run(
        job,
        "vips",
        &[
            "affine".into(),
            input(job, &source.options),
            OsString::from(format!(
                "{}[Q=85,strip,background=52 54 51]",
                partial.display()
            )),
            format!(
                "{} 0 0 {}",
                target_width as f64 / source.width as f64,
                target_height as f64 / source.height as f64
            )
            .into(),
            format!(
                "--oarea={} {} {} {}",
                x * TILE_SIZE,
                y * TILE_SIZE,
                tile_width,
                tile_height
            )
            .into(),
            "--interpolate=bilinear".into(),
            "--extend=copy".into(),
        ],
    );
    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    if job.cancel.load(Ordering::Relaxed) {
        let _ = fs::remove_file(&partial);
        return Err("Loading cancelled.".into());
    }
    if tree_size(&job.root) > CACHE_LIMIT {
        let _ = fs::remove_file(&partial);
        return Err("Display cache limit reached.".into());
    }
    fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
    fs::read(destination).map_err(|e| e.to_string())
}

fn tree_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_symlink() => 0,
            Ok(kind) if kind.is_dir() => tree_size(&entry.path()),
            Ok(_) => entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn prune(root: &Path, limit: u64) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|entry| {
            entry.path().join("complete").is_file()
                && !entry.file_name().to_string_lossy().starts_with("work-")
        })
        .collect();
    entries.sort_by_key(|entry| {
        fs::metadata(entry.path().join("access"))
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let mut size = tree_size(root);
    for entry in entries {
        if size <= limit {
            break;
        }
        let bytes = tree_size(&entry.path());
        fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
        size = size.saturating_sub(bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Small generated TIFFs keep source data and validation caches out of Git.
    fn channel_fixture(path: &Path, bands: u16, pages: u16, description: &str) {
        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        for page in 0..pages {
            let directory = bytes.len() as u32;
            let count = 12_u16;
            let payload = directory + 2 + count as u32 * 12 + 4;
            let description_offset = payload;
            let bits_offset = description_offset + description.len() as u32 + 1;
            let pixels_offset = bits_offset + 2 * bands as u32;
            let pixel_bytes = 12 * 9 * bands as u32 * 2;
            let next = if page + 1 == pages {
                0
            } else {
                pixels_offset + pixel_bytes
            };
            bytes.extend(count.to_le_bytes());
            for (tag, kind, count, value) in [
                (256_u16, 4_u16, 1_u32, 12_u32),
                (257, 4, 1, 9),
                (
                    258,
                    3,
                    bands as u32,
                    if bands == 1 { 16 } else { bits_offset },
                ),
                (259, 3, 1, 1),
                (262, 3, 1, 1),
                (270, 2, description.len() as u32 + 1, description_offset),
                (273, 4, 1, pixels_offset),
                (274, 3, 1, 1),
                (277, 3, 1, bands as u32),
                (278, 4, 1, 9),
                (279, 4, 1, pixel_bytes),
                (284, 3, 1, 1),
            ] {
                bytes.extend(tag.to_le_bytes());
                bytes.extend(kind.to_le_bytes());
                bytes.extend(count.to_le_bytes());
                bytes.extend(value.to_le_bytes());
            }
            bytes.extend(next.to_le_bytes());
            bytes.extend(description.as_bytes());
            bytes.push(0);
            for _ in 0..bands {
                bytes.extend(16_u16.to_le_bytes());
            }
            for _ in 0..12 * 9 {
                for band in 0..bands {
                    bytes.extend(((page + band + 1) * 1285).to_le_bytes());
                }
            }
        }
        fs::write(path, bytes).unwrap();
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
        let backend = Backend::new(root.join("cache")).unwrap();
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
                if state.phase != "loading" {
                    assert_eq!(state.phase, "ready", "{state:?}");
                    assert!(state.channel_mode);
                    assert_eq!(state.channels.len(), 5);
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
            let job = backend.current.lock().unwrap().clone().unwrap();
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
                if state.phase != "loading" {
                    assert!(state.detail.contains("Reused"), "{state:?}");
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
        }
        backend.shutdown();
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
    fn reject_signed_float_and_excessive_channels() {
        let header="width: 12\nheight: 13\nbands: 1\nformat: ushort\nvips-loader: tiffload\ninterpretation: grey16\n";
        assert_eq!(validate_header(header).unwrap(), (12, 13));
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
                if state.phase != "loading" {
                    return state;
                }
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(20));
            }
        };
        let corrupt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/corrupt.tif");
        backend.open(&corrupt).unwrap();
        let failed = wait();
        assert_eq!(failed.phase, "error");
        assert_eq!(
            failed.error.as_deref(),
            Some("Could not read this image. It may be damaged or use an unsupported format.")
        );
        let first = backend.open(&fixture).unwrap();
        let state = wait();
        assert_eq!(state.phase, "ready", "{:?}", state);
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
        assert_eq!(rebuilt.phase, "ready");
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
        assert_eq!(state.phase, "ready");
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
        assert_eq!(backend.snapshot().unwrap().phase, "cancelled");
        backend.shutdown();
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
            if state.phase != "loading" {
                assert_eq!(state.phase, "ready", "{:?}", state);
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
            if state.phase != "loading" {
                assert_eq!(state.phase, "ready");
                assert!(state.detail.contains("Reused"));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        let second = backend.current.lock().unwrap().clone().unwrap();
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
    #[test]
    fn cache_pruning_never_follows_source_alias() {
        let root = std::env::temp_dir().join(format!(
            "aperture-prune-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("entry")).unwrap();
        fs::write(root.join("entry/complete"), b"ok").unwrap();
        fs::write(root.join("entry/pixels"), vec![0; 1024]).unwrap();
        prune(&root, 0).unwrap();
        assert!(!root.join("entry").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
