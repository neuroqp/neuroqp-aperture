//! One acquisition owns reusable raw frames and one interruptible preload pass.

use crate::{
    backend::{self, Job, LoadContext},
    frame_cache::{Cache, Geometry},
    frames::{Dimensions, ExtractedFrame, Format, Projection, Selection},
    instances::Fingerprint,
    memory::{Budget, Reservation},
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, TryLockError, Weak,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloadStatus {
    pub cached_frames: u64,
    pub retained_bytes: u64,
    pub previews_ready: u64,
    pub frames_ready: u64,
    pub running: bool,
}

#[derive(Clone, Copy, Default)]
struct Hint {
    axis: usize,
    direction: i8,
}

pub struct Acquisition {
    format: Format,
    source: PathBuf,
    identity: Option<Fingerprint>,
    memory: Arc<Budget>,
    cache: Mutex<Option<Cache>>,
    generation: AtomicU64,
    active: Mutex<Option<Weak<Job>>>,
    hint: Mutex<Hint>,
    stopped: AtomicBool,
    trim_requested: AtomicBool,
    clear_requested: AtomicBool,
    cached_frames: AtomicU64,
    retained_bytes: AtomicU64,
    previews_ready: AtomicU64,
    frames_ready: AtomicU64,
    running: AtomicU64,
}

impl Acquisition {
    pub fn new(
        format: Format,
        source: PathBuf,
        identity: Option<Fingerprint>,
        memory: Arc<Budget>,
    ) -> Arc<Self> {
        Arc::new(Self {
            format,
            source,
            identity,
            memory,
            cache: Mutex::new(None),
            generation: AtomicU64::new(0),
            active: Mutex::new(None),
            hint: Mutex::new(Hint {
                axis: 1,
                direction: 1,
            }),
            stopped: AtomicBool::new(false),
            trim_requested: AtomicBool::new(false),
            clear_requested: AtomicBool::new(false),
            cached_frames: AtomicU64::new(0),
            retained_bytes: AtomicU64::new(0),
            previews_ready: AtomicU64::new(0),
            frames_ready: AtomicU64::new(0),
            running: AtomicU64::new(0),
        })
    }

    pub fn matches(&self, format: Format, source: &Path, identity: &Option<Fingerprint>) -> bool {
        self.format == format
            && self.source == source
            && self.identity.is_some()
            && &self.identity == identity
    }

    /// Never wait for the raw-cache lock on the window thread. At most the Rust
    /// frame currently being decoded runs to completion before foreground work.
    pub fn foreground(&self) {
        let previous = {
            let mut active = self.active.lock().unwrap();
            self.generation.fetch_add(1, Ordering::SeqCst);
            active.take().and_then(|job| job.upgrade())
        };
        if let Some(previous) = previous {
            previous.cancel();
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.foreground();
        self.clear_optional();
    }

    pub fn hint(&self, axis: Option<&str>, direction: Option<i8>) {
        let mut hint = self.hint.lock().unwrap();
        if let Some(axis) = axis {
            hint.axis = match axis {
                "position" => 0,
                "z" => 1,
                "time" => 2,
                _ => return,
            };
        }
        if let Some(direction) = direction.filter(|direction| *direction != 0) {
            hint.direction = direction.signum();
        }
    }

    pub fn status(&self) -> PreloadStatus {
        PreloadStatus {
            cached_frames: self.cached_frames.load(Ordering::Relaxed),
            retained_bytes: self.retained_bytes.load(Ordering::Relaxed),
            previews_ready: self.previews_ready.load(Ordering::Relaxed),
            frames_ready: self.frames_ready.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Relaxed) != 0,
        }
    }

    fn snapshot_cache(&self, cache: &Cache) {
        self.cached_frames
            .store(cache.cached_count() as u64, Ordering::Relaxed);
        self.retained_bytes
            .store(cache.retained_bytes(), Ordering::Relaxed);
    }

    pub fn trim(&self) -> Result<(), String> {
        self.trim_requested.store(true, Ordering::Relaxed);
        match self.cache.try_lock() {
            Ok(mut cache) => self.trim_cache(&mut cache),
            Err(TryLockError::WouldBlock) => Ok(()),
            Err(TryLockError::Poisoned(_)) => Err("Frame cache lock failed.".into()),
        }
    }

    pub fn clear_optional(&self) {
        self.clear_requested.store(true, Ordering::Relaxed);
        if let Ok(mut cache) = self.cache.try_lock() {
            let _ = self.trim_cache(&mut cache);
        }
    }

    pub fn reserve_workspace(
        &self,
        bytes: u64,
        background: bool,
    ) -> Result<Option<Reservation>, String> {
        let mut guard = self.cache()?;
        let cache = guard.as_mut().unwrap();
        let result = cache.reserve(bytes, background);
        self.snapshot_cache(cache);
        self.trim_cache(&mut guard)?;
        result
    }

    pub fn geometry(&self, selection: Selection) -> Result<Geometry, String> {
        self.cache()?.as_ref().unwrap().geometry(selection)
    }

    fn trim_cache(&self, cache: &mut Option<Cache>) -> Result<(), String> {
        self.trim_requested.store(false, Ordering::Relaxed);
        let clear = self.clear_requested.swap(false, Ordering::Relaxed);
        if let Some(cache) = cache {
            if clear {
                cache.clear();
            }
            cache.trim()?;
            self.snapshot_cache(cache);
        }
        Ok(())
    }

    fn cache(&self) -> Result<MutexGuard<'_, Option<Cache>>, String> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Loading cancelled.".into());
        }
        let mut cache = self.cache.lock().map_err(|_| "Frame cache lock failed.")?;
        if self.stopped.load(Ordering::SeqCst) {
            self.trim_cache(&mut cache)?;
            return Err("Loading cancelled.".into());
        }
        if cache.is_none() {
            *cache = Some(Cache::new(
                self.format,
                self.source.clone(),
                self.memory.clone(),
            )?);
        }
        if self.trim_requested.load(Ordering::Relaxed)
            || self.clear_requested.load(Ordering::Relaxed)
        {
            self.trim_cache(&mut cache)?;
        }
        Ok(cache)
    }

    pub fn extract_display(
        &self,
        output: &Path,
        selection: Selection,
        background: bool,
        cancel: &AtomicBool,
    ) -> Result<ExtractedFrame, String> {
        let mut guard = self.cache()?;
        if cancel.load(Ordering::Relaxed) || self.stopped.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let cache = guard.as_mut().unwrap();
        let result = cache.extract_display(output, selection, background, Some(cancel));
        self.snapshot_cache(cache);
        if self.trim_requested.load(Ordering::Relaxed)
            || self.clear_requested.load(Ordering::Relaxed)
        {
            self.trim_cache(&mut guard)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        result
    }

    pub fn extract_projection(
        &self,
        output: &Path,
        selection: Selection,
        projection: Projection,
        cancel: &AtomicBool,
        progress: impl FnMut(f64),
    ) -> Result<ExtractedFrame, String> {
        let mut guard = self.cache()?;
        if cancel.load(Ordering::Relaxed) || self.stopped.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let cache = guard.as_mut().unwrap();
        let result = cache.extract_projection(output, selection, projection, cancel, progress);
        self.snapshot_cache(cache);
        if self.trim_requested.load(Ordering::Relaxed)
            || self.clear_requested.load(Ordering::Relaxed)
        {
            self.trim_cache(&mut guard)?;
        }
        if cancel.load(Ordering::Relaxed) || self.stopped.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        result
    }

    fn current(&self, generation: u64, foreground: &Job) -> bool {
        !self.stopped.load(Ordering::Relaxed)
            && !foreground.cancel.load(Ordering::Relaxed)
            && self.generation.load(Ordering::SeqCst) == generation
    }

    fn available(&self, generation: u64, foreground: &Job) -> bool {
        while self.current(generation, foreground) {
            let Ok(status) = self.memory.status() else {
                return false;
            };
            if !status.preloading_paused {
                return true;
            }
            let _ = self.trim();
            // Recheck pressure and cancellation without occupying a decoder or
            // holding the cache lock. A user action interrupts within 100 ms.
            if !status.pressure {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

/// Iterate coordinates without materializing a potentially enormous acquisition.
/// A fixed set of nearby coordinates comes first, in the navigation direction.
struct Candidates {
    sizes: [u32; 3],
    origin: Selection,
    nearby: Vec<Selection>,
    near_index: usize,
    offset: u128,
    total: u128,
    start: u128,
}

impl Candidates {
    fn new(dimensions: &Dimensions, origin: Selection, hint: Hint) -> Self {
        let sizes = [
            dimensions.positions.len() as u32,
            dimensions.z,
            dimensions.time,
        ];
        let mut nearby = Vec::with_capacity(8);
        let coordinates = [origin.position, origin.z, origin.time];
        for distance in 1..=4i64 {
            for direction in [i64::from(hint.direction), -i64::from(hint.direction)] {
                let next = i64::from(coordinates[hint.axis]) + distance * direction;
                if next >= 0 && next < i64::from(sizes[hint.axis]) {
                    let mut next_coordinates = coordinates;
                    next_coordinates[hint.axis] = next as u32;
                    nearby.push(Selection {
                        position: next_coordinates[0],
                        z: next_coordinates[1],
                        time: next_coordinates[2],
                    });
                }
            }
        }
        let total = sizes.iter().map(|size| u128::from(*size)).product();
        let start = (u128::from(origin.position) * u128::from(sizes[2]) + u128::from(origin.time))
            * u128::from(sizes[1])
            + u128::from(origin.z);
        Self {
            sizes,
            origin,
            nearby,
            near_index: 0,
            offset: 1,
            total,
            start,
        }
    }
}

impl Iterator for Candidates {
    type Item = Selection;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(selection) = self.nearby.get(self.near_index) {
            self.near_index += 1;
            return Some(*selection);
        }
        while self.offset < self.total {
            let index = (self.start + self.offset) % self.total;
            self.offset += 1;
            let z = index % u128::from(self.sizes[1]);
            let rest = index / u128::from(self.sizes[1]);
            let selection = Selection {
                position: (rest / u128::from(self.sizes[2])) as u32,
                time: (rest % u128::from(self.sizes[2])) as u32,
                z: z as u32,
            };
            if selection != self.origin && !self.nearby.contains(&selection) {
                return Some(selection);
            }
        }
        None
    }
}

struct Running<'a> {
    acquisition: &'a Acquisition,
    token: u64,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.acquisition.running.compare_exchange(
            self.token,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// Runs after foreground loading, in that loader's existing owned thread.
/// Every optional job has a separate work directory and no UI notifier.
pub(crate) fn run(context: &LoadContext, foreground: &Arc<Job>) {
    if foreground.projection != Projection::None {
        return;
    }
    let Some(acquisition) = &foreground.acquisition else {
        return;
    };
    let state = foreground.state.lock().unwrap().clone();
    let Some(dimensions) = state.dimensions else {
        return;
    };
    let selected = Selection {
        position: dimensions.position,
        z: dimensions.z_index,
        time: dimensions.time_index,
    };
    let generation = acquisition.generation.load(Ordering::SeqCst);
    if !acquisition.current(generation, foreground) {
        return;
    }
    let token = generation.wrapping_add(1).max(1);
    acquisition.running.store(token, Ordering::Relaxed);
    let _running = Running { acquisition, token };
    let hint = *acquisition.hint.lock().unwrap();
    acquisition.previews_ready.fetch_max(1, Ordering::Relaxed);
    acquisition.frames_ready.fetch_max(1, Ordering::Relaxed);
    for preview_only in [true, false] {
        let mut ready = 1;
        for selection in Candidates::new(&dimensions, selected, hint) {
            if !acquisition.available(generation, foreground)
                || backend::tree_size(&context.root) >= backend::CACHE_LIMIT / 2
            {
                return;
            }
            let Ok(mut guard) = acquisition.cache() else {
                return;
            };
            if !acquisition.current(generation, foreground) {
                return;
            }
            let cache = guard.as_mut().unwrap();
            let Ok(metadata) = cache.describe(selection) else {
                continue;
            };
            // Preload admission preserves existing raw frames when memory is full.
            // Display generation can evict optional raw frames as needed.
            if preview_only {
                cache.preload(selection);
            }
            acquisition.snapshot_cache(cache);
            if acquisition.trim_requested.load(Ordering::Relaxed)
                || acquisition.clear_requested.load(Ordering::Relaxed)
            {
                let _ = acquisition.trim_cache(&mut guard);
            }
            drop(guard);
            if !acquisition.current(generation, foreground) {
                return;
            }
            let Ok(job) = context.background_job(
                Path::new(&state.path),
                foreground.source.clone(),
                (selection, metadata.dimensions, metadata.channels),
                acquisition.clone(),
                preview_only,
            ) else {
                return;
            };
            {
                let mut active = acquisition.active.lock().unwrap();
                if !acquisition.current(generation, foreground) {
                    return;
                }
                *active = Some(Arc::downgrade(&job));
            }
            let result = backend::load(&job);
            backend::finish_decoder(&job);
            // Even a native pyramid must release its workers after preloading.
            job.cancel();
            {
                let mut active = acquisition.active.lock().unwrap();
                if active
                    .as_ref()
                    .is_some_and(|active| active.ptr_eq(&Arc::downgrade(&job)))
                {
                    active.take();
                }
            }
            if result.is_err() || !acquisition.current(generation, foreground) {
                return;
            }
            ready += 1;
            let count = if preview_only {
                &acquisition.previews_ready
            } else {
                &acquisition.frames_ready
            };
            // Counts are conservative completed-pass maxima until a pass reaches
            // every frame. No coordinate-sized status allocation is retained.
            count.fetch_max(ready, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn maintenance_and_stop_defer_without_waiting_for_a_decode_lock() {
        let directory = crate::frames::tests::temporary();
        let acquisition = Acquisition::new(
            Format::Nd2,
            directory.join("unopened.nd2"),
            None,
            Budget::new(directory.clone()).unwrap(),
        );
        let decode = acquisition.cache.lock().unwrap();
        acquisition.trim().unwrap();
        acquisition.clear_optional();
        acquisition.stop();
        assert!(acquisition.trim_requested.load(Ordering::Relaxed));
        assert!(acquisition.clear_requested.load(Ordering::Relaxed));
        drop(decode);
        acquisition.trim().unwrap();
        assert!(!acquisition.trim_requested.load(Ordering::Relaxed));
        assert!(!acquisition.clear_requested.load(Ordering::Relaxed));
        assert!(acquisition.geometry(Selection::default()).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn aliases_with_another_format_do_not_reuse_the_acquisition() {
        let directory = crate::frames::tests::temporary();
        let source = directory.join("source");
        std::fs::write(&source, b"bytes").unwrap();
        let identity = crate::instances::fingerprint(&source).unwrap();
        let acquisition = Acquisition::new(
            Format::Nd2,
            source.clone(),
            identity.clone(),
            Budget::new(directory.clone()).unwrap(),
        );
        assert!(acquisition.matches(Format::Nd2, &source, &identity));
        assert!(!acquisition.matches(Format::Czi, &source, &identity));
        acquisition.stop();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn candidates_prioritize_direction_and_cover_each_other_coordinate_once() {
        let dimensions = Dimensions {
            positions: vec!["a".into(), "b".into()],
            z: 6,
            time: 3,
            position: 1,
            z_index: 2,
            time_index: 1,
        };
        let selected = Selection {
            position: 1,
            z: 2,
            time: 1,
        };
        let candidates: Vec<_> = Candidates::new(
            &dimensions,
            selected,
            Hint {
                axis: 1,
                direction: -1,
            },
        )
        .collect();
        assert_eq!(candidates[0], Selection { z: 1, ..selected });
        assert_eq!(candidates[1], Selection { z: 3, ..selected });
        assert_eq!(candidates.len(), 35);
        assert_eq!(candidates.iter().copied().collect::<HashSet<_>>().len(), 35);
        assert!(!candidates.contains(&selected));
        let huge = Dimensions {
            positions: vec!["a".into()],
            z: u32::MAX,
            time: u32::MAX,
            ..dimensions
        };
        assert_eq!(
            Candidates::new(
                &huge,
                Selection::default(),
                Hint {
                    axis: 2,
                    direction: 1
                }
            )
            .take(20)
            .count(),
            20
        );
    }
}
