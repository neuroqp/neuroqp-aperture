use crate::{
    channels::{self, Channel, Plane},
    native::{self, NativeLevel},
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::process::Command;
use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, HashMap},
    ffi::OsString,
    fs::{self, File},
    future::Future,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp", "nd2", "czi",
];
const TILE_SIZE: u32 = 256;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

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
    pub phase: String,
    pub preview_ready: bool,
    pub tiles_ready: bool,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub detail: String,
    pub channels: Vec<Channel>,
    pub channel_mode: bool,
    pub read_only: bool,
    pub dimensions: Option<crate::frames::Dimensions>,
    pub calibration: Option<crate::frames::Calibration>,
    pub display_ranges: Vec<[u16; 2]>,
    pub projection: crate::frames::Projection,
}

type FrameSelection = (
    crate::frames::Selection,
    crate::frames::Dimensions,
    Vec<Channel>,
    crate::frames::Projection,
);

type Notifier = Arc<dyn Fn(&ImageState) + Send + Sync>;

pub struct Job {
    pub state: Mutex<ImageState>,
    pub cancel: AtomicBool,
    pub cancelled: tokio::sync::Notify,
    pub dir: Mutex<PathBuf>,
    pub input: PathBuf,
    pub source: PathBuf,
    selection: Option<crate::frames::Selection>,
    pub(crate) projection: crate::frames::Projection,
    pub(crate) acquisition: Option<Arc<crate::preload::Acquisition>>,
    pub(crate) preview_only: bool,
    pub(crate) background: bool,
    memory: Arc<crate::memory::Budget>,
    /// Initial metadata remains the sourceIndex basis through repeated saves.
    pub annotations: Mutex<Option<crate::annotation_io::AnnotationBasis>>,
    work: PathBuf,
    pub native: Mutex<Vec<NativeLevel>>,
    planes: Mutex<Vec<Plane>>,
    root: PathBuf,
    cache_key: String,
    process_number: AtomicU64,
    decoder: Mutex<Option<Arc<crate::decoder::Decoder>>>,
    decoder_memory: Mutex<Option<DecoderMemory>>,
    frame_memory: AtomicU64,
    processes: Mutex<HashMap<u64, Arc<Mutex<Child>>>>,
    notifier: Option<Notifier>,
    budget_check: Mutex<Instant>,
    housekeeping: AtomicBool,
    cache_access: Arc<Mutex<()>>,
}

