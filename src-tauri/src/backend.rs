pub(crate) mod assets;
pub(crate) mod cache;
pub(crate) mod ome;
mod process;
pub(crate) mod pyramid;

use crate::{
    channels::{Channel, Plane},
    native::NativeLevel,
};
use process::{decoded_workspace, DecoderMemory};
use pyramid::load;
use serde::Serialize;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fs::{self, File},
    future::Future,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Child,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    task::Poll,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Two 32-channel 8-bit overview sets during replacement and a 32 MiB
/// tile-cache allowance. Browser/runtime overhead and GPU differ.
const BROWSER_BYTES: u64 = 96 * 1024 * 1024;
const DISPLAY_MEMORY_ERROR: &str = "Image display does not fit the shared memory budget. Close another image window or increase the limit in Settings.";

/// Extra display memory for 16-bit channel planes: the second byte of each
/// 1024-pixel overview sample in both overview sets, the encoded PNG and
/// decoded body of each concurrent `.u16` preview response (at most 16), and
/// the viewer's 65536-bin histogram per channel plus one temporary. The tile
/// cache keeps its byte budget.
fn wide_display_bytes(channels: u64) -> u64 {
    const PREVIEW: u64 = 1024 * 1024 * 2;
    const HISTOGRAM: u64 = 65536 * 4;
    channels * (PREVIEW + HISTOGRAM) + channels.min(16) * 2 * PREVIEW + 2 * HISTOGRAM
}

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp", "nd2", "czi",
];
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Open also accepts an OME-TIFF companion, which folder navigation omits so a
/// multi-file dataset is not listed twice.
pub const COMPANION_EXTENSION: &str = "companion.ome";

fn openable(path: &Path) -> bool {
    supported_image(path) || crate::ome_tiff::is_companion(path)
}

/// ND2 and CZI by extension; OME-TIFF by its first page's OME-XML or a companion.
fn detect_format(path: &Path, source: &Path) -> Result<Option<crate::frames::Format>, String> {
    if let Some(format) = crate::frames::Format::from_path(path) {
        return Ok(Some(format));
    }
    let tiff = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["tif", "tiff"]
                .iter()
                .any(|t| extension.eq_ignore_ascii_case(t))
        });
    Ok(
        ((tiff || crate::ome_tiff::is_companion(path)) && crate::ome_tiff::detect(source)?)
            .then_some(crate::frames::Format::OmeTiff),
    )
}

fn supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

pub fn folder_images(path: &Path) -> Result<Vec<String>, String> {
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    let directory = path.parent().ok_or("Image has no containing folder.")?;
    let mut images = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| format!("Cannot read image folder: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if supported_image(&path) && path.is_file() {
            if let Some(path) = path.to_str() {
                images.push(path.to_owned());
            }
        }
    }
    images.sort_by_cached_key(|path| {
        let name = Path::new(path).file_name().unwrap().to_str().unwrap();
        (name.to_lowercase(), name.to_owned())
    });
    Ok(images)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageState {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub orientation: u8,
    pub phase: Phase,
    pub preview_ready: bool,
    pub tiles_ready: bool,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub detail: String,
    pub channels: Vec<Channel>,
    pub channel_mode: bool,
    /// Display sample width: 16 only for lossless 16-bit channel planes.
    pub sample_bits: u8,
    /// The three channels are R, G and B samples of one acquisition.
    pub rgb: bool,
    /// A 16-bit source shown with 8-bit display samples to fit the cache limit.
    pub display_fallback: bool,
    pub read_only: bool,
    pub dimensions: Option<crate::frames::Dimensions>,
    pub calibration: Option<crate::frames::Calibration>,
    pub display_ranges: Vec<[u16; 2]>,
    pub projection: crate::frames::Projection,
    /// OME-TIFF series left out because their dimensions differ from series 0.
    pub skipped_series: u32,
    /// Why max projections cannot be offered for the selected position.
    pub projection_unavailable: Option<String>,
}

/// Serialized as the frontend's lowercase `ImageState.phase` strings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Loading,
    Ready,
    Cancelled,
    Error,
}

/// A selected frame with the navigation state and channels seeded for it.
pub(crate) struct FrameSelection {
    pub selection: crate::frames::Selection,
    pub dimensions: crate::frames::Dimensions,
    pub channels: Vec<Channel>,
    pub sample_bits: u8,
    pub rgb: bool,
    pub projection: crate::frames::Projection,
    pub skipped_series: u32,
    pub projection_unavailable: Option<String>,
}

type Notifier = Arc<dyn Fn(&ImageState) + Send + Sync>;

