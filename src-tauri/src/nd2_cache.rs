//! Original ND2 frames retained under the shared process memory budget.

use crate::memory::{Budget, Reservation};
use crate::nd2::{self, ExtractedNd2, RawFrame, Selection};
use openreadout_core::source::{ByteSource, Input, LocalFile};
use openreadout_core::{FormatReader, ImageInfo};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MIB: u64 = 1024 * 1024;
const FRAME_OVERHEAD: u64 = 64 * 1024;
const MAX_METADATA_BYTES: u64 = 32 * MIB;

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
    source: Arc<CheckedSource>,
    input: Input,
    budget: Arc<Budget>,
    images: Vec<ImageInfo>,
    storage_bytes: u64,
    bounds: Bounds,
    _metadata: Reservation,
    // ponytail: linear LRU lookup; frame buffers dominate these small entries.
    frames: VecDeque<(Selection, Frame)>,
}

impl Cache {
    pub fn new(source: PathBuf, budget: Arc<Budget>) -> Result<Self, String> {
        let source = Arc::new(CheckedSource::open(source)?);
        // Only bounded header reads occur before reserving parser allocations.
        let bounds = Bounds::scan(&source.stable_path())?;
        source.check().map_err(|e| e.to_string())?;
        let metadata = reserve_after_reclaim(&budget, bounds.metadata_workspace, false)?
            .ok_or("ND2 metadata does not fit the application memory budget.")?;
        let storage_bytes = nd2::preflight(&source.stable_path())?;
        source.check().map_err(|e| e.to_string())?;
        let input = Input::from_source(&source.path, source.clone());
        let dataset = openreadout_nd2::Nd2Reader
            .open_input(&input)
            .map_err(|e| format!("ND2: {e}"))?;
        let images = dataset.info().map_err(|e| format!("ND2: {e}"))?.images;
        source.check().map_err(|e| e.to_string())?;
        // The library retains split channel data internally; never retain a reader.
        drop(dataset);
        nd2::description(&images, Selection::default())?;
        Ok(Self {
            source,
            input,
            budget,
            images,
            storage_bytes,
            bounds,
            _metadata: metadata,
            frames: VecDeque::new(),
        })
    }

    pub fn describe(&self, selection: Selection) -> Result<ExtractedNd2, String> {
        nd2::description(&self.images, selection)
    }

    pub fn geometry(&self, selection: Selection) -> Result<Geometry, String> {
        let metadata = self.describe(selection)?;
        let image = &self.images[selection.position as usize];
        let bands = metadata.channels.len() as u32;
        let bytes = image.pixel_type.bytes_per_sample();
        let frame_bytes =
            u64::from(image.size_x) * u64::from(image.size_y) * u64::from(bands) * bytes as u64;
        // Pinned openreadout 0.3.0: modern decoding has a padded input, zlib Vec
        // growth and split channel planes alongside our assembled frame. Legacy
        // JPEG2000 additionally keeps i32 coefficient, reconstruction and sample
        // planes; allow 32 bytes of codec storage per 16-bit storage byte.
        let multiplier = if self.bounds.legacy { 32 } else { 4 };
        let decode_peak_bytes = self.bounds.metadata_workspace
            + self.bounds.encoded_bytes
            + self.storage_bytes.max(frame_bytes) * multiplier
            + frame_bytes
            + FRAME_OVERHEAD;
        Ok(Geometry {
            width: image.size_x,
            height: image.size_y,
            decode_peak_bytes,
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
            let mut dataset = openreadout_nd2::Nd2Reader
                .open_input(&self.input)
                .map_err(|e| format!("ND2: {e}"))?;
            let images = dataset.info().map_err(|e| format!("ND2: {e}"))?.images;
            nd2::decode_frame(dataset.as_mut(), &images, selection)
        })();
        self.check_source()?;
        let raw = decoded?;
        reservation.shrink(raw.pixels.len() as u64 + FRAME_OVERHEAD)?;
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
    ) -> Result<ExtractedNd2, String> {
        self.extract_display_with_priority(output, selection, false)
    }

    pub fn extract_display_with_priority(
        &mut self,
        output: &Path,
        selection: Selection,
        background: bool,
    ) -> Result<ExtractedNd2, String> {
        self.trim()?;
        let frame = if let Some(index) = self.frames.iter().position(|(key, _)| *key == selection) {
            self.frames.remove(index).unwrap().1
        } else {
            self.load(selection, background, true)?.ok_or("ND2 selected frame decoding does not fit the application memory budget. Increase the limit in Settings.")?
        };
        // Keep the raw frame's lease while normalizing its separate display copy.
        let display_bytes = if frame.raw.bytes == 2 {
            frame.raw.pixels.len() as u64 / 2
        } else {
            0
        };
        let result = match self.reserve(display_bytes + FRAME_OVERHEAD, background)? {
            Some(_display) => nd2::write_frame(&frame.raw, output, true),
            None => Err("ND2 display conversion does not fit the application memory budget. Increase the limit in Settings.".into()),
        };
        if let Err(error) = self.check_source() {
            if result.is_ok() {
                let _ = std::fs::remove_file(output);
            }
            return Err(error);
        }
        self.frames.push_back((selection, frame));
        result
    }
}

