//! Original acquisition frames retained under the shared process memory budget.

use crate::frames::{self, ExtractedFrame, Format, Projection, RawFrame, Selection};
use crate::memory::{Budget, Reservation};
use crate::nd2;
use openreadout_core::source::{ByteSource, Input, LocalFile};
use openreadout_core::{ChannelInfo, FormatReader, ImageInfo};
use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};

pub(crate) const MIB: u64 = 1024 * 1024;
pub(crate) const FRAME_OVERHEAD: u64 = 64 * 1024;

/// Format-specific bounds checked before the pinned reader allocates.
enum Plan {
    Nd2(nd2::Plan),
}

impl Plan {
    fn new(
        format: Format,
        source: &CheckedSource,
        budget: &Arc<Budget>,
    ) -> Result<(Self, Reservation), String> {
        match format {
            Format::Nd2 => {
                nd2::Plan::new(source, budget).map(|(plan, lease)| (Self::Nd2(plan), lease))
            }
        }
    }

    fn reader(&self) -> &'static dyn FormatReader {
        match self {
            Self::Nd2(_) => &openreadout_nd2::Nd2Reader,
        }
    }

    fn check_images(&self, images: &[ImageInfo]) -> Result<(), String> {
        match self {
            Self::Nd2(plan) => plan.check_images(images),
        }
    }

    /// Includes opening the reader, encoded data, codec workspace and assembly.
    fn decode_peak(&self, frame_bytes: u64) -> u64 {
        match self {
            Self::Nd2(plan) => plan.decode_peak(frame_bytes),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// Includes opening the reader, encoded data, codec workspace and assembly.
    pub decode_peak_bytes: u64,
}

struct Frame {
    raw: RawFrame,
    _reservation: Reservation,
}

pub struct Cache {
    format: Format,
    source: Arc<CheckedSource>,
    input: Input,
    budget: Arc<Budget>,
    images: Vec<ImageInfo>,
    plan: Plan,
    _metadata: Reservation,
    // ponytail: linear LRU lookup; frame buffers dominate these small entries.
    frames: VecDeque<(Selection, Frame)>,
}

impl Cache {
    pub fn new(format: Format, source: PathBuf, budget: Arc<Budget>) -> Result<Self, String> {
        let source = Arc::new(CheckedSource::open(source)?);
        let (plan, mut metadata) = Plan::new(format, &source, &budget)?;
        source.check().map_err(|e| e.to_string())?;
        let input = Input::from_source(&source.path, source.clone());
        let dataset = plan
            .reader()
            .open_input(&input)
            .map_err(|e| format!("{format}: {e}"))?;
        let full_images = dataset.info().map_err(|e| format!("{format}: {e}"))?.images;
        plan.check_images(&full_images)?;
        let images = compact_images(full_images);
        source.check().map_err(|e| e.to_string())?;
        // Readers can retain split channel data internally; never retain one.
        drop(dataset);
        frames::description(format, &images, Selection::default())?;
        metadata.shrink(summary_bytes(&images, images.capacity()))?;
        Ok(Self {
            format,
            source,
            input,
            budget,
            images,
            plan,
            _metadata: metadata,
            frames: VecDeque::new(),
        })
    }

    pub fn describe(&self, selection: Selection) -> Result<ExtractedFrame, String> {
        frames::description(self.format, &self.images, selection)
    }

    pub fn geometry(&self, selection: Selection) -> Result<Geometry, String> {
        let metadata = self.describe(selection)?;
        let image = &self.images[selection.position as usize];
        let bands = metadata.channels.len() as u32;
        let bytes = image.pixel_type.bytes_per_sample();
        let frame_bytes =
            u64::from(image.size_x) * u64::from(image.size_y) * u64::from(bands) * bytes as u64;
        Ok(Geometry {
            width: image.size_x,
            height: image.size_y,
            decode_peak_bytes: self.plan.decode_peak(frame_bytes),
        })
    }

    pub fn contains(&self, selection: Selection) -> bool {
        self.frames.iter().any(|(key, _)| *key == selection)
    }

    pub fn cached_count(&self) -> usize {
        self.frames.len()
    }

    pub fn retained_bytes(&self) -> u64 {
        self.frames
            .iter()
            .map(|(_, frame)| frame.raw.pixels.len() as u64)
            .sum()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    fn check_source(&mut self) -> Result<(), String> {
        if let Err(error) = self.source.check() {
            self.clear();
            return Err(error.to_string());
        }
        Ok(())
    }

    pub fn trim(&mut self) -> Result<(), String> {
        self.check_source()?;
        let status = self.budget.status()?;
        if status.pressure {
            self.clear();
        } else {
            while self.budget.status()?.used_bytes > status.limit_bytes {
                if self.frames.pop_front().is_none() {
                    break;
                }
            }
        }
        Ok(())
    }

    fn reserve(&mut self, bytes: u64, background: bool) -> Result<Option<Reservation>, String> {
        loop {
            if let Some(reservation) = self.budget.try_reserve(bytes, background)? {
                return Ok(Some(reservation));
            }
            if self.frames.pop_front().is_none() {
                break;
            }
        }
        reserve_after_reclaim(&self.budget, bytes, background)
    }

    pub fn reserve_external(
        &mut self,
        bytes: u64,
        background: bool,
    ) -> Result<Option<Reservation>, String> {
        self.reserve(bytes, background)
    }

    fn load(
        &mut self,
        selection: Selection,
        background: bool,
        evict: bool,
    ) -> Result<Option<Frame>, String> {
        self.check_source()?;
        let geometry = self.geometry(selection)?;
        let reserved = if evict {
            self.reserve(geometry.decode_peak_bytes, background)?
        } else {
            self.budget
                .try_reserve(geometry.decode_peak_bytes, background)?
        };
        let Some(mut reservation) = reserved else {
            return Ok(None);
        };
        let decoded = (|| {
            let mut dataset = self
                .plan
                .reader()
                .open_input(&self.input)
                .map_err(|e| format!("{}: {e}", self.format))?;
            frames::decode_frame(self.format, dataset.as_mut(), &self.images, selection)
        })();
        self.check_source()?;
        let raw = decoded?;
        reservation.shrink(retained_frame_bytes(&raw))?;
        Ok(Some(Frame {
            raw,
            _reservation: reservation,
        }))
    }

    pub fn preload(&mut self, selection: Selection) -> bool {
        if self.trim().is_err() || self.budget.status().map_or(true, |s| s.preloading_paused) {
            return false;
        }
        if self.contains(selection) {
            return true;
        }
        match self.load(selection, true, false) {
            Ok(Some(frame)) => {
                self.frames.push_back((selection, frame));
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn extract_display(
        &mut self,
        output: &Path,
        selection: Selection,
    ) -> Result<ExtractedFrame, String> {
        self.extract_display_with_priority(output, selection, false)
    }

    pub fn extract_display_with_priority(
        &mut self,
        output: &Path,
        selection: Selection,
        background: bool,
    ) -> Result<ExtractedFrame, String> {
        self.extract_display_cancellable(output, selection, background, None)
    }

    fn extract_display_cancellable(
        &mut self,
        output: &Path,
        selection: Selection,
        background: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<ExtractedFrame, String> {
        frames::check_cancel(cancel)?;
        self.trim()?;
        let frame = if let Some(index) = self.frames.iter().position(|(key, _)| *key == selection) {
            self.frames.remove(index).unwrap().1
        } else {
            self.load(selection, background, true)?.ok_or_else(|| format!("{} selected frame decoding does not fit the application memory budget. Increase the limit in Settings.", self.format))?
        };
        // Keep the raw frame's lease while normalizing its separate display copy.
        let display_bytes = if frame.raw.bytes == 2 {
            frame.raw.pixels.len() as u64 / 2
        } else {
            0
        };
        let result = match self.reserve(display_bytes + FRAME_OVERHEAD + metadata_bytes(&frame.raw.metadata), background)? {
            Some(_display) => frames::write_frame_cancellable(&frame.raw, output, true, cancel),
            None => Err(format!("{} display conversion does not fit the application memory budget. Increase the limit in Settings.", self.format)),
        };
        if let Err(error) = self
            .check_source()
            .and_then(|_| frames::check_cancel(cancel))
        {
            if result.is_ok() {
                let _ = std::fs::remove_file(output);
            }
            return Err(error);
        }
        self.frames.push_back((selection, frame));
        result
    }

    pub fn extract_projection(
        &mut self,
        output: &Path,
        selection: Selection,
        projection: Projection,
        cancel: &AtomicBool,
        mut progress: impl FnMut(f64),
    ) -> Result<ExtractedFrame, String> {
        frames::check_cancel(Some(cancel))?;
        progress(0.0);
        if projection == Projection::None {
            let result =
                self.extract_display_cancellable(output, selection, false, Some(cancel))?;
            progress(0.6);
            if let Err(error) = self
                .check_source()
                .and_then(|_| frames::check_cancel(Some(cancel)))
            {
                let _ = std::fs::remove_file(output);
                return Err(error);
            }
            return Ok(result);
        }
        self.trim()?;
        self.describe(selection)?;
        let selection = projection.canonical(selection);
        let metadata = self.describe(selection)?;
        let image = &self.images[selection.position as usize];
        let width = image.size_x;
        let height = image.size_y;
        let bands = metadata.channels.len() as u32;
        let bytes = image.pixel_type.bytes_per_sample();
        let rgb = image.samples_per_pixel == 3;
        let z_count = if projection.projects_z() {
            image.size_z
        } else {
            1
        };
        let time_count = if projection.projects_time() {
            image.size_t
        } else {
            1
        };
        let total = u64::from(z_count) * u64::from(time_count);
        let frame_bytes = u64::from(width) * u64::from(height) * u64::from(bands) * bytes as u64;
        let _accumulator = self.reserve(frame_bytes + FRAME_OVERHEAD + metadata_bytes(&metadata), false)?
            .ok_or_else(|| format!("{} projection accumulator does not fit the application memory budget. Increase the limit in Settings.", self.format))?;
        frames::check_cancel(Some(cancel))?;
        let mut accumulator = RawFrame {
            width,
            height,
            bands,
            bytes,
            rgb,
            pixels: vec![0; frame_bytes as usize],
            metadata,
        };
        let mut completed = 0u64;
        for time in 0..time_count {
            for z in 0..z_count {
                frames::check_cancel(Some(cancel))?;
                self.trim()?;
                let selected = Selection {
                    z: if projection.projects_z() {
                        z
                    } else {
                        selection.z
                    },
                    time: if projection.projects_time() {
                        time
                    } else {
                        selection.time
                    },
                    ..selection
                };
                let index = self.frames.iter().position(|(key, _)| *key == selected);
                let frame = if let Some(index) = index {
                    self.frames.remove(index).unwrap().1
                } else {
                    self.load(selected, false, true)?.ok_or_else(|| format!("{} projection frame decoding does not fit the application memory budget. Increase the limit in Settings.", self.format))?
                };
                // Read cached pixels in place. Newly decoded frames are temporary:
                // retaining a whole projection's inputs would churn the useful LRU.
                let reduced =
                    max_samples(&mut accumulator.pixels, &frame.raw.pixels, bytes, cancel);
                if index.is_some() {
                    self.frames.push_back((selected, frame));
                } else {
                    drop(frame);
                }
                reduced?;
                self.check_source()?;
                frames::check_cancel(Some(cancel))?;
                completed += 1;
                progress(0.6 * completed as f64 / total as f64);
            }
        }
        self.check_source()?;
        frames::check_cancel(Some(cancel))?;
        let display_bytes = if bytes == 2 { frame_bytes / 2 } else { 0 };
        let _display = self.reserve(display_bytes + FRAME_OVERHEAD + metadata_bytes(&accumulator.metadata), false)?
            .ok_or_else(|| format!("{} projection display conversion does not fit the application memory budget. Increase the limit in Settings.", self.format))?;
        let result = frames::write_frame_cancellable(&accumulator, output, true, Some(cancel));
        if let Err(error) = self
            .check_source()
            .and_then(|_| frames::check_cancel(Some(cancel)))
        {
            if result.is_ok() {
                let _ = std::fs::remove_file(output);
            }
            return Err(error);
        }
        result
    }
}

fn max_samples(
    output: &mut [u8],
    input: &[u8],
    bytes: usize,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if output.len() != input.len() || !matches!(bytes, 1 | 2) || !input.len().is_multiple_of(bytes)
    {
        return Err("Projection frames have incompatible sample layouts.".into());
    }
    for (output, input) in output
        .chunks_mut(65536 * bytes)
        .zip(input.chunks(65536 * bytes))
    {
        frames::check_cancel(Some(cancel))?;
        if bytes == 1 {
            for (destination, source) in output.iter_mut().zip(input) {
                *destination = (*destination).max(*source);
            }
        } else {
            for (destination, source) in output
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(input.as_chunks::<2>().0)
            {
                *destination = u16::from_le_bytes(*destination)
                    .max(u16::from_le_bytes(*source))
                    .to_le_bytes();
            }
        }
    }
    frames::check_cancel(Some(cancel))
}

fn metadata_bytes(metadata: &ExtractedFrame) -> u64 {
    let positions = &metadata.dimensions.positions;
    let mut bytes = positions.capacity() as u64 * std::mem::size_of::<String>() as u64
        + positions
            .iter()
            .map(|name| name.capacity() as u64)
            .sum::<u64>()
        + metadata.channels.capacity() as u64
            * std::mem::size_of::<crate::channels::Channel>() as u64
        + metadata.display_ranges.capacity() as u64 * std::mem::size_of::<[u16; 2]>() as u64;
    for channel in &metadata.channels {
        bytes += channel.name.capacity() as u64 + channel.color.capacity() as u64;
    }
    bytes
}

fn retained_frame_bytes(frame: &RawFrame) -> u64 {
    frame.pixels.capacity() as u64 + FRAME_OVERHEAD + metadata_bytes(&frame.metadata)
}

fn compact_images(images: Vec<ImageInfo>) -> Vec<ImageInfo> {
    fn name(value: Option<String>) -> Option<String> {
        value.map(|value| value.chars().take(256).collect())
    }
    images
        .into_iter()
        .map(|image| {
            let mut compact =
                ImageInfo::new(image.index, image.size_x, image.size_y, image.pixel_type);
            compact.name = name(image.name);
            compact.size_c = image.size_c;
            compact.size_z = image.size_z;
            compact.size_t = image.size_t;
            compact.samples_per_pixel = image.samples_per_pixel;
            compact.physical_size = image.physical_size;
            compact.time_increment_s = image.time_increment_s;
            compact.channels = image
                .channels
                .into_iter()
                .take(image.size_c.min(crate::channels::MAX_CHANNELS) as usize)
                .map(|channel| ChannelInfo {
                    index: channel.index,
                    name: name(channel.name),
                    color: channel.color.filter(|color| {
                        color.len() == 7
                            && color.starts_with('#')
                            && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
                    }),
                    ..ChannelInfo::default()
                })
                .collect();
            compact
        })
        .collect()
}

// compact_images constructs fresh defaults, so these are its only heap fields.
fn summary_bytes(images: &[ImageInfo], capacity: usize) -> u64 {
    let mut bytes = FRAME_OVERHEAD + capacity as u64 * std::mem::size_of::<ImageInfo>() as u64;
    for image in images {
        bytes += image.dimension_order.capacity() as u64
            + image.physical_size.unit.capacity() as u64
            + image.name.as_ref().map_or(0, |name| name.capacity() as u64)
            + image.channels.capacity() as u64 * std::mem::size_of::<ChannelInfo>() as u64;
        for channel in &image.channels {
            bytes += channel
                .name
                .as_ref()
                .map_or(0, |name| name.capacity() as u64)
                + channel
                    .color
                    .as_ref()
                    .map_or(0, |color| color.capacity() as u64);
        }
    }
    bytes
}

pub(crate) fn reserve_after_reclaim(
    budget: &Arc<Budget>,
    bytes: u64,
    background: bool,
) -> Result<Option<Reservation>, String> {
    if let Some(reservation) = budget.try_reserve(bytes, background)? {
        return Ok(Some(reservation));
    }
    if !background && bytes <= budget.status()?.limit_bytes {
        budget.request_reclaim()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Some(reservation) = budget.try_reserve(bytes, false)? {
                return Ok(Some(reservation));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    Ok(None)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
}

impl Identity {
    fn from(metadata: std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct CheckedSource {
    path: PathBuf,
    file: File,
    reader: LocalFile,
    identity: Identity,
}

impl CheckedSource {
    fn open(path: PathBuf) -> Result<Self, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        // Deny write/delete sharing for the session on Windows. Linux readers
        // use the pinned descriptor and detect in-place writes around each read.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let file = options.open(&path).map_err(|e| e.to_string())?;
        let identity = Identity::from(file.metadata().map_err(|e| e.to_string())?);
        let reader = LocalFile::from_file(file.try_clone().map_err(|e| e.to_string())?, &path);
        Ok(Self {
            path,
            file,
            reader,
            identity,
        })
    }

    pub(crate) fn check(&self) -> std::io::Result<()> {
        let current = self.file.metadata().map(Identity::from)?;
        let path = std::fs::metadata(&self.path).map(Identity::from)?;
        if current != self.identity || path != self.identity {
            return Err(std::io::Error::other(
                "The image source changed while open. Reopen the file.",
            ));
        }
        Ok(())
    }

    pub(crate) fn stable_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.path.clone()
        }
    }
}

impl ByteSource for CheckedSource {
    fn size(&self) -> std::io::Result<u64> {
        self.check()?;
        Ok(self.identity.len)
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        let read = self.reader.read_at(offset, buffer)?;
        self.check()?;
        Ok(read)
    }
    fn name(&self) -> String {
        self.path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::tests::{strip, temporary};
    use crate::nd2::tests::{synthetic, synthetic_samples};
    use crate::nd2::Bounds;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    use std::sync::atomic::Ordering;

    #[test]
    fn raw_frames_survive_display_conversion_and_foreground_evicts_for_admission() {
        let temp = temporary();
        let source = temp.join("raw.nd2");
        let bytes = synthetic(16, false);
        std::fs::write(&source, &bytes).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        budget.set_limit(crate::memory::MIN_LIMIT_BYTES).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        let first = Selection::default();
        let output = temp.join("display.tif");
        let description = cache.extract_display(&output, first).unwrap();
        assert_eq!(description.display_ranges, [[1, 3], [2, 4]]);
        assert_eq!(strip(&output), [0, 0, 255, 255]);
        let original: Vec<u8> = [1u16, 2, 3, 4]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(cache.frames[0].1.raw.pixels, original);
        assert_eq!(cache.retained_bytes(), 8);
        assert_eq!(
            cache.frames[0].1._reservation.bytes(),
            retained_frame_bytes(&cache.frames[0].1.raw)
        );
        assert!(cache.contains(first));
        let raw_pointer = cache.frames[0].1.raw.pixels.as_ptr();
        let cached = temp.join("cached.tif");
        cache.extract_display(&cached, first).unwrap();
        assert_eq!(strip(&cached), strip(&output));
        assert_eq!(cache.frames[0].1.raw.pixels.as_ptr(), raw_pointer);

        let next = Selection { z: 1, ..first };
        let peak = cache.geometry(next).unwrap().decode_peak_bytes;
        let before = budget.status().unwrap();
        // Leave one byte less than decoding requires. Background work must keep
        // the existing useful frame; foreground can evict it to reclaim its lease.
        let other = budget
            .try_reserve(before.available_bytes - peak + 1, false)
            .unwrap()
            .unwrap();
        assert!(!cache.preload(next));
        assert!(cache.contains(first));
        let selected = temp.join("selected.tif");
        cache.extract_display(&selected, next).unwrap();
        assert!(!cache.contains(first));
        assert!(cache.contains(next));
        let status = budget.status().unwrap();
        assert!(status.used_bytes <= status.limit_bytes);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(other);
        cache.clear();
        assert_eq!(cache.cached_count(), 0);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    fn projection_source() -> Vec<u8> {
        synthetic_samples(
            16,
            false,
            &[
                &[256, 10, 50000, 20],
                &[60000, 30, 300, 15],
                &[1, 1, 2, 2],
                &[3, 3, 4, 4],
                &[1000, 100, 65000, 200],
                &[62000, 150, 64000, 180],
                &[500, 50, 600, 60],
                &[700, 70, 800, 80],
            ],
        )
    }

    #[test]
    fn projections_max_original_unsigned_samples_before_normalizing_and_keep_cached_frames() {
        let temp = temporary();
        let source = temp.join("projections.nd2");
        let bytes = projection_source();
        std::fs::write(&source, &bytes).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        cache
            .extract_display(&temp.join("first.tif"), Selection::default())
            .unwrap();
        let raw_pointer = cache.frames[0].1.raw.pixels.as_ptr();
        let cancel = AtomicBool::new(false);
        for (index, (projection, selection, ranges, pixels, count)) in [
            (
                Projection::Z,
                Selection {
                    z: 1,
                    ..Selection::default()
                },
                [[50000, 60000], [20, 30]],
                [255, 255, 0, 0],
                2,
            ),
            (
                Projection::Time,
                Selection::default(),
                [[1000, 65000], [100, 200]],
                [0, 0, 255, 255],
                2,
            ),
            (
                Projection::ZTime,
                Selection {
                    z: 1,
                    time: 1,
                    ..Selection::default()
                },
                [[62000, 65000], [150, 200]],
                [0, 0, 255, 255],
                4,
            ),
            (
                Projection::ZTime,
                Selection {
                    position: 1,
                    ..Selection::default()
                },
                [[700, 800], [70, 80]],
                [0, 0, 255, 255],
                4,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let output = temp.join(format!("projection-{index}.tif"));
            let mut progress = Vec::new();
            let result = cache
                .extract_projection(&output, selection, projection, &cancel, |value| {
                    progress.push(value)
                })
                .unwrap();
            assert_eq!(result.display_ranges, ranges);
            assert_eq!(strip(&output), pixels);
            assert_eq!(result.channels.len(), 2);
            assert_eq!((result.dimensions.z, result.dimensions.time), (2, 2));
            let canonical = projection.canonical(selection);
            assert_eq!(
                (
                    result.dimensions.position,
                    result.dimensions.z_index,
                    result.dimensions.time_index
                ),
                (canonical.position, canonical.z, canonical.time)
            );
            assert_eq!(progress.len(), count + 1);
            assert_eq!(progress.first(), Some(&0.0));
            assert_eq!(progress.last(), Some(&0.6));
            assert!(progress.windows(2).all(|v| v[0] <= v[1]));
            assert_eq!(cache.cached_count(), 1);
            assert_eq!(cache.frames[0].1.raw.pixels.as_ptr(), raw_pointer);
        }
        // Per-frame normalization produces [0,0,255,255] and [255,255,0,0].
        // Their maximum would be white everywhere; original-intensity Z max is not.
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn projection_preserves_rgb_sample_order_and_shared_display_range() {
        let temp = temporary();
        let cancel = AtomicBool::new(false);
        for bits in [8, 16] {
            let source = temp.join(format!("rgb-{bits}.nd2"));
            let first: &[u16] = &[10, 100, 20, 30, 40, 50];
            let next: &[u16] = &[60, 20, 40, 20, 70, 10];
            std::fs::write(
                &source,
                synthetic_samples(
                    bits,
                    true,
                    &[first, next, first, next, first, next, first, next],
                ),
            )
            .unwrap();
            let budget = Budget::new(temp.join(format!("budget-{bits}"))).unwrap();
            let mut cache = Cache::new(Format::Nd2, source, Arc::clone(&budget)).unwrap();
            let output = temp.join(format!("rgb-{bits}.tif"));
            let result = cache
                .extract_projection(
                    &output,
                    Selection::default(),
                    Projection::Z,
                    &cancel,
                    |_| {},
                )
                .unwrap();
            if bits == 8 {
                assert_eq!(strip(&output), [40, 100, 60, 50, 70, 30]);
                assert!(result.display_ranges.is_empty());
            } else {
                assert_eq!(strip(&output), [36, 255, 109, 73, 146, 0]);
                assert_eq!(result.display_ranges, [[30, 100]; 3]);
            }
            assert_eq!(
                result
                    .channels
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>(),
                ["Red", "Green", "Blue"]
            );
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn projection_admission_evicts_optional_frames_without_retaining_streamed_inputs() {
        let temp = temporary();
        let source = temp.join("bounded.nd2");
        std::fs::write(&source, projection_source()).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        budget.set_limit(crate::memory::MIN_LIMIT_BYTES).unwrap();
        let mut cache = Cache::new(Format::Nd2, source, Arc::clone(&budget)).unwrap();
        cache
            .extract_display(&temp.join("first.tif"), Selection::default())
            .unwrap();
        let peak = cache
            .geometry(Selection::default())
            .unwrap()
            .decode_peak_bytes;
        let accumulator = 8 + FRAME_OVERHEAD;
        let available = budget.status().unwrap().available_bytes;
        let other = budget
            .try_reserve(available - peak - accumulator + 1, false)
            .unwrap()
            .unwrap();
        let output = temp.join("bounded.tif");
        let result = cache
            .extract_projection(
                &output,
                Selection::default(),
                Projection::ZTime,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(result.display_ranges, [[62000, 65000], [150, 200]]);
        assert_eq!(strip(&output), [0, 0, 255, 255]);
        assert_eq!(cache.cached_count(), 0);
        let status = budget.status().unwrap();
        assert!(status.used_bytes <= status.limit_bytes);
        drop(other);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn cancelling_projection_releases_accumulator_and_preserves_usable_raw_cache() {
        let temp = temporary();
        let source = temp.join("cancel.nd2");
        std::fs::write(&source, projection_source()).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source, Arc::clone(&budget)).unwrap();
        cache
            .extract_display(&temp.join("first.tif"), Selection::default())
            .unwrap();
        let before = budget.status().unwrap().used_bytes;
        let cancel = AtomicBool::new(false);
        let output = temp.join("cancelled.tif");
        let error = cache
            .extract_projection(
                &output,
                Selection::default(),
                Projection::ZTime,
                &cancel,
                |value| {
                    if value > 0.0 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            )
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(!output.exists());
        assert_eq!(budget.status().unwrap().used_bytes, before);
        assert!(cache.contains(Selection::default()));
        cancel.store(false, Ordering::Relaxed);
        cache
            .extract_projection(
                &output,
                Selection::default(),
                Projection::Z,
                &cancel,
                |_| {},
            )
            .unwrap();
        assert_eq!(strip(&output), [255, 255, 0, 0]);
        let ordinary = temp.join("ordinary-cancelled.tif");
        assert!(cache
            .extract_projection(
                &ordinary,
                Selection::default(),
                Projection::None,
                &cancel,
                |value| {
                    if value == 0.6 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            )
            .is_err());
        assert!(!ordinary.exists());
        let mut samples = vec![0; 65537 * 2];
        assert!(max_samples(&mut samples, &vec![255; 65537 * 2], 2, &cancel).is_err());
        assert!(samples.iter().all(|v| *v == 0));
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn changing_source_during_projection_clears_raw_cache_and_aborts_output() {
        let temp = temporary();
        let source = temp.join("changing.nd2");
        std::fs::write(&source, projection_source()).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        cache
            .extract_display(&temp.join("first.tif"), Selection::default())
            .unwrap();
        let replacement = temp.join("replacement.nd2");
        std::fs::write(&replacement, synthetic(8, false)).unwrap();
        let output = temp.join("changed.tif");
        let mut replaced = false;
        let result = cache.extract_projection(
            &output,
            Selection::default(),
            Projection::ZTime,
            &AtomicBool::new(false),
            |value| {
                if value > 0.0 && !replaced {
                    std::fs::rename(&replacement, &source).unwrap();
                    replaced = true;
                }
            },
        );
        assert!(result.unwrap_err().contains("source changed"));
        assert!(!output.exists());
        assert_eq!(cache.cached_count(), 0);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn frame_accounting_includes_large_name_vectors_and_spare_pixel_capacity() {
        let image = ImageInfo::new(0, 1, 1, openreadout_core::PixelType::Uint8);
        let mut metadata =
            frames::description(Format::Nd2, &[image], Selection::default()).unwrap();
        metadata.dimensions.positions = Vec::with_capacity(8192);
        metadata
            .dimensions
            .positions
            .extend((0..4096).map(|_| "p".into()));
        metadata.display_ranges = Vec::with_capacity(32);
        let mut pixels = Vec::with_capacity(1024);
        pixels.push(1);
        let frame = RawFrame {
            width: 1,
            height: 1,
            bands: 1,
            bytes: 1,
            rgb: false,
            pixels,
            metadata,
        };
        let vector_bytes = 8192 * std::mem::size_of::<String>() as u64;
        assert!(
            retained_frame_bytes(&frame) >= 1024 + FRAME_OVERHEAD + vector_bytes + 4096 + 32 * 4
        );
        assert!(serde_json::to_vec(&frame.metadata).unwrap().len() < 48 * 1024);
    }

    fn metadata_source(additions: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let original = synthetic(16, false);
        let mut chunks = Vec::new();
        let mut cursor = 0;
        loop {
            let name_bytes =
                u32::from_le_bytes(original[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            let length =
                u64::from_le_bytes(original[cursor + 8..cursor + 16].try_into().unwrap()) as usize;
            let name = String::from_utf8(original[cursor + 16..cursor + 16 + name_bytes].to_vec())
                .unwrap();
            if name == openreadout_nd2::FILE_MAP_NAME {
                break;
            }
            let payload =
                original[cursor + 16 + name_bytes..cursor + 16 + name_bytes + length].to_vec();
            chunks.push((name, payload));
            cursor += 16 + name_bytes + length;
        }
        for (name, payload) in additions {
            if let Some(chunk) = chunks.iter_mut().find(|(key, _)| key == name) {
                chunk.1 = payload;
            } else {
                chunks.push((name.into(), payload));
            }
        }
        let mut output = Vec::new();
        let mut map = Vec::new();
        for (name, payload) in chunks {
            map.extend(name.as_bytes());
            map.extend((output.len() as u64).to_le_bytes());
            map.extend((payload.len() as u64).to_le_bytes());
            output.extend(openreadout_nd2::CHUNK_MAGIC_BYTES);
            output.extend((name.len() as u32).to_le_bytes());
            output.extend((payload.len() as u64).to_le_bytes());
            output.extend(name.as_bytes());
            output.extend(payload);
        }
        let offset = output.len() as u64;
        let name = openreadout_nd2::FILE_MAP_NAME;
        output.extend(openreadout_nd2::CHUNK_MAGIC_BYTES);
        output.extend((name.len() as u32).to_le_bytes());
        output.extend((map.len() as u64).to_le_bytes());
        output.extend(name.as_bytes());
        output.extend(map);
        output.extend(offset.to_le_bytes());
        output
    }

    fn lv_item(name: &str, value: &serde_json::Value) -> Vec<u8> {
        let kind = if value.is_object() {
            11
        } else if value.is_string() {
            8
        } else if value.as_u64().is_some() {
            3
        } else {
            6
        };
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let mut raw = vec![kind, name.len().try_into().unwrap()];
        for word in name {
            raw.extend(word.to_le_bytes());
        }
        match kind {
            3 => raw.extend((value.as_u64().unwrap() as u32).to_le_bytes()),
            6 => raw.extend(value.as_f64().unwrap().to_le_bytes()),
            8 => {
                for word in value.as_str().unwrap().encode_utf16().chain([0]) {
                    raw.extend(word.to_le_bytes());
                }
            }
            _ => {
                let children = value.as_object().unwrap();
                raw.extend((children.len() as u32).to_le_bytes());
                raw.extend(0u64.to_le_bytes());
                for (name, value) in children {
                    raw.extend(lv_item(name, value));
                }
                for _ in children {
                    raw.extend(0u64.to_le_bytes());
                }
            }
        }
        raw
    }

    fn compressed_lv(value: serde_json::Value) -> Vec<u8> {
        let raw: Vec<u8> = value
            .as_object()
            .unwrap()
            .iter()
            .flat_map(|(name, value)| lv_item(name, value))
            .collect();
        compressed_bytes(&raw)
    }

    fn compressed_bytes(raw: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(raw).unwrap();
        let mut output = vec![76; 10];
        output.extend(encoder.finish().unwrap());
        output
    }

    fn experiment_xml(positions: u32) -> Vec<u8> {
        format!(r#"<variant><no_name><eType runtype="lx_uint32" value="1"/><uLoopPars><uiCount runtype="lx_uint32" value="2"/><dPeriod runtype="double" value="2000"/></uLoopPars><ppNextLevelEx><i0><eType runtype="lx_uint32" value="2"/><uLoopPars><uiCount runtype="lx_uint32" value="{positions}"/></uLoopPars><ppNextLevelEx><i0><eType runtype="lx_uint32" value="4"/><uLoopPars><uiCount runtype="lx_uint32" value="2"/><dZStep runtype="double" value="1.5"/></uLoopPars></i0></ppNextLevelEx></i0></ppNextLevelEx></no_name></variant>"#).into_bytes()
    }

    #[test]
    fn compressed_metadata_is_bounded_and_compacted_without_losing_display_information() {
        let temp = temporary();
        let source = temp.join("compressed-metadata.nd2");
        let attrs = serde_json::json!({"uiWidth":2,"uiHeight":1,"uiComp":2,"uiWidthBytes":8,
            "uiBpcInMemory":16,"uiBpcSignificant":16,"uiSequenceCount":8,"ePixelType":1,"eCompression":2});
        let seq = serde_json::json!({"dCalibration":0.25,"unused": "microscope".repeat(1024),
            "sPicturePlanes":{"sPlaneNew":{
                "a0":{"uiCompCount":1,"sDescription":"DAPI","uiColor":4278190335u32},
                "a1":{"uiCompCount":1,"sDescription":"CY5","uiColor":4278255360u32}}}});
        let bytes = metadata_source(vec![
            ("ImageAttributesLV!", compressed_lv(attrs)),
            ("ImageMetadata!", experiment_xml(2)),
            ("ImageMetadataSeqLV|0!", compressed_lv(seq)),
        ]);
        std::fs::write(&source, &bytes).unwrap();
        let raw = Bounds::scan(&source).unwrap();
        let expanded = Bounds::validated(&source).unwrap();
        assert!(expanded.expanded_bytes > raw.expanded_bytes);
        assert!(expanded.metadata_workspace > raw.metadata_workspace);
        assert_eq!(nd2::preflight(&source).unwrap().positions, 2);
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        assert_eq!(
            cache._metadata.bytes(),
            summary_bytes(&cache.images, cache.images.capacity())
        );
        assert!(cache._metadata.bytes() < expanded.metadata_workspace);
        assert!(cache
            .images
            .iter()
            .all(|image| image.physical_size.unit == "µm"));
        assert!(cache.images.iter().all(|image| image.extra.is_empty()
            && image.objective.is_none()
            && image.instrument.is_none()));
        let result = cache
            .extract_display(
                &temp.join("metadata.tif"),
                Selection {
                    position: 1,
                    z: 1,
                    time: 1,
                },
            )
            .unwrap();
        assert_eq!(
            result
                .channels
                .iter()
                .map(|channel| (&*channel.name, &*channel.color))
                .collect::<Vec<_>>(),
            [("DAPI", "#FF0000"), ("CY5", "#00FF00")]
        );
        let calibration = result.calibration.unwrap();
        assert_eq!(calibration.pixel_width_um, Some(0.25));
        assert_eq!(calibration.pixel_height_um, Some(0.25));
        assert_eq!(calibration.z_step_um, Some(1.5));
        assert_eq!(calibration.time_increment_seconds, Some(2.0));
        assert_eq!(result.display_ranges, [[71, 73], [72, 74]]);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn position_metadata_peak_is_admitted_before_reader_info_is_materialized() {
        let temp = temporary();
        let source = temp.join("many-positions.nd2");
        let bytes = metadata_source(vec![
            ("ImageMetadata!", experiment_xml(4096)),
            (
                "ImageMetadataSeqLV|0!",
                compressed_lv(serde_json::json!({"unused":"x".repeat(8192)})),
            ),
        ]);
        std::fs::write(&source, bytes).unwrap();
        assert_eq!(nd2::preflight(&source).unwrap().positions, 4096);
        let budget = Budget::new(temp.join("budget")).unwrap();
        budget.set_limit(crate::memory::MIN_LIMIT_BYTES).unwrap();
        let error = Cache::new(Format::Nd2, source, Arc::clone(&budget))
            .err()
            .unwrap();
        assert!(error.contains("position metadata expansion"), "{error}");
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn ignored_and_xml_only_chunks_do_not_get_misread_as_compressed_lv() {
        let temp = temporary();
        let source = temp.join("ignored-lv0z.nd2");
        // This vendor-specific LV0Z envelope starts with 76 but its zlib stream
        // is not at the LV type-76 offset. The reader does not parse this chunk.
        let opaque = b"LV0Zr\0\0\0\0\0\0\0\x78\x9c".to_vec();
        assert!(crate::nd2_metadata::expanded_size(&opaque).is_err());
        let bytes = metadata_source(vec![
            ("CustomData|CustomDescriptionV1_0!", opaque.clone()),
            ("CustomData|unknown!", opaque.clone()),
            ("ImageTextInfo!", opaque),
        ]);
        std::fs::write(&source, &bytes).unwrap();
        let raw = Bounds::scan(&source).unwrap();
        let validated = Bounds::validated(&source).unwrap();
        assert_eq!(validated.expanded_bytes, raw.expanded_bytes);
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        let output = temp.join("ignored-lv0z.tif");
        let result = cache
            .extract_display(&output, Selection::default())
            .unwrap();
        assert_eq!(result.display_ranges, [[1, 3], [2, 4]]);
        assert_eq!(strip(&output), [0, 0, 255, 255]);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn parsed_compression_errors_identify_the_metadata_chunk() {
        let temp = temporary();
        let source = temp.join("bad-parsed-lv.nd2");
        std::fs::write(
            &source,
            metadata_source(vec![("ImageTextInfoLV!", vec![76; 26])]),
        )
        .unwrap();
        let error = Bounds::validated(&source).err().unwrap();
        assert!(error.contains("ImageTextInfoLV!"), "{error}");
        assert!(error.contains("compressed metadata is invalid"), "{error}");
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn metadata_expansion_is_shared_across_compressed_chunks_before_parsing() {
        let temp = temporary();
        let source = temp.join("aggregate-metadata.nd2");
        let packed = compressed_bytes(&vec![0; 17 * 1024 * 1024]);
        std::fs::write(
            &source,
            metadata_source(vec![
                ("ImageTextInfoLV!", packed.clone()),
                ("ImageMetadataSeqLV|0!", packed),
            ]),
        )
        .unwrap();
        assert!(Bounds::scan(&source).is_ok());
        let budget = Budget::new(temp.join("budget")).unwrap();
        let error = Cache::new(Format::Nd2, source, Arc::clone(&budget))
            .err()
            .unwrap();
        assert!(error.contains("32 MiB expanded"), "{error}");
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn changing_source_invalidates_raw_cache_before_another_decode() {
        let temp = temporary();
        let source = temp.join("replace.nd2");
        std::fs::write(&source, synthetic(8, false)).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Nd2, source.clone(), Arc::clone(&budget)).unwrap();
        cache
            .extract_display(&temp.join("first.tif"), Selection::default())
            .unwrap();
        let mut larger = synthetic(16, false);
        larger.extend([0; 4096]);
        #[cfg(windows)]
        {
            assert!(std::fs::write(&source, &larger).is_err());
            drop(cache);
            std::fs::write(&source, larger).unwrap();
        }
        #[cfg(not(windows))]
        {
            let replacement = temp.join("larger.nd2");
            std::fs::write(&replacement, larger).unwrap();
            std::fs::rename(replacement, &source).unwrap();
            assert!(!cache.preload(Selection {
                z: 1,
                ..Selection::default()
            }));
            assert_eq!(cache.cached_count(), 0);
            assert!(cache
                .extract_display(&temp.join("stale.tif"), Selection::default())
                .is_err());
            assert!(!temp.join("stale.tif").exists());
            drop(cache);
        }
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        drop(budget);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn header_planning_rejects_rescue_maps_and_misnamed_entries() {
        let temp = temporary();
        for kind in 0..3 {
            let mut bytes = synthetic(8, false);
            let map_offset =
                u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
            let name_len =
                u32::from_le_bytes(bytes[map_offset + 4..map_offset + 8].try_into().unwrap())
                    as usize;
            let map_start = map_offset + 16 + name_len;
            let map_len =
                u64::from_le_bytes(bytes[map_offset + 8..map_offset + 16].try_into().unwrap())
                    as usize;
            match kind {
                0 => bytes[map_offset + 8..map_offset + 16].copy_from_slice(&0u64.to_le_bytes()),
                1 => {
                    let name = b"ImageAttributes!";
                    let index = bytes[map_start..map_start + map_len]
                        .windows(name.len())
                        .position(|v| v == name)
                        .unwrap()
                        + map_start;
                    bytes.drain(index..index + name.len() + 16);
                    bytes[map_offset + 8..map_offset + 16]
                        .copy_from_slice(&((map_len - name.len() - 16) as u64).to_le_bytes());
                }
                _ => {
                    let name = b"ImageMetadata!";
                    let index = bytes[map_start..map_start + map_len]
                        .windows(name.len())
                        .position(|v| v == name)
                        .unwrap()
                        + map_start;
                    bytes[index..index + name.len()].copy_from_slice(b"ImageDataSeq0!");
                }
            }
            let source = temp.join(format!("bad-map-{kind}.nd2"));
            std::fs::write(&source, bytes).unwrap();
            assert!(
                Bounds::scan(&source).is_err(),
                "map kind {kind} should be rejected before reader open"
            );
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn header_planning_rejects_oversized_metadata_before_reader_allocation() {
        let temp = temporary();
        let source = temp.join("oversized-map.nd2");
        let mut bytes = synthetic(8, false);
        let map_offset = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
        bytes[map_offset + 8..map_offset + 16].copy_from_slice(&(32 * MIB + 1).to_le_bytes());
        std::fs::write(&source, bytes).unwrap();
        assert!(Bounds::scan(&source).is_err());
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    #[ignore = "requires public fixtures in test-data/nd2; parent operator runs this check"]
    fn plans_real_chunk_maps_with_padding_between_chunks() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/nd2");
        for name in [
            "dims_p4z5t3c2y32x32.nd2",
            "rgb.nd2",
            "compressed_lossless.nd2",
        ] {
            let plan = Bounds::scan(&fixtures.join(name)).unwrap();
            assert!(plan.metadata_workspace > 0);
            assert!(plan.encoded_bytes > 0);
            assert!(!plan.legacy);
        }
    }
}