pub struct Job {
    pub state: Mutex<ImageState>,
    pub cancel: AtomicBool,
    cancelled: tokio::sync::Notify,
    pub dir: Mutex<PathBuf>,
    pub input: PathBuf,
    pub source: PathBuf,
    pub(crate) format: Option<crate::frames::Format>,
    selection: Option<crate::frames::Selection>,
    pub(crate) projection: crate::frames::Projection,
    pub(crate) acquisition: Option<Arc<crate::preload::Acquisition>>,
    pub(crate) preview_only: bool,
    pub(crate) background: bool,
    memory: Arc<crate::memory::Budget>,
    /// Initial metadata remains the sourceIndex basis through repeated saves.
    pub annotations: Mutex<Option<crate::annotation_io::AnnotationBasis>>,
    work: PathBuf,
    native: Mutex<Vec<NativeLevel>>,
    planes: Mutex<Vec<Plane>>,
    root: PathBuf,
    /// OME-TIFF jobs add their members' identity once the dataset is open.
    cache_key: Mutex<String>,
    ome: OnceLock<Arc<ome::Source>>,
    process_number: AtomicU64,
    decoder: Mutex<Option<Arc<crate::decoder::Decoder>>>,
    decoder_memory: Mutex<Option<DecoderMemory>>,
    display_memory: Mutex<Option<crate::memory::Reservation>>,
    frame_memory: AtomicU64,
    processes: Mutex<HashMap<u64, Arc<Mutex<Child>>>>,
    notifier: Option<Notifier>,
    budget_check: Mutex<Instant>,
    housekeeping: AtomicBool,
    cache_access: Arc<Mutex<()>>,
}

#[derive(Clone, Copy)]
enum LoadMode {
    Foreground,
    Preview,
    Detail,
}

#[derive(Clone)]
pub(crate) struct LoadContext {
    pub root: PathBuf,
    pub memory: Arc<crate::memory::Budget>,
    pub cache_access: Arc<Mutex<()>>,
}

pub struct Backend {
    pub current: Mutex<Option<Arc<Job>>>,
    root: PathBuf,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    requests: AtomicU64,
    closing: Arc<AtomicBool>,
    tile_slots: Arc<tokio::sync::Semaphore>,
    notifier: Option<Notifier>,
    cache_access: Arc<Mutex<()>>,
    open_lock: Mutex<()>,
    pub memory: Arc<crate::memory::Budget>,
    acquisition: Arc<Mutex<Option<Arc<crate::preload::Acquisition>>>>,
    browser_memory: Mutex<Option<crate::memory::Reservation>>,
}