fn reserve_after_reclaim(
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
struct CheckedSource {
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
        let file = options.open(&path).map_err(|e| format!("ND2: {e}"))?;
        let identity = Identity::from(file.metadata().map_err(|e| format!("ND2: {e}"))?);
        let reader =
            LocalFile::from_file(file.try_clone().map_err(|e| format!("ND2: {e}"))?, &path);
        Ok(Self {
            path,
            file,
            reader,
            identity,
        })
    }

    fn check(&self) -> std::io::Result<()> {
        let current = self.file.metadata().map(Identity::from)?;
        let path = std::fs::metadata(&self.path).map(Identity::from)?;
        if current != self.identity || path != self.identity {
            return Err(std::io::Error::other(
                "ND2 source changed while open. Reopen the acquisition.",
            ));
        }
        Ok(())
    }

    fn stable_path(&self) -> PathBuf {
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

struct Bounds {
    metadata_workspace: u64,
    encoded_bytes: u64,
    legacy: bool,
}

impl Bounds {
    // Walk legacy boxes or stream the modern chunk map before allocating the
    // reader's parsed metadata. Pixel payloads are skipped, never read here.
    fn scan(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("ND2: {e}"))?;
        let file_len = file.metadata().map_err(|e| format!("ND2: {e}"))?.len();
        let mut signature = [0; 12];
        file.read_exact(&mut signature)
            .map_err(|e| format!("ND2: {e}"))?;
        let legacy = signature == openreadout_nd2::LEGACY_SIGNATURE;
        if !legacy {
            return Self::scan_chunk_map(path, file, file_len);
        }
        let mut offset = 0u64;
        let mut metadata_bytes = 0u64;
        let mut encoded_bytes = 0u64;
        let mut entries = 0u64;
        while file_len.saturating_sub(offset) >= if legacy { 8 } else { 16 } {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("ND2: {e}"))?;
            let mut header = [0; 16];
            file.read_exact(&mut header[..8])
                .map_err(|e| format!("ND2: {e}"))?;
            let length = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
                0 => file_len - offset,
                1 => {
                    file.read_exact(&mut header[8..])
                        .map_err(|e| format!("ND2: {e}"))?;
                    let length = u64::from_be_bytes(header[8..].try_into().unwrap());
                    if length < 16 {
                        return Err("ND2 legacy box has an invalid extended length.".into());
                    }
                    length
                }
                n => u64::from(n),
            };
            if length < 8 {
                return Err("ND2 legacy box has an invalid length.".into());
            }
            let pixels = &header[4..8] == b"jp2c";
            if offset.checked_add(length).is_none_or(|end| end > file_len) {
                return Err("ND2 chunk extends beyond its source file.".into());
            }
            if pixels {
                if length > nd2::MAX_FRAME_BYTES + MIB + 65552 {
                    return Err("ND2 encoded frame exceeds the 257 MiB input-buffer limit.".into());
                }
                encoded_bytes = encoded_bytes.max(length);
            } else {
                metadata_bytes = metadata_bytes
                    .checked_add(length)
                    .ok_or("ND2 metadata length overflows.")?;
                if metadata_bytes > MAX_METADATA_BYTES {
                    return Err("ND2 metadata and chunk map exceed the 32 MiB input limit.".into());
                }
            }
            entries += 1;
            offset += length;
        }
        Ok(Self {
            metadata_workspace: metadata_bytes * 24 + entries * 256 + 8 * MIB,
            encoded_bytes,
            legacy,
        })
    }
    fn scan_chunk_map(path: &Path, mut file: File, file_len: u64) -> Result<Self, String> {
        fn read(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(bytes))
                .map_err(|e| format!("ND2: {e}"))
        }
        fn header(
            file: &mut File,
            offset: u64,
            file_len: u64,
        ) -> Result<(Vec<u8>, u64, u64), String> {
            let mut raw = [0; 16];
            read(file, offset, &mut raw)?;
            if raw[..4] != openreadout_nd2::CHUNK_MAGIC_BYTES {
                return Err("ND2 chunk header has invalid magic.".into());
            }
            let name_len = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
            let length = u64::from_le_bytes(raw[8..].try_into().unwrap());
            if name_len > 65536
                || offset
                    .checked_add(16 + name_len as u64)
                    .and_then(|n| n.checked_add(length))
                    .is_none_or(|n| n > file_len)
            {
                return Err(
                    "ND2 chunk extends beyond its source file or has an excessive name.".into(),
                );
            }
            let mut name = vec![0; name_len];
            file.read_exact(&mut name)
                .map_err(|e| format!("ND2: {e}"))?;
            Ok((name, offset + 16 + name_len as u64, length))
        }
        let mut tail = [0; 8];
        read(
            &mut file,
            file_len.checked_sub(8).ok_or("ND2 is truncated.")?,
            &mut tail,
        )?;
        let (name, map_start, map_bytes) = header(&mut file, u64::from_le_bytes(tail), file_len)?;
        if name.split(|b| *b == 0).next() != Some(openreadout_nd2::FILE_MAP_NAME.as_bytes()) {
            return Err("ND2 requires an intact chunk map for bounded memory decoding.".into());
        }
        if map_bytes > MAX_METADATA_BYTES {
            return Err("ND2 chunk map exceeds the 32 MiB input limit.".into());
        }
        let mut map = BufReader::new(File::open(path).map_err(|e| format!("ND2: {e}"))?);
        map.seek(SeekFrom::Start(map_start))
            .map_err(|e| format!("ND2: {e}"))?;
        let mut cursor = map_start;
        let map_end = map_start + map_bytes;
        let mut metadata_bytes = map_bytes;
        let mut encoded_bytes = 0u64;
        let mut entries = 0u64;
        let mut has_attributes = false;
        let mut name = Vec::new();
        while cursor < map_end {
            name.clear();
            loop {
                if cursor >= map_end || name.len() >= 65536 {
                    return Err("ND2 chunk map name is truncated or excessive.".into());
                }
                let mut byte = [0];
                map.read_exact(&mut byte).map_err(|e| format!("ND2: {e}"))?;
                cursor += 1;
                name.push(byte[0]);
                if byte[0] == b'!' {
                    break;
                }
            }
            if name == openreadout_nd2::CHUNK_MAP_SIGNATURE.as_bytes() {
                break;
            }
            if cursor + 16 > map_end {
                return Err("ND2 chunk map entry is truncated.".into());
            }
            let mut item = [0; 16];
            map.read_exact(&mut item).map_err(|e| format!("ND2: {e}"))?;
            cursor += 16;
            let (mut actual_name, _, length) = header(
                &mut file,
                u64::from_le_bytes(item[..8].try_into().unwrap()),
                file_len,
            )?;
            while actual_name.last() == Some(&0) {
                actual_name.pop();
            }
            if actual_name != name {
                return Err("ND2 chunk map name disagrees with its referenced header.".into());
            }
            has_attributes |=
                matches!(name.as_slice(), b"ImageAttributes!" | b"ImageAttributesLV!");
            let pixels = actual_name.starts_with(b"ImageDataSeq|");
            if pixels {
                if length > nd2::MAX_FRAME_BYTES + MIB {
                    return Err("ND2 encoded frame exceeds the 257 MiB input-buffer limit.".into());
                }
                encoded_bytes = encoded_bytes.max(length);
            } else {
                metadata_bytes = metadata_bytes
                    .checked_add(length)
                    .ok_or("ND2 metadata length overflows.")?;
                if metadata_bytes > MAX_METADATA_BYTES {
                    return Err("ND2 metadata and chunk map exceed the 32 MiB input limit.".into());
                }
            }
            entries += 1;
        }
        if entries == 0 || !has_attributes {
            return Err("ND2 requires a nonempty chunk map containing image attributes; rescue scanning is unsupported by bounded decoding.".into());
        }
        Ok(Self {
            metadata_workspace: metadata_bytes * 24 + entries * 256 + 8 * MIB,
            encoded_bytes,
            legacy: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nd2::tests::{strip, synthetic, temporary};

    #[test]
    fn raw_frames_survive_display_conversion_and_foreground_evicts_for_admission() {
        let temp = temporary();
        let source = temp.join("raw.nd2");
        let bytes = synthetic(16, false);
        std::fs::write(&source, &bytes).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        budget.set_limit(crate::memory::MIN_LIMIT_BYTES).unwrap();
        let mut cache = Cache::new(source.clone(), Arc::clone(&budget)).unwrap();
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

    #[test]
    fn changing_source_invalidates_raw_cache_before_another_decode() {
        let temp = temporary();
        let source = temp.join("replace.nd2");
        std::fs::write(&source, synthetic(8, false)).unwrap();
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(source.clone(), Arc::clone(&budget)).unwrap();
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
        bytes[map_offset + 8..map_offset + 16]
            .copy_from_slice(&(MAX_METADATA_BYTES + 1).to_le_bytes());
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