struct DecoderMemory {
    bytes: u64,
    // Additional admission after metadata discovery keeps the initial lease alive.
    reservations: Vec<crate::memory::Reservation>,
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
    pub requests: AtomicU64,
    pub closing: Arc<AtomicBool>,
    pub tile_slots: Arc<tokio::sync::Semaphore>,
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
            Some((selection, dimensions, state.channels, projection)),
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
                // Two 32-channel overview sets during replacement and a 32 MiB
                // tile-cache allowance. Browser/runtime overhead and GPU differ.
                let amount = 96 * 1024 * 1024;
                let mut reservation = self.memory.try_reserve(amount, false)?;
                if reservation.is_none() {
                    self.memory.request_reclaim()?;
                    for _ in 0..80 {
                        thread::sleep(Duration::from_millis(25));
                        reservation = self.memory.try_reserve(amount, false)?;
                        if reservation.is_some() {
                            break;
                        }
                    }
                }
                *browser = Some(reservation.ok_or("Image display does not fit the shared memory budget. Close another image window or increase the limit in Settings.")?);
            }
        }
        let acquisition = if let Some(format) = crate::frames::Format::from_path(path) {
            let identity =
                crate::instances::fingerprint(&source).map_err(|error| error.to_string())?;
            let mut current = self.acquisition.lock().unwrap();
            if current
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
                    state.phase = "error".into();
                    state.error = Some(error);
                    state.detail = "Image loading failed.".into();
                });
            }
            finish_decoder(&job);
            if !job.cancel.load(Ordering::Relaxed) && job.state.lock().unwrap().phase == "ready" {
                crate::preload::run(&context, &job);
            }
            if job.cancel.load(Ordering::Relaxed) || job.state.lock().unwrap().phase == "error" {
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
        frame: (
            crate::frames::Selection,
            crate::frames::Dimensions,
            Vec<Channel>,
        ),
        acquisition: Arc<crate::preload::Acquisition>,
        preview_only: bool,
    ) -> Result<Arc<Job>, String> {
        self.create_job(
            path,
            source,
            Some((frame.0, frame.1, frame.2, crate::frames::Projection::None)),
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
        if !supported_image(path) {
            return Err(
                "Supported formats: JPEG, PNG, BMP, WebP, TIFF, BigTIFF, Nikon ND2 and Zeiss CZI."
                    .into(),
            );
        }
        let projection = frame.as_ref().map(|frame| frame.3).unwrap_or_default();
        let format = crate::frames::Format::from_path(path);
        let selection = format.map(|_| frame.as_ref().map(|frame| frame.0).unwrap_or_default());
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
        "aperture-display-v5-area-preview".hash(&mut hasher);
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
        let (dimensions, initial_channels) = frame
            .map(|(_, dimensions, channels, _)| (Some(dimensions), channels))
            .unwrap_or_default();
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
            phase: "loading".into(),
            preview_ready: false,
            tiles_ready: false,
            progress: None,
            error: None,
            detail: "Reading image metadata…".into(),
            channels: initial_channels,
            channel_mode: selection.is_some(),
            read_only: selection.is_some(),
            dimensions,
            calibration: None,
            display_ranges: Vec::new(),
            projection,
        };
        let job = Arc::new(Job {
            state: Mutex::new(initial.clone()),
            cancel: AtomicBool::new(false),
            cancelled: tokio::sync::Notify::new(),
            dir: Mutex::new(dir.clone()),
            input,
            source,
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
            cache_key,
            process_number: AtomicU64::new(0),
            decoder: Mutex::new(None),
            decoder_memory: Mutex::new(None),
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
    pub async fn tile_permit(
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
            state.phase = "cancelled".into();
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
        for (_, process) in self.processes.lock().unwrap().drain() {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
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
                || state.phase != "loading"
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

#[derive(Serialize, Deserialize)]
struct CacheManifest {
    version: u32,
    source_key: String,
    tools: u64,
    width: u32,
    height: u32,
    orientation: u8,
    channels: Vec<Channel>,
    channel_mode: bool,
    planes: Vec<Plane>,
    native: Vec<NativeLevel>,
    #[serde(default)]
    dimensions: Option<crate::frames::Dimensions>,
    #[serde(default)]
    calibration: Option<crate::frames::Calibration>,
    #[serde(default)]
    display_ranges: Vec<[u16; 2]>,
    #[serde(default)]
    projection: crate::frames::Projection,
}

// AppImage readers live under a different mount path after each launch. The
// containing package identifies that complete reader payload without scanning it.
#[cfg(target_os = "linux")]
fn appimage_tool_identity(executable: &Path, appdir: &Path, appimage: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    if !appdir.is_absolute()
        || !appdir.is_dir()
        || !executable.starts_with(appdir)
        || !appimage.is_absolute()
    {
        return None;
    }
    let image = fs::canonicalize(appimage).ok()?;
    let metadata = fs::metadata(&image).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    "aperture-appimage-readers-v1".hash(&mut hasher);
    image.hash(&mut hasher);
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
        .hash(&mut hasher);
    Some(hasher.finish())
}

// Metadata-only identity keeps a cache made with different decoder tools from
// assuming that the installed reader still supports its native TIFF layout.
fn tool_identity() -> u64 {
    let executable = std::env::current_exe().ok();
    #[cfg(target_os = "linux")]
    if let (Some(executable), Some(appdir), Some(appimage)) = (
        executable.as_deref(),
        std::env::var_os("APPDIR"),
        std::env::var_os("APPIMAGE"),
    ) {
        if let Some(identity) =
            appimage_tool_identity(executable, Path::new(&appdir), Path::new(&appimage))
        {
            return identity;
        }
    }
    let mut hasher = DefaultHasher::new();
    // The application contains the Rust BMP reader and native worker. Bundled
    // DLLs can also change while CLI filenames remain unchanged across updates.
    if let Some(executable) = executable {
        let mut readers = vec![executable.clone()];
        if cfg!(windows) {
            readers.extend(
                [
                    "libvips-42.dll",
                    "libtiff-6.dll",
                    "libglib-2.0-0.dll",
                    "libgobject-2.0-0.dll",
                ]
                .map(|name| executable.with_file_name(name)),
            );
        }
        for reader in readers {
            reader.hash(&mut hasher);
            if let Ok(metadata) = fs::metadata(reader) {
                metadata.len().hash(&mut hasher);
                metadata.modified().ok().hash(&mut hasher);
            }
        }
    }
    for program in ["vips", "vipsheader", "tiffinfo"] {
        program.hash(&mut hasher);
        let bundled = PathBuf::from(crate::platform::tool(program).get_program());
        #[cfg(windows)]
        let filename = format!("{program}.exe");
        #[cfg(not(windows))]
        let filename = program.to_owned();
        let search_path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(bundled)
            .chain(std::env::split_paths(&search_path).map(|directory| directory.join(&filename)));
        for path in paths {
            if let Ok(metadata) = fs::metadata(&path) {
                if metadata.is_file() {
                    path.hash(&mut hasher);
                    metadata.len().hash(&mut hasher);
                    metadata.modified().ok().hash(&mut hasher);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.ctime().hash(&mut hasher);
                        metadata.ctime_nsec().hash(&mut hasher);
                    }
                    break;
                }
            }
        }
    }
    hasher.finish()
}

fn write_manifest(job: &Job, dir: &Path) -> Result<(), String> {
    let state = job.state.lock().unwrap().clone();
    let manifest = CacheManifest {
        version: 1,
        source_key: job.cache_key.clone(),
        tools: tool_identity(),
        width: state.width,
        height: state.height,
        orientation: state.orientation,
        channels: state.channels,
        channel_mode: state.channel_mode,
        planes: job.planes.lock().unwrap().clone(),
        native: job.native.lock().unwrap().clone(),
        dimensions: state.dimensions,
        calibration: state.calibration,
        display_ranges: state.display_ranges,
        projection: state.projection,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
    fs::write(dir.join("manifest.json"), bytes).map_err(|error| error.to_string())
}

fn cached_manifest(job: &Job, dir: &Path) -> Option<CacheManifest> {
    let complete = dir.join("complete").is_file();
    if !complete && (job.selection.is_none() || !dir.join("previews").is_file()) {
        return None;
    }
    let file = File::open(dir.join("manifest.json")).ok()?;
    if file.metadata().ok()?.len() > 65536 {
        return None;
    }
    let manifest: CacheManifest = serde_json::from_reader(file.take(65536)).ok()?;
    if manifest.version != 1
        || manifest.source_key != job.cache_key
        || manifest.tools != tool_identity()
        || manifest.projection != job.projection
        || !(1..=i32::MAX as u32).contains(&manifest.width)
        || !(1..=i32::MAX as u32).contains(&manifest.height)
        || !(1..=8).contains(&manifest.orientation)
        || manifest.channels.is_empty()
        || manifest.channels.len() > channels::MAX_CHANNELS as usize
        || manifest.planes.len() != manifest.channels.len()
        || manifest
            .planes
            .iter()
            .any(|plane| plane.page >= 256 || plane.band >= channels::MAX_CHANNELS)
        || manifest.native.len() > 29
        || (job.selection.is_some() != manifest.dimensions.is_some())
        || manifest.dimensions.as_ref().is_some_and(|dimensions| {
            dimensions.positions.is_empty()
                || dimensions.position as usize >= dimensions.positions.len()
                || dimensions.z_index >= dimensions.z
                || dimensions.time_index >= dimensions.time
                || job.selection.is_none_or(|selected| {
                    selected.position != dimensions.position
                        || selected.z != dimensions.z_index
                        || selected.time != dimensions.time_index
                })
        })
        || (!manifest.native.is_empty()
            && !native::valid_cached_levels(&manifest.native, manifest.width, manifest.height))
        || (manifest.channel_mode
            && manifest.planes.iter().any(|plane| plane.page != 0)
            && !manifest.native.is_empty())
    {
        return None;
    }
    let valid = if complete {
        valid_cache_assets(
            dir,
            manifest.channels.len(),
            manifest.channel_mode,
            !manifest.native.is_empty(),
        )
    } else {
        manifest.channel_mode
            && manifest.native.is_empty()
            && (0..manifest.channels.len()).all(|i| {
                valid_preview(
                    &dir.join("channels").join(i.to_string()).join("preview.png"),
                    true,
                )
            })
    };
    if !valid {
        return None;
    }
    Some(manifest)
}

fn valid_cache_assets(dir: &Path, channels: usize, channel_mode: bool, native: bool) -> bool {
    dir.join("complete").is_file()
        && if channel_mode {
            (0..channels).all(|index| {
                let channel = dir.join("channels").join(index.to_string());
                valid_preview(&channel.join("preview.png"), true)
                    && (native || channel.join("tiles_files").is_dir())
            })
        } else {
            valid_preview(&dir.join("preview.jpg"), false)
                && (native || dir.join("tiles_files").is_dir())
        }
}

fn valid_preview(path: &Path, png: bool) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !(16..=16 * 1024 * 1024).contains(&metadata.len()) {
        return false;
    }
    let mut first = [0; 8];
    let mut last = [0; 12];
    if file.read_exact(&mut first).is_err()
        || file.seek(SeekFrom::End(-12)).is_err()
        || file.read_exact(&mut last).is_err()
    {
        return false;
    }
    if png {
        first == *b"\x89PNG\r\n\x1a\n" && last == *b"\0\0\0\0IEND\xaeB`\x82"
    } else {
        first.starts_with(&[0xff, 0xd8]) && last.ends_with(&[0xff, 0xd9])
    }
}

fn housekeeping(job: &Job) -> Result<(), String> {
    let _access = job.cache_access.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let active = job.dir.lock().unwrap().clone();
    if !job.background {
        prune_except(&job.root, CACHE_LIMIT / 2, Some(&active))?;
    }
    job.housekeeping.store(true, Ordering::Relaxed);
    if tree_size(&job.root) > CACHE_LIMIT {
        return Err("Display cache limit reached.".into());
    }
    Ok(())
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
                "ready"
            } else {
                "loading"
            }
            .into();
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
    let (width, height) = validate_header(&header)?;
    if field(&header, "vips-loader") != Some("tiffload") {
        let bands = field(&header, "bands").unwrap().parse::<u64>().unwrap();
        let sample_bytes = if field(&header, "format") == Some("ushort") {
            2
        } else {
            1
        };
        job.frame_memory.fetch_max(
            decoded_workspace(width, height, bands, sample_bytes)?,
            Ordering::Relaxed,
        );
    }
    let tiffinfo = if field(&header, "vips-loader") == Some("tiffload") {
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
                state.phase = "ready".into();
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
        return load_channels(job, width, height, pixel == "ushort");
    }
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
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
    // Large, ordinary gray/RGB8 strips use full-coverage area reduction. Other
    // layouts retain libvips, and full-resolution detail always uses libvips.
    let raw_preview = uses_worker(job)
        && ((pixel == "ushort" && field(&header, "bands") == Some("1"))
            || (pixel == "uchar"
                && (field(&header, "bands") == Some("1")
                    || (field(&header, "bands") == Some("3")
                        && field(&header, "interpretation") == Some("srgb")))))
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
        let jpeg = if uses_worker(job)
            && pixel == "uchar"
            && field(&header, "bands") == Some("3")
            && field(&header, "interpretation") == Some("srgb")
        {
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
        state.phase = "ready".into();
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
        // Windows cannot move a directory while a descendant file is open.
        // Keep path selection and each short read inside the promotion guard.
        let access = job.cache_access.lock().unwrap();
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
                    drop(access);
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
        if let Ok(bytes) = fs::read(&path) {
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.ends_with(b"IEND\xaeB`\x82") {
                return Ok(bytes);
            }
        }
        drop(access);
        if *job.dir.lock().unwrap() != dir {
            continue;
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
    let access = job.cache_access.lock().unwrap();
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
    drop(access);
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
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let _access = job.cache_access.lock().unwrap();
        let tiles = job
            .dir
            .lock()
            .unwrap()
            .join("channels")
            .join(channel.to_string())
            .join("native_tiles");
        fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
        let destination = tiles.join(format!("{level}-{x}-{y}.png"));
        fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
        fs::read(&destination).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(resized);
    let _ = fs::remove_file(partial);
    result
}

// Before first pixels, count only this bounded preview workspace; prune and
// count the persisted cache once the preview is visible. Short read-only metadata
// commands then avoid repeatedly traversing thousands of cached display tiles.
fn budget_exceeded(job: &Job, finished: bool) -> bool {
    if !job.housekeeping.load(Ordering::Relaxed) {
        if finished || job.budget_check.lock().unwrap().elapsed() >= Duration::from_millis(500) {
            *job.budget_check.lock().unwrap() = Instant::now();
            return tree_size(&job.work) > CACHE_LIMIT / 2;
        }
        return false;
    }
    let mut last = job.budget_check.lock().unwrap();
    if !finished && last.elapsed() < Duration::from_millis(500) {
        return false;
    }
    *last = Instant::now();
    tree_size(&job.root) > CACHE_LIMIT
}

// Each decode lives in a killable process. libvips' operation cache is bounded;
// codec memory is not a hard RSS limit and should be measured on representative files.
pub(crate) fn run(job: &Job, program: &str, args: &[OsString]) -> Result<String, String> {
    run_with_progress(job, program, args, None)
}

fn run_with_progress(
    job: &Job,
    program: &str,
    args: &[OsString],
    progress: Option<(f64, f64)>,
) -> Result<String, String> {
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    if uses_worker(job) && matches!(program, "vips" | "vipsheader") {
        let args = args
            .iter()
            .map(|arg| {
                arg.to_str()
                    .map(String::from)
                    .ok_or("Image decoder path is not UTF-8")
            })
            .collect::<Result<Vec<_>, _>>()?;
        return worker_call(
            job,
            &crate::vips_worker::Request::Command {
                program: program.into(),
                args,
            },
            progress,
        );
    }
    // Direct tools have the same admission rules as the worker. Keep this lease
    // local until the child has exited or has been killed and reaped, including
    // when Job::cancel drains the process table from another thread.
    let bytes = if matches!(program, "vips" | "vipsheader") {
        decoder_workspace(job)?
    } else {
        // Metadata tools do not need a second image workspace beside a live worker.
        8 * 1024 * 1024
    };
    let _memory = reserve_workspace(job, bytes)?;
    let dir = job.work.clone();
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let stdout_path = dir.join(format!("stdout-{number}"));
    let stderr_path = dir.join(format!("stderr-{number}"));
    let stdout = File::create(&stdout_path).map_err(|e| e.to_string())?;
    let stderr = File::create(&stderr_path).map_err(|e| e.to_string())?;
    let mut command = crate::platform::tool(program);
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
    let process = {
        // Serialize spawning with cancellation so replacement cannot leave a
        // decoder process starting after its job has been cancelled.
        let mut processes = job.processes.lock().unwrap();
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let child = command.stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr)).spawn().map_err(|e| format!("Cannot run {program}: {e}. Install libvips tools (and libtiff tools for existing page pyramids)."))?;
        let process = Arc::new(Mutex::new(child));
        processes.insert(number, process.clone());
        process
    };
    let status = loop {
        if job.cancel.load(Ordering::Relaxed) {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            job.processes.lock().unwrap().remove(&number);
            let _ = fs::remove_file(stdout_path);
            let _ = fs::remove_file(stderr_path);
            return Err("Loading cancelled.".into());
        }
        let status = process.lock().unwrap().try_wait();
        match status {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(error) => {
                let mut child = process.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
                drop(child);
                job.processes.lock().unwrap().remove(&number);
                return Err(error.to_string());
            }
        }
        if budget_exceeded(job, false) {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            job.processes.lock().unwrap().remove(&number);
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err("The 2 GiB display cache limit was reached. This image needs more temporary disk space than this proof of concept allows.".into());
        }
        thread::sleep(Duration::from_millis(2));
    };
    job.processes.lock().unwrap().remove(&number);
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
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
    if budget_exceeded(job, program == "vips") {
        return Err("Display cache limit reached.".into());
    }
    if status.success() {
        Ok(output)
    } else {
        Err("Could not read this image. It may be damaged or use an unsupported format.".into())
    }
}

fn uses_worker(job: &Job) -> bool {
    #[cfg(test)]
    if std::env::var_os("APERTURE_WORKER_EXECUTABLE").is_none() {
        return false;
    }
    cfg!(windows)
        || matches!(
            job.input.extension().and_then(|s| s.to_str()),
            Some("tif" | "tiff" | "bmp")
        )
}

fn decoded_workspace(
    width: u32,
    height: u32,
    bands: u64,
    sample_bytes: u64,
) -> Result<u64, String> {
    // Full-image codecs can retain source pixels alongside decoder/output copies.
    // Four copies also cover BMP's two retained loader variants and copy peak.
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|bytes| bytes.checked_mul(bands))
        .and_then(|bytes| bytes.checked_mul(sample_bytes))
        .and_then(|bytes| bytes.checked_mul(4))
        .ok_or_else(|| "Image decoder workspace is too large.".into())
}

fn decoder_workspace(job: &Job) -> Result<u64, String> {
    let state = job.state.lock().unwrap();
    let preview = state.channels.len().max(1) as u64 * 1024 * 1024 * 4;
    let (overhead, frame) = if job.selection.is_some() {
        // Frame display TIFFs have one whole-image strip; preserve its full decode.
        (
            128 * 1024 * 1024,
            decoded_workspace(state.width, state.height, state.channels.len() as u64, 1)?,
        )
    } else {
        // A bounded metadata/streaming allowance, plus geometry for codecs that
        // can materialize a complete frame. Small images fit the minimum budget.
        (
            64 * 1024 * 1024,
            job.frame_memory
                .load(Ordering::Relaxed)
                .max(64 * 1024 * 1024),
        )
    };
    frame
        .checked_add(overhead)
        .and_then(|bytes| bytes.checked_add(preview))
        .ok_or_else(|| "Image decoder workspace is too large.".into())
}

fn reserve_workspace(job: &Job, bytes: u64) -> Result<crate::memory::Reservation, String> {
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let reservation = if let Some(acquisition) = &job.acquisition {
        acquisition.reserve_workspace(bytes, job.background)?
    } else {
        let mut reservation = job.memory.try_reserve(bytes, job.background)?;
        if reservation.is_none() && !job.background && bytes <= job.memory.status()?.limit_bytes {
            job.memory.request_reclaim()?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while reservation.is_none() && Instant::now() < deadline {
                if job.cancel.load(Ordering::Relaxed) {
                    return Err("Loading cancelled.".into());
                }
                thread::sleep(Duration::from_millis(25));
                reservation = job.memory.try_reserve(bytes, false)?;
            }
        }
        reservation
    };
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    reservation.ok_or_else(|| "Image decoding does not fit the memory limit. Increase it in Settings or close another image window.".into())
}

fn worker_call(
    job: &Job,
    request: &crate::vips_worker::Request,
    progress: Option<(f64, f64)>,
) -> Result<String, String> {
    let exceeded = std::cell::Cell::new(false);
    let call = |decoder: &crate::decoder::Decoder| {
        decoder.call(
            request,
            &job.cancel,
            || {
                exceeded.set(budget_exceeded(job, false));
                exceeded.get()
            },
            |fraction| {
                if let Some((start, end)) = progress {
                    // Rough work percentage: preview 20%, pyramid 79%, final writes 1%.
                    job.report_progress(start + (end - start) * fraction);
                }
            },
        )
    };
    let decoder = worker(job)?;
    let mut output = call(&decoder);
    // A crashed decoder fails its in-flight call; retry it once with a fresh
    // decoder. Cancellation, cache-limit stops and failed loads are not retried.
    if output.is_err()
        && decoder.stopped()
        && !exceeded.get()
        && !job.cancel.load(Ordering::Relaxed)
        && job.state.lock().unwrap().phase != "error"
    {
        output = call(&*worker(job)?);
    }
    let output = output?;
    if budget_exceeded(job, true) {
        return Err("Display cache limit reached.".into());
    }
    Ok(output)
}

fn worker(job: &Job) -> Result<Arc<crate::decoder::Decoder>, String> {
    let mut slot = job.decoder.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let amount = decoder_workspace(job)?;
    let mut memory = job.decoder_memory.lock().unwrap();
    let previous = memory.as_ref().map_or(0, |memory| memory.bytes);
    if amount > previous {
        let reservation = reserve_workspace(job, amount - previous)?;
        let memory = memory.get_or_insert_with(|| DecoderMemory {
            bytes: 0,
            reservations: Vec::new(),
        });
        memory.bytes = amount;
        memory.reservations.push(reservation);
    }
    // Replace a decoder that exited while idle or stopped after a failed call.
    if slot.as_ref().is_none_or(|decoder| decoder.stopped()) {
        *slot = match crate::decoder::Decoder::start(&job.work) {
            Ok(decoder) => Some(Arc::new(decoder)),
            Err(error) => {
                memory.take();
                return Err(error);
            }
        };
    }
    Ok(slot.as_ref().unwrap().clone())
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
        // Windows cannot move a directory while a descendant file is open.
        // Keep path selection and each short read inside the promotion guard.
        let access = job.cache_access.lock().unwrap();
        let dir = job.dir.lock().unwrap().clone();
        let path = match asset {
            Asset::Preview => dir.join("preview.jpg"),
            Asset::ChannelPreview { .. } | Asset::ChannelTile { .. } => unreachable!(),
            Asset::Tile { level, x, y } => {
                if !job.native.lock().unwrap().is_empty() {
                    if state.phase == "error" {
                        return Err((500, state.error.unwrap_or_default()));
                    }
                    drop(access);
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
            // dzsave writes each tile directly; only expose complete JPEG streams.
            if bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) {
                return Ok(bytes);
            }
        }
        drop(access);
        if *job.dir.lock().unwrap() != dir {
            continue;
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
    let access = job.cache_access.lock().unwrap();
    let dir = job.dir.lock().unwrap().clone();
    let tiles = dir.join("native_tiles");
    fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
    let destination = tiles.join(format!("{level}-{x}-{y}.jpg"));
    if let Ok(bytes) = fs::read(&destination) {
        return Ok(bytes);
    }
    drop(access);
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
    let _access = job.cache_access.lock().unwrap();
    let tiles = job.dir.lock().unwrap().join("native_tiles");
    fs::create_dir_all(&tiles).map_err(|e| e.to_string())?;
    let destination = tiles.join(format!("{level}-{x}-{y}.jpg"));
    fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
    fs::read(destination).map_err(|e| e.to_string())
}

pub(crate) fn tree_size(path: &Path) -> u64 {
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

#[cfg(test)]
fn prune(root: &Path, limit: u64) -> Result<(), String> {
    prune_except(root, limit, None)
}

fn prune_except(root: &Path, limit: u64, protected: Option<&Path>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|entry| {
            protected != Some(entry.path().as_path())
                && (entry.path().join("complete").is_file()
                    || entry.path().join("previews").is_file())
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

    fn workspace_fixture() -> (PathBuf, Backend, Arc<Job>) {
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
    fn decoder_workspace_accounts_for_rgba_ushort_and_eager_bmp() {
        assert_eq!(decoded_workspace(8000, 8000, 4, 2).unwrap(), 2_048_000_000);
        assert_eq!(decoded_workspace(8000, 8000, 3, 1).unwrap(), 768_000_000);
        assert!(decoded_workspace(u32::MAX, u32::MAX, 4, 2).is_err());
        let (root, backend, job) = workspace_fixture();
        // The browser allowance plus a small image workspace fits 256 MiB.
        assert!(
            96 * 1024 * 1024 + decoder_workspace(&job).unwrap() <= crate::memory::MIN_LIMIT_BYTES
        );
        let bmp = root.join("source.bmp");
        image::RgbaImage::new(16, 12).save(&bmp).unwrap();
        let context = LoadContext {
            root: backend.root.clone(),
            memory: backend.memory.clone(),
            cache_access: backend.cache_access.clone(),
        };
        let bmp_job = context
            .create_job(&bmp, bmp.clone(), None, None, LoadMode::Foreground, None)
            .unwrap();
        assert_eq!(
            bmp_job.frame_memory.load(Ordering::Relaxed),
            16 * 12 * 4 * 4
        );
        backend.shutdown();
        drop((job, bmp_job, backend));
        fs::remove_dir_all(root).unwrap();
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
    fn ordinary_foreground_workspace_reclaims_another_windows_optional_memory() {
        let (root, backend, job) = workspace_fixture();
        let peer = crate::memory::Budget::new(root.join("cache")).unwrap();
        let optional = peer.try_reserve(200 * 1024 * 1024, false).unwrap().unwrap();
        let epoch = peer.reclaim_epoch().unwrap();
        let maintenance = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while peer.reclaim_epoch().unwrap() == epoch {
                assert!(
                    Instant::now() < deadline,
                    "Foreground admission did not request reclaim"
                );
                thread::sleep(Duration::from_millis(2));
            }
            drop(optional);
        });
        let bytes = decoder_workspace(&job).unwrap();
        let reservation = reserve_workspace(&job, bytes).unwrap();
        maintenance.join().unwrap();
        assert_eq!(backend.memory.status().unwrap().used_bytes, bytes);
        drop(reservation);
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_decoder_refuses_oversized_geometry_before_spawning() {
        let (root, backend, job) = workspace_fixture();
        job.frame_memory.store(
            decoded_workspace(8000, 8000, 4, 2).unwrap(),
            Ordering::Relaxed,
        );
        let error = run(&job, "vipsheader", &["-a".into(), input(&job, "")]).unwrap_err();
        assert!(error.contains("does not fit the memory limit"), "{error}");
        assert!(job.processes.lock().unwrap().is_empty());
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_admission_cancels_before_spawn_and_releases_after_reaping() {
        let (root, backend, job) = workspace_fixture();
        let full = backend
            .memory
            .try_reserve(crate::memory::MIN_LIMIT_BYTES, false)
            .unwrap()
            .unwrap();
        let epoch = backend.memory.reclaim_epoch().unwrap();
        let pending = {
            let job = job.clone();
            thread::spawn(move || run(&job, "sleep", &["0".into()]))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while backend.memory.reclaim_epoch().unwrap() == epoch {
            assert!(
                Instant::now() < deadline,
                "Direct command did not wait for admission"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(job.processes.lock().unwrap().is_empty());
        let cancelled = Instant::now();
        job.cancel();
        assert!(pending.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(cancelled.elapsed() < Duration::from_secs(1));
        drop(full);
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);

        job.cancel.store(false, Ordering::Relaxed);
        run(&job, "sleep", &["0".into()]).unwrap();
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        let running = {
            let job = job.clone();
            thread::spawn(move || run(&job, "sleep", &["30".into()]))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.processes.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "Admitted child did not spawn");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(backend.memory.status().unwrap().used_bytes, 8 * 1024 * 1024);
        job.cancel();
        assert!(running.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(job.processes.lock().unwrap().is_empty());
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
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
                .cache_key
                .clone();
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
                backend.current.lock().unwrap().as_ref().unwrap().cache_key,
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

    #[cfg(windows)]
    #[test]
    fn windows_directory_promotion_waits_for_open_descendant_to_close() {
        let root = std::env::temp_dir().join(format!(
            "aperture-directory-promotion-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let tiles = root.join("tiles_files");
        fs::create_dir_all(&tiles).unwrap();
        let tile = tiles.join("tile.jpg");
        fs::write(&tile, b"tile").unwrap();
        let reader = File::open(&tile).unwrap();
        let published = root.join("published_tiles");
        assert_eq!(
            fs::rename(&tiles, &published).unwrap_err().raw_os_error(),
            Some(5)
        );
        drop(reader);
        fs::rename(&tiles, &published).unwrap();
        assert_eq!(fs::read(published.join("tile.jpg")).unwrap(), b"tile");
        fs::remove_dir_all(root).unwrap();
    }

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
        job.state.lock().unwrap().phase = "ready".into();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_reader_identity_survives_mount_changes_and_detects_package_changes() {
        let root = std::env::temp_dir().join(format!(
            "aperture-package-identity-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let image = root.join("Aperture.AppImage");
        fs::write(&image, b"aperture vips-v1").unwrap();
        let mounts = [root.join(".mount_first"), root.join(".mount_second")];
        let identity = |mount: &Path| {
            appimage_tool_identity(&mount.join("usr/bin/aperture"), mount, &image).unwrap()
        };
        for mount in &mounts {
            fs::create_dir_all(mount.join("usr/bin")).unwrap();
            fs::write(mount.join("usr/bin/aperture"), b"same application").unwrap();
            fs::write(mount.join("usr/bin/vips"), b"same bundled reader").unwrap();
        }
        let original = identity(&mounts[0]);
        assert_eq!(
            original,
            identity(&mounts[1]),
            "Mount locations and reader ctimes must not invalidate a cache"
        );
        assert!(appimage_tool_identity(&root.join("development/aperture"), &mounts[0], &image).is_none(), "Inherited AppImage environment must leave ordinary binaries on their normal reader identity");
        let metadata = fs::metadata(&image).unwrap();
        let modified = metadata.modified().unwrap();
        let replacement = root.join("replacement.AppImage");
        fs::write(&replacement, b"aperture vips-v2").unwrap();
        fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        fs::rename(replacement, &image).unwrap();
        assert_eq!(fs::metadata(&image).unwrap().len(), metadata.len());
        assert_eq!(fs::metadata(&image).unwrap().modified().unwrap(), modified);
        let changed = identity(&mounts[0]);
        assert_ne!(
            original, changed,
            "Changed bundled readers must invalidate old caches even with preserved size and mtime"
        );
        assert_eq!(changed, identity(&mounts[1]));
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
        job.state.lock().unwrap().phase = "loading".into();
        fs::remove_file(job.dir.lock().unwrap().join("preview.jpg")).unwrap();
        let waiting = {
            let job = job.clone();
            thread::spawn(move || read_asset(&job, Asset::Preview))
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
        assert_eq!(backend.snapshot().unwrap().phase, "cancelled");
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

    fn wait_ready(backend: &Backend) -> ImageState {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = backend.snapshot().unwrap();
            if state.phase != "loading" {
                assert_eq!(state.phase, "ready", "{state:?}");
                return state;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cache_assets_require_only_the_previews_used_by_each_mode() {
        let root = std::env::temp_dir().join(format!(
            "aperture-cache-previews-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("tiles_files")).unwrap();
        fs::write(root.join("complete"), []).unwrap();
        let jpeg = [0xff, 0xd8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xd9];
        assert!(!valid_cache_assets(&root, 1, false, false));
        assert!(!valid_cache_assets(&root, 1, false, true));
        fs::write(root.join("preview.jpg"), jpeg).unwrap();
        assert!(valid_cache_assets(&root, 1, false, false));
        assert!(valid_cache_assets(&root, 1, false, true));
        fs::remove_file(root.join("preview.jpg")).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82";
        for channel in 0..2 {
            let dir = root.join("channels").join(channel.to_string());
            fs::create_dir_all(dir.join("tiles_files")).unwrap();
            fs::write(dir.join("preview.png"), png).unwrap();
        }
        assert!(valid_cache_assets(&root, 2, true, false));
        assert!(valid_cache_assets(&root, 2, true, true));
        // Older channel caches can retain an unused JPEG, even a corrupt one.
        fs::write(root.join("preview.jpg"), b"unused legacy preview").unwrap();
        assert!(valid_cache_assets(&root, 2, true, false));
        fs::remove_dir(root.join("channels/1/tiles_files")).unwrap();
        assert!(!valid_cache_assets(&root, 2, true, false));
        assert!(valid_cache_assets(&root, 2, true, true));
        fs::write(root.join("channels/1/preview.png"), b"broken preview").unwrap();
        assert!(!valid_cache_assets(&root, 2, true, true));
        fs::remove_file(root.join("channels/1/preview.png")).unwrap();
        assert!(!valid_cache_assets(&root, 2, true, true));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn early_cache_requires_valid_manifest_preview_and_source_fingerprint() {
        let root = std::env::temp_dir().join(format!(
            "aperture-early-cache-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.tif");
        channel_fixture(&source, 1, 1, "");
        let mut backend = Backend::new(root.join("cache")).unwrap();
        let events = Arc::new(Mutex::new(Vec::<ImageState>::new()));
        let recorded = events.clone();
        let callback_job = Arc::new(Mutex::new(None::<std::sync::Weak<Job>>));
        let target = callback_job.clone();
        backend.set_notifier(move |state| {
            if let Some(job) = target
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
            {
                let _snapshot = job.state.lock().unwrap().clone();
            }
            recorded.lock().unwrap().push(state.clone());
        });
        backend.open(&source).unwrap();
        let first = wait_ready(&backend);
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
        let manifest = fs::read(dir.join("manifest.json")).unwrap();
        let preview_metadata = fs::metadata(dir.join("preview.jpg")).unwrap();
        fs::remove_file(dir.join("manifest.json")).unwrap();
        backend.open(&source).unwrap();
        assert!(wait_ready(&backend).detail.contains("Reused"));
        assert_eq!(
            fs::metadata(dir.join("preview.jpg"))
                .unwrap()
                .modified()
                .unwrap(),
            preview_metadata.modified().unwrap()
        );
        assert!(dir.join("manifest.json").is_file());
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(
            job.process_number.load(Ordering::Relaxed),
            if cfg!(windows) {
                0
            } else if uses_worker(&job) {
                1
            } else {
                2
            },
            "legacy cache migration starts only the CLI metadata readers (Windows omits tiffinfo)"
        );
        for corrupt in [b"{invalid".to_vec(), vec![b'x'; 65537]] {
            fs::write(dir.join("manifest.json"), corrupt).unwrap();
            backend.open(&source).unwrap();
            assert!(!wait_ready(&backend).detail.contains("Reused"));
        }
        fs::write(dir.join("manifest.json"), &manifest).unwrap();
        let reopened = backend.open(&source).unwrap();
        let ready = wait_ready(&backend);
        assert!(ready.detail.contains("Reused"));
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(
            job.process_number.load(Ordering::Relaxed),
            0,
            "cache hit must not spawn readers"
        );
        assert!(
            job.decoder.lock().unwrap().is_none(),
            "cache hit must not start the native worker"
        );
        // Reenter this same job's state mutex from its callback.
        *callback_job.lock().unwrap() = Some(Arc::downgrade(&job));
        job.update(|state| state.detail = "Reused cache notification check".into());
        fs::write(dir.join("preview.jpg"), b"broken preview").unwrap();
        backend.open(&source).unwrap();
        assert!(!wait_ready(&backend).detail.contains("Reused"));
        // Same path with a changed source cannot reuse its previous descriptor.
        channel_fixture(&source, 1, 1, "changed metadata");
        backend.open(&source).unwrap();
        assert!(!wait_ready(&backend).detail.contains("Reused"));
        backend.cancel();
        let events = events.lock().unwrap();
        let initial_events: Vec<_> = events.iter().filter(|state| state.id == first.id).collect();
        assert!(initial_events
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision));
        assert!(
            initial_events
                .iter()
                .position(|state| state.preview_ready)
                .unwrap()
                < initial_events
                    .iter()
                    .position(|state| state.phase == "ready")
                    .unwrap()
        );
        assert!(events
            .iter()
            .any(|state| state.id == reopened.id && state.phase == "ready"));
        assert_eq!(events.last().unwrap().phase, "cancelled");
        drop(events);
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deferred_housekeeping_pins_live_cache_and_preview_work_remains_bounded() {
        let root = std::env::temp_dir().join(format!(
            "aperture-budget-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let active = root.join("active");
        let stale = root.join("stale");
        for dir in [&active, &stale] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("complete"), b"ok").unwrap();
            fs::write(dir.join("preview.jpg"), b"preview").unwrap();
        }
        prune_except(&root, 0, Some(&active)).unwrap();
        assert!(active.join("preview.jpg").is_file());
        assert!(!stale.exists());
        let backend = Backend::new(root.join("cache")).unwrap();
        let source = root.join("source.tif");
        channel_fixture(&source, 1, 1, "");
        backend.open(&source).unwrap();
        wait_ready(&backend);
        let job = backend.current.lock().unwrap().clone().unwrap();
        // A replaced worker waiting on cache housekeeping must recheck cancel
        // while holding the same guard used by the next warm-cache selection.
        let guard = backend.cache_access.lock().unwrap();
        let replaced = job.clone();
        let old_housekeeping = thread::spawn(move || housekeeping(&replaced));
        backend.open(&source).unwrap();
        assert_eq!(backend.snapshot().unwrap().phase, "loading");
        drop(guard);
        assert!(old_housekeeping
            .join()
            .unwrap()
            .unwrap_err()
            .contains("cancelled"));
        assert!(wait_ready(&backend).detail.contains("Reused"));
        let oversized = File::create(job.work.join("oversized")).unwrap();
        oversized.set_len(CACHE_LIMIT / 2 + 1).unwrap();
        job.housekeeping.store(false, Ordering::Relaxed);
        assert!(budget_exceeded(&job, true));
        fs::remove_file(job.work.join("oversized")).unwrap();
        let oversized = File::create(job.dir.lock().unwrap().join("oversized")).unwrap();
        oversized.set_len(CACHE_LIMIT + 1).unwrap();
        job.housekeeping.store(true, Ordering::Relaxed);
        assert!(budget_exceeded(&job, true));
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    // Small generated TIFFs keep source data and validation caches out of Git.
    fn channel_fixture(path: &Path, bands: u16, pages: u16, description: &str) {
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
            let id = job.state.lock().unwrap().id.clone();
            assert!(
                !job.dir.lock().unwrap().join("preview.jpg").exists(),
                "channel previews must not encode an unused composite JPEG"
            );
            while !events
                .lock()
                .unwrap()
                .iter()
                .any(|state| state.id == id && state.phase == "ready")
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            let loading: Vec<_> = events
                .lock()
                .unwrap()
                .iter()
                .filter(|state| state.id == id && state.phase == "loading")
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
                if state.phase != "loading" {
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