impl Backend {
    #[cfg(test)]
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let memory = crate::memory::Budget::new(root.clone())?;
        Self::with_memory(root, memory)
    }

    pub fn with_memory(root: PathBuf, memory: Arc<crate::memory::Budget>) -> Result<Self, String> {
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
        let closing = Arc::new(AtomicBool::new(false));
        let acquisition: Arc<Mutex<Option<Arc<crate::preload::Acquisition>>>> =
            Arc::new(Mutex::new(None));
        let maintenance = maintain(&closing, &acquisition, &memory);
        Ok(Self {
            current: Mutex::new(None),
            root,
            workers: Mutex::new(vec![cleanup, maintenance]),
            closing,
            requests: AtomicU64::new(0),
            tile_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            notifier: None,
            cache_access: Arc::new(Mutex::new(())),
            open_lock: Mutex::new(()),
            memory,
            acquisition,
            browser_memory: Mutex::new(None),
        })
    }

    pub fn set_notifier(&mut self, callback: impl Fn(&ImageState) + Send + Sync + 'static) {
        self.notifier = Some(Arc::new(callback));
    }

    #[cfg(test)]
    pub fn open(&self, path: &Path) -> Result<ImageState, String> {
        let _opening = self.open_lock.lock().unwrap();
        self.cancel();
        if self.closing.load(Ordering::Relaxed) {
            return Err("Viewer shutting down.".into());
        }
        let source = fs::canonicalize(path).map_err(|e| format!("Cannot open image: {e}"))?;
        self.open_source(path, source, None)
    }

    /// Keep the selected filename for display/format detection while loading the
    /// exact source already resolved for presence and fingerprint tracking.
    pub fn open_resolved(&self, path: &Path, source: &Path) -> Result<ImageState, String> {
        let _opening = self.open_lock.lock().unwrap();
        self.cancel();
        self.open_source(path, source.to_path_buf(), None)
    }

    /// Select a frame without changing the pinned source or the folder selection.
    #[cfg(test)]
    pub fn select_plane(
        &self,
        id: &str,
        selection: crate::frames::Selection,
    ) -> Result<ImageState, String> {
        self.select_view(id, selection, None)
    }

    pub fn select_view(
        &self,
        id: &str,
        selection: crate::frames::Selection,
        projection: Option<crate::frames::Projection>,
    ) -> Result<ImageState, String> {
        let _opening = self.open_lock.lock().unwrap();
        let job = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or("No image open")?;
        let state = job.state.lock().unwrap().clone();
        if state.id != id {
            return Err("The image has changed".into());
        }
        let dimensions = state
            .dimensions
            .as_ref()
            .ok_or("This image has no dimension navigation")?;
        if selection.position as usize >= dimensions.positions.len()
            || selection.z >= dimensions.z
            || selection.time >= dimensions.time
        {
            return Err("The selected position, Z or time is out of range.".into());
        }
        let projection = projection.unwrap_or(job.projection);
        let selection = projection.canonical(selection);
        // Seed navigation immediately; the worker independently validates the file metadata.
        let mut dimensions = dimensions.clone();
        dimensions.position = selection.position;
        dimensions.z_index = selection.z;
        dimensions.time_index = selection.time;
        self.open_source(
            Path::new(&state.path),
            job.source.clone(),
            Some(FrameSelection {
                selection,
                dimensions,
                channels: state.channels,
                sample_bits: state.sample_bits,
                rgb: state.rgb,
                projection,
                skipped_series: state.skipped_series,
                projection_unavailable: state.projection_unavailable,
            }),
        )
    }

    fn open_source(
        &self,
        path: &Path,
        source: PathBuf,
        frame: Option<FrameSelection>,
    ) -> Result<ImageState, String> {
        if self.closing.load(Ordering::Relaxed) {
            return Err("Viewer shutting down.".into());
        }
        {
            let mut browser = self.browser_memory.lock().unwrap();
            if browser.is_none() {
                *browser = Some(self.memory.require(BROWSER_BYTES, DISPLAY_MEMORY_ERROR)?);
            }
        }
        let acquisition = if let Some(format) = detect_format(path, &source)? {
            let identity =
                crate::instances::fingerprint(&source).map_err(|error| error.to_string())?;
            let mut current = self.acquisition.lock().unwrap();
            // Reopening an OME-TIFF re-reads its metadata and member identities.
            let reopen = format == crate::frames::Format::OmeTiff && frame.is_none();
            if reopen
                || current
                    .as_ref()
                    .is_none_or(|item| !item.matches(format, &source, &identity))
            {
                if let Some(old) = current.take() {
                    old.stop();
                }
                *current = Some(crate::preload::Acquisition::new(
                    format,
                    source.clone(),
                    identity,
                    self.memory.clone(),
                ));
            }
            current.clone()
        } else {
            if let Some(old) = self.acquisition.lock().unwrap().take() {
                old.stop();
            }
            None
        };
        if let Some(acquisition) = &acquisition {
            acquisition.foreground();
        }
        let context = LoadContext {
            root: self.root.clone(),
            memory: self.memory.clone(),
            cache_access: self.cache_access.clone(),
        };
        let job = context.create_job(
            path,
            source,
            frame,
            acquisition,
            LoadMode::Foreground,
            self.notifier.clone(),
        )?;
        let initial = job.state.lock().unwrap().clone();
        let old = self.current.lock().unwrap().replace(job.clone());
        if let Some(old) = old {
            old.cancel();
        }
        let mut workers = self.workers.lock().unwrap();
        workers.retain(|worker| !worker.is_finished());
        workers.push(thread::spawn(move || {
            if let Err(error) = load(&job) {
                job.update(|state| {
                    state.phase = Phase::Error;
                    state.error = Some(error);
                    state.detail = "Image loading failed.".into();
                });
            }
            finish_decoder(&job);
            if !job.cancel.load(Ordering::Relaxed)
                && job.state.lock().unwrap().phase == Phase::Ready
            {
                crate::preload::run(&context, &job);
            }
            if job.cancel.load(Ordering::Relaxed) || job.state.lock().unwrap().phase == Phase::Error
            {
                let decoder = job.decoder.lock().unwrap().clone();
                if let Some(decoder) = decoder {
                    decoder.stop();
                }
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

    pub fn navigation_hint(&self, axis: Option<&str>, direction: Option<i8>) {
        if let Some(acquisition) = self.acquisition.lock().unwrap().as_ref() {
            acquisition.hint(axis, direction);
        }
    }

    pub fn preload_status(&self) -> crate::preload::PreloadStatus {
        self.acquisition
            .lock()
            .unwrap()
            .as_ref()
            .map(|item| item.status())
            .unwrap_or_default()
    }

    pub fn set_memory_budget(&self, bytes: u64) -> Result<crate::memory::MemoryStatus, String> {
        self.memory.set_limit(bytes)?;
        if let Some(acquisition) = self.acquisition.lock().unwrap().as_ref() {
            acquisition.trim()?;
        }
        self.memory.status()
    }

    pub fn snapshot(&self) -> Option<ImageState> {
        self.current
            .lock()
            .unwrap()
            .as_ref()
            .map(|job| job.state.lock().unwrap().clone())
    }
    pub fn cancel(&self) {
        if let Some(acquisition) = self.acquisition.lock().unwrap().as_ref() {
            acquisition.foreground();
        }
        let job = self.current.lock().unwrap().clone();
        if let Some(job) = job {
            job.cancel();
        }
    }
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::Relaxed);
        if let Some(acquisition) = self.acquisition.lock().unwrap().take() {
            acquisition.stop();
        }
        self.cancel();
        for worker in self.workers.lock().unwrap().drain(..) {
            let _ = worker.join();
        }
        while self.requests.load(Ordering::Relaxed) != 0 {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Undo a shutdown whose update install failed; the user reopens the image.
    pub fn resume(&self) {
        let mut workers = self.workers.lock().unwrap();
        self.closing.store(false, Ordering::Relaxed);
        workers.push(maintain(&self.closing, &self.acquisition, &self.memory));
    }
}

fn maintain(
    closing: &Arc<AtomicBool>,
    acquisition: &Arc<Mutex<Option<Arc<crate::preload::Acquisition>>>>,
    memory: &Arc<crate::memory::Budget>,
) -> thread::JoinHandle<()> {
    let closing = Arc::downgrade(closing);
    let acquisition = acquisition.clone();
    let memory = memory.clone();
    thread::spawn(move || {
        let mut epoch = memory.reclaim_epoch().unwrap_or(0);
        while closing
            .upgrade()
            .is_some_and(|closing| !closing.load(Ordering::Relaxed))
        {
            let next = memory.reclaim_epoch().unwrap_or(epoch);
            let reclaim = next != epoch;
            epoch = next;
            let current = acquisition.lock().unwrap().clone();
            if let Some(current) = current {
                if reclaim {
                    current.foreground();
                    current.clear_optional();
                } else {
                    let _ = current.trim();
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
    })
}

impl LoadContext {
    pub(crate) fn background_job(
        &self,
        path: &Path,
        source: PathBuf,
        frame: FrameSelection,
        acquisition: Arc<crate::preload::Acquisition>,
        preview_only: bool,
    ) -> Result<Arc<Job>, String> {
        self.create_job(
            path,
            source,
            Some(frame),
            Some(acquisition),
            if preview_only {
                LoadMode::Preview
            } else {
                LoadMode::Detail
            },
            None,
        )
    }

    fn create_job(
        &self,
        path: &Path,
        source: PathBuf,
        frame: Option<FrameSelection>,
        acquisition: Option<Arc<crate::preload::Acquisition>>,
        mode: LoadMode,
        notifier: Option<Notifier>,
    ) -> Result<Arc<Job>, String> {
        #[cfg(windows)]
        let source_file = File::open(&source).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let metadata = source_file.metadata().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let metadata = fs::metadata(&source).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Choose a regular image file.".into());
        }
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !openable(path) {
            return Err(
                "Supported formats: JPEG, PNG, BMP, WebP, TIFF, BigTIFF, OME-TIFF, Nikon ND2 and Zeiss CZI."
                    .into(),
            );
        }
        let projection = frame
            .as_ref()
            .map(|frame| frame.projection)
            .unwrap_or_default();
        let format = acquisition.as_ref().map(|acquisition| acquisition.format());
        let selection = format.map(|_| {
            frame
                .as_ref()
                .map(|frame| frame.selection)
                .unwrap_or_default()
        });
        // BMP opens eagerly in the worker, including for vipsheader. Read only
        // its dimensions here, before permitting any full-frame allocations.
        let frame_memory = if extension == "bmp" {
            let (width, height) = image::ImageReader::with_format(
                std::io::BufReader::new(File::open(&source).map_err(|e| e.to_string())?),
                image::ImageFormat::Bmp,
            )
            .into_dimensions()
            .map_err(|e| e.to_string())?;
            if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
                return Err("Invalid or unsupported image dimensions.".into());
            }
            decoded_workspace(width, height, 4, 1)?
        } else {
            64 * 1024 * 1024
        };
        let id = format!(
            "{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let mut hasher = DefaultHasher::new();
        // Bump when display samples change: v6 keeps 16-bit channel planes
        // unless a generated pyramid would exceed half the cache limit.
        "aperture-display-v6-16bit-fallback-half-cache".hash(&mut hasher);
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
        #[cfg(windows)]
        match crate::platform::file_identity(&source_file) {
            Some(identity) => identity.hash(&mut hasher),
            // Unknown filesystem identity must never authorize old pixel reuse.
            // The existing per-open ID remains unique across process launches.
            None => id.hash(&mut hasher),
        }
        metadata
            .modified()
            .map_err(|e| e.to_string())?
            .hash(&mut hasher);
        if let (Some(format), Some(selection)) = (format, &selection) {
            format.cache_version().hash(&mut hasher);
            (selection.position, selection.z, selection.time).hash(&mut hasher);
            if projection != crate::frames::Projection::None {
                "max-original-v1".hash(&mut hasher);
                projection.hash(&mut hasher);
            }
        }
        let cache_key = format!("{:016x}", hasher.finish());
        let dir = self.root.join(format!("work-{id}"));
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        fs::create_dir(dir.join("tmp")).map_err(|e| e.to_string())?;
        let alias = dir.join(format!("source.{extension}"));
        // Loader options use brackets, so never pass a user filename to libvips.
        crate::platform::source_alias(&source, &alias).map_err(|e| e.to_string())?;
        let input = if selection.is_some() {
            dir.join("selected.tif")
        } else {
            alias
        };
        let (
            dimensions,
            initial_channels,
            sample_bits,
            rgb,
            skipped_series,
            projection_unavailable,
        ) = frame
            .map(|frame| {
                (
                    Some(frame.dimensions),
                    frame.channels,
                    frame.sample_bits,
                    frame.rgb,
                    frame.skipped_series,
                    frame.projection_unavailable,
                )
            })
            .unwrap_or((None, Vec::new(), 8, false, 0, None));
        let initial = ImageState {
            id,
            revision: 0,
            name: path
                .file_name()
                .unwrap_or(source.file_name().unwrap())
                .to_string_lossy()
                .into_owned(),
            path: std::path::absolute(path)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Image path is not UTF-8")?
                .to_owned(),
            width: 0,
            height: 0,
            orientation: 1,
            phase: Phase::Loading,
            preview_ready: false,
            tiles_ready: false,
            progress: None,
            error: None,
            detail: "Reading image metadata…".into(),
            channels: initial_channels,
            channel_mode: selection.is_some(),
            sample_bits,
            rgb,
            display_fallback: false,
            read_only: selection.is_some(),
            dimensions,
            calibration: None,
            display_ranges: Vec::new(),
            projection,
            skipped_series,
            projection_unavailable,
        };
        let job = Arc::new(Job {
            state: Mutex::new(initial.clone()),
            cancel: AtomicBool::new(false),
            cancelled: tokio::sync::Notify::new(),
            dir: Mutex::new(dir.clone()),
            input,
            source,
            format,
            selection,
            projection,
            acquisition,
            preview_only: matches!(mode, LoadMode::Preview),
            background: !matches!(mode, LoadMode::Foreground),
            memory: self.memory.clone(),
            annotations: Mutex::new(None),
            work: dir,
            native: Mutex::new(Vec::new()),
            planes: Mutex::new(Vec::new()),
            root: self.root.clone(),
            cache_key: Mutex::new(cache_key),
            ome: OnceLock::new(),
            process_number: AtomicU64::new(0),
            decoder: Mutex::new(None),
            decoder_memory: Mutex::new(None),
            display_memory: Mutex::new(None),
            frame_memory: AtomicU64::new(frame_memory),
            processes: Mutex::new(HashMap::new()),
            notifier,
            budget_check: Mutex::new(Instant::now()),
            housekeeping: AtomicBool::new(false),
            cache_access: self.cache_access.clone(),
        });
        Ok(job)
    }
}

/// Generated pyramids need no decoder once complete; native tiles do.
pub(crate) fn finish_decoder(job: &Job) {
    if job.native.lock().unwrap().is_empty() {
        if let Some(decoder) = job.decoder.lock().unwrap().take() {
            decoder.stop();
        }
        job.decoder_memory.lock().unwrap().take();
    }
}

impl Job {
    pub(crate) fn cache_key(&self) -> String {
        self.cache_key.lock().unwrap().clone()
    }

    /// Display caches of an OME-TIFF selection depend on every member file.
    fn set_member_identity(&self, identity: u64) {
        let mut key = self.cache_key.lock().unwrap();
        let mut hasher = DefaultHasher::new();
        (key.as_str(), identity).hash(&mut hasher);
        *key = format!("{:016x}", hasher.finish());
    }

    /// OME-TIFF planes are read in place from member files, except projections,
    /// which are written to the job's display TIFF like ND2/CZI frames.
    pub(crate) fn lazy_planes(&self) -> bool {
        self.format == Some(crate::frames::Format::OmeTiff)
            && self.projection == crate::frames::Projection::None
    }

    pub(crate) fn member_alias(&self, member: u32) -> PathBuf {
        self.work.join(format!("member-{member}.tif"))
    }

    /// The libvips source file of a display plane.
    pub(crate) fn plane_path(&self, plane: &Plane) -> PathBuf {
        if self.lazy_planes() {
            self.member_alias(plane.member)
        } else {
            self.input.clone()
        }
    }

    /// Re-checks OME-TIFF member files after evaluation and before publishing.
    pub(crate) fn verify_sources(&self) -> Result<(), String> {
        self.ome.get().map_or(Ok(()), |source| source.verify())
    }

    async fn tile_permit(
        &self,
        slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, (u16, String)> {
        // notify_waiters also wakes futures created before their first poll.
        let cancelled = self.cancelled.notified();
        if self.cancel.load(Ordering::Relaxed) {
            return Err((410, "Image replaced or cancelled.".into()));
        }
        let mut cancelled = std::pin::pin!(cancelled);
        let mut acquire = std::pin::pin!(slots.acquire_owned());
        std::future::poll_fn(|cx| {
            if cancelled.as_mut().poll(cx).is_ready() {
                Poll::Ready(Err((410, "Image replaced or cancelled.".into())))
            } else {
                acquire
                    .as_mut()
                    .poll(cx)
                    .map(|result| result.map_err(|_| (503, "Viewer shutting down.".into())))
            }
        })
        .await
    }

    pub(crate) fn cancel(&self) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            if self.cancel.swap(true, Ordering::Relaxed) {
                return;
            }
            state.phase = Phase::Cancelled;
            state.detail = "Loading cancelled.".into();
            state.progress = None;
            state.revision += 1;
            state.clone()
        };
        self.cancelled.notify_waiters();
        self.notify(&snapshot);
        let decoder = self.decoder.lock().unwrap().clone();
        if let Some(decoder) = decoder {
            decoder.stop();
        }
        self.decoder_memory.lock().unwrap().take();
        self.display_memory.lock().unwrap().take();
        for (_, process) in self.processes.lock().unwrap().drain() {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    /// Admit the foreground display's 16-bit allowance before publishing it.
    fn reserve_display(&self, sample_bits: u8, channels: usize) -> Result<(), String> {
        if self.background || sample_bits != 16 || self.display_memory.lock().unwrap().is_some() {
            return Ok(());
        }
        // Never hold the slot while waiting, so cancellation stays immediate.
        let reservation = self
            .memory
            .reserve(
                wide_display_bytes(channels as u64),
                false,
                Some(&self.cancel),
            )?
            .ok_or(DISPLAY_MEMORY_ERROR)?;
        let mut display = self.display_memory.lock().unwrap();
        if !self.cancel.load(Ordering::Relaxed) {
            *display = Some(reservation);
        }
        Ok(())
    }
    fn update(&self, action: impl FnOnce(&mut ImageState)) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            action(&mut state);
            state.revision += 1;
            state.clone()
        };
        self.notify(&snapshot);
    }
    fn report_progress(&self, fraction: f64) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            // Coarse overall progress keeps long multichannel conversions quiet.
            let fraction = (fraction * 20.0).floor() / 20.0;
            if self.cancel.load(Ordering::Relaxed)
                || state.phase != Phase::Loading
                || state.progress.is_some_and(|previous| fraction <= previous)
            {
                return;
            }
            state.progress = Some(fraction);
            state.revision += 1;
            state.clone()
        };
        self.notify(&snapshot);
    }
    fn notify(&self, state: &ImageState) {
        if let Some(notifier) = &self.notifier {
            notifier(state);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(decoder) = self.decoder.get_mut().unwrap().take() {
            decoder.stop();
        }
        let _ = fs::remove_dir_all(&self.work);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use assets::{read_asset, Asset};
    use process::run;

    pub(super) fn workspace_fixture() -> (PathBuf, Backend, Arc<Job>) {
        let root = std::env::temp_dir().join(format!(
            "aperture-decoder-memory-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");
        fs::write(&source, []).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        backend
            .memory
            .set_limit(crate::memory::MIN_LIMIT_BYTES)
            .unwrap();
        let context = LoadContext {
            root: backend.root.clone(),
            memory: backend.memory.clone(),
            cache_access: backend.cache_access.clone(),
        };
        let job = context
            .create_job(
                &source,
                source.clone(),
                None,
                None,
                LoadMode::Foreground,
                None,
            )
            .unwrap();
        (root, backend, job)
    }

    #[test]
    fn resume_after_shutdown_restarts_maintenance_and_opening() {
        let (root, backend, job) = workspace_fixture();
        backend.shutdown();
        assert!(backend.workers.lock().unwrap().is_empty());
        assert_eq!(
            backend.open(&job.source).unwrap_err(),
            "Viewer shutting down."
        );
        backend.resume();
        assert!(backend
            .workers
            .lock()
            .unwrap()
            .iter()
            .any(|worker| !worker.is_finished()));
        backend.open(&job.source).unwrap();
        backend.shutdown();
        assert!(backend.workers.lock().unwrap().is_empty());
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nd2_projection_caches_are_canonical_and_separate_from_source_planes() {
        use crate::frames::{Projection, Selection};
        let root = std::env::temp_dir().join(format!(
            "aperture-projection-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("stack.nd2");
        let bytes = crate::nd2::tests::synthetic(8, false);
        fs::write(&source, &bytes).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        wait_ready(&backend);
        let samples = |backend: &Backend| {
            let job = backend.current.lock().unwrap().clone().unwrap();
            read_asset(&job, Asset::ChannelPreview { channel: 0 }).unwrap()
        };
        let initial = samples(&backend);
        for (mode, z, time) in [
            (Projection::Z, 1, 0),
            (Projection::Time, 0, 1),
            (Projection::ZTime, 1, 1),
        ] {
            let id = backend.snapshot().unwrap().id;
            backend
                .select_view(
                    &id,
                    Selection {
                        position: 0,
                        z,
                        time,
                    },
                    Some(Projection::None),
                )
                .unwrap();
            wait_ready(&backend);
            let expected = samples(&backend);
            let id = backend.snapshot().unwrap().id;
            backend
                .select_view(&id, Selection::default(), Some(mode))
                .unwrap();
            let projected = wait_ready(&backend);
            assert_eq!(projected.projection, mode);
            assert_eq!(samples(&backend), expected);
            let key = backend
                .current
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .cache_key();
            backend
                .select_view(
                    &projected.id,
                    Selection {
                        position: 0,
                        z,
                        time,
                    },
                    None,
                )
                .unwrap();
            let repeated = wait_ready(&backend);
            assert_eq!(
                backend
                    .current
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .cache_key(),
                key
            );
            assert_eq!(repeated.dimensions.as_ref().unwrap().z_index, 0);
            assert_eq!(repeated.dimensions.as_ref().unwrap().time_index, 0);
            assert_eq!(samples(&backend), expected);
        }
        let id = backend.snapshot().unwrap().id;
        backend
            .select_view(&id, Selection::default(), Some(Projection::None))
            .unwrap();
        wait_ready(&backend);
        assert_eq!(samples(&backend), initial);
        assert_eq!(fs::read(&source).unwrap(), bytes);
        backend.shutdown();
        drop(backend);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nd2_16_bit_frames_display_original_samples_and_old_manifests_are_not_reused() {
        if std::process::Command::new("vips")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "aperture-nd2-16-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        let samples = |channel: u32| {
            let job = backend.current.lock().unwrap().clone().unwrap();
            let png = read_asset(&job, Asset::ChannelPreview { channel }).unwrap();
            let body = assets::u16_body(&png, |_, _| true).unwrap();
            body[8..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| u16::from_le_bytes(*sample))
                .collect::<Vec<_>>()
        };
        for (bits, rgb) in [(8, false), (16, false), (16, true)] {
            let source = root.join(format!("frame-{bits}-{rgb}.nd2"));
            fs::write(&source, crate::nd2::tests::synthetic(bits, rgb)).unwrap();
            backend.open(&source).unwrap();
            let state = wait_ready(&backend);
            assert!(state.channel_mode);
            assert_eq!(state.rgb, rgb);
            assert_eq!(state.channels.len(), if rgb { 3 } else { 2 });
            let sample_bits = if bits == 16 { 16 } else { 8 };
            assert_eq!(state.sample_bits, sample_bits);
            assert!(state
                .channels
                .iter()
                .all(|channel| channel.domain == crate::channels::domain(sample_bits)));
            if bits == 8 {
                continue;
            }
            // Low 16-bit values stay exact instead of being stretched to 0–255.
            if !rgb {
                assert_eq!(state.display_ranges, [[1, 3], [2, 4]]);
                assert_eq!((samples(0), samples(1)), (vec![1, 3], vec![2, 4]));
            }
            let id = state.id.clone();
            backend
                .select_plane(&id, crate::frames::Selection::default())
                .unwrap();
            let reused = wait_ready(&backend);
            assert!(reused.detail.contains("Reused"), "{reused:?}");
            assert_eq!((reused.sample_bits, reused.rgb), (16, rgb));
            // A manifest written before 16-bit display samples is never reused.
            let dir = backend
                .current
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .dir
                .lock()
                .unwrap()
                .clone();
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
            manifest.as_object_mut().unwrap().remove("sample_bits");
            fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
            backend
                .select_plane(&reused.id, crate::frames::Selection::default())
                .unwrap();
            let rebuilt = wait_ready(&backend);
            assert!(!rebuilt.detail.contains("Reused"), "{rebuilt:?}");
            assert_eq!(rebuilt.sample_bits, 16);
        }
        backend.shutdown();
        drop(backend);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resolved_source_stays_pinned_when_selected_symlink_is_retargeted() {
        let root = std::env::temp_dir().join(format!(
            "aperture-pinned-source-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let original = root.join("original.data");
        let replacement = root.join("replacement.data");
        let selected = root.join("selected alias.png");
        fs::write(&original, b"original source").unwrap();
        fs::write(&replacement, b"replacement source").unwrap();
        std::os::unix::fs::symlink(&original, &selected).unwrap();
        let resolved = crate::instances::path_key(&selected).unwrap();
        fs::remove_file(&selected).unwrap();
        std::os::unix::fs::symlink(&replacement, &selected).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        let state = backend.open_resolved(&selected, &resolved).unwrap();
        assert_eq!(state.name, "selected alias.png");
        assert_eq!(Path::new(&state.path), selected);
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(job.source, fs::canonicalize(&original).unwrap());
        assert_ne!(job.source, fs::canonicalize(&selected).unwrap());
        assert_eq!(fs::read(&job.source).unwrap(), b"original source");
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn folder_navigation_matches_open_formats_and_preserves_aliases() {
        let root = std::env::temp_dir().join(format!(
            "aperture-folder-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for name in [
            "z.JPG",
            "a.png",
            "A.PNG",
            "b.JPEG",
            "c.bmp",
            "d.tif",
            "e.TIFF",
            "f.webp",
            "notes.txt",
            "g.gif",
            "source.data",
        ] {
            fs::write(root.join(name), []).unwrap();
        }
        fs::create_dir(root.join("folder.png")).unwrap();
        std::os::unix::fs::symlink(root.join("source.data"), root.join("alias.png")).unwrap();
        std::os::unix::fs::symlink(root.join("missing.png"), root.join("broken.png")).unwrap();
        let paths = folder_images(&root.join("a.png")).unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|path| Path::new(path).file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "A.PNG",
                "a.png",
                "alias.png",
                "b.JPEG",
                "c.bmp",
                "d.tif",
                "e.TIFF",
                "f.webp",
                "z.JPG"
            ]
        );
        assert!(paths.iter().all(|path| Path::new(path).is_absolute()));
        assert_eq!(folder_images(&root.join("deleted.tif")).unwrap(), paths);
        let backend = Backend::new(root.join("cache")).unwrap();
        let state = backend.open(&root.join("alias.png")).unwrap();
        assert_eq!(state.path, root.join("alias.png").to_str().unwrap());
        backend.shutdown();
        let empty = root.join("empty");
        fs::create_dir(&empty).unwrap();
        assert!(folder_images(&empty.join("absent.tif")).unwrap().is_empty());
        assert!(folder_images(&root.join("missing/absent.tif")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_replacement_reaps_subprocess_and_stops_waiting_tiles() {
        let root = std::env::temp_dir().join(format!(
            "aperture-replace-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.tif");
        channel_fixture(&source, 1, 1, "");
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        wait_ready(&backend);
        let job = backend.current.lock().unwrap().clone().unwrap();
        job.state.lock().unwrap().phase = Phase::Loading;
        fs::remove_file(job.dir.lock().unwrap().join("channels/0/preview.png")).unwrap();
        let waiting = {
            let job = job.clone();
            thread::spawn(move || read_asset(&job, Asset::ChannelPreview { channel: 0 }))
        };
        let sleeper = {
            let job = job.clone();
            thread::spawn(move || run(&job, "sleep", &["30".into()]))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Some(child) = job.processes.lock().unwrap().values().next() {
                break child.lock().unwrap().id();
            }
            assert!(Instant::now() < deadline, "subprocess did not start");
            thread::sleep(Duration::from_millis(2));
        };
        let began = Instant::now();
        assert!(backend.open(&root.join("deleted.tif")).is_err());
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(job.processes.lock().unwrap().is_empty());
        assert!(sleeper.join().unwrap().unwrap_err().contains("cancelled"));
        assert_eq!(waiting.join().unwrap().unwrap_err().0, 410);
        assert_eq!(backend.snapshot().unwrap().phase, Phase::Cancelled);
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_tile_permits_cancel_before_and_after_first_poll() {
        let root = std::env::temp_dir().join(format!(
            "aperture-tile-queue-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.png");
        fs::write(&source, []).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        for poll_first in [false, true] {
            backend.open(&source).unwrap();
            let job = backend.current.lock().unwrap().clone().unwrap();
            tauri::async_runtime::block_on(async {
                // No permit can become available; cancellation must wake it.
                let slots = Arc::new(tokio::sync::Semaphore::new(0));
                let mut waiting = std::pin::pin!(job.tile_permit(slots));
                if poll_first {
                    std::future::poll_fn(|cx| {
                        assert!(waiting.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                }
                job.cancel();
                let result = tokio::time::timeout(Duration::from_millis(100), waiting)
                    .await
                    .expect("cancelled tile remained queued");
                assert_eq!(result.unwrap_err().0, 410);
            });
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    pub(crate) fn wait_ready(backend: &Backend) -> ImageState {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = backend.snapshot().unwrap();
            if state.phase != Phase::Loading {
                assert_eq!(state.phase, Phase::Ready, "{state:?}");
                return state;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Small generated TIFFs keep source data and validation caches out of Git.
    pub(super) fn channel_fixture(path: &Path, bands: u16, pages: u16, description: &str) {
        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        for page in 0..pages {
            let directory = bytes.len() as u32;
            let count = 12_u16;
            let payload = directory + 2 + count as u32 * 12 + 4;
            let description_offset = payload;
            let bits_offset = description_offset + description.len() as u32 + 1;
            let description_value = if description.len() < 4 {
                let mut inline = [0_u8; 4];
                inline[..description.len()].copy_from_slice(description.as_bytes());
                u32::from_le_bytes(inline)
            } else {
                description_offset
            };
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
                    if bands == 1 {
                        16
                    } else if bands == 2 {
                        16 | (16 << 16)
                    } else {
                        bits_offset
                    },
                ),
                (259, 3, 1, 1),
                (262, 3, 1, 1),
                (270, 2, description.len() as u32 + 1, description_value),
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
}
