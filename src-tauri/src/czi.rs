//! Zeiss CZI checks before the pinned reader allocates, and CZI decode accounting.

use crate::frame_cache::{reserve_after_reclaim, CheckedSource, FRAME_OVERHEAD, MIB};
use crate::frames::MAX_FRAME_BYTES;
use crate::memory::{Budget, Reservation};
use openreadout_core::ImageInfo;
use openreadout_czi::{
    CompressionId, DimensionEntry, DirectoryEntry, PixelTypeId, SegmentId, SEGMENT_HEADER_LEN,
};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

const MAX_METADATA_BYTES: u64 = 32 * MIB;
const MAX_IMAGES: usize = 4096;
// Pinned openreadout-czi 0.3.0 dataset.rs and attach.rs constants: one plane's
// tiles decode in batches of at most this many output bytes; subblock masks and
// TimeStamps/EventList attachments up to these sizes are read and interpreted.
const TILE_BATCH_BYTES: u64 = 256 * MIB;
const MAX_SUBBLOCK_MASK: u64 = MIB;
const MAX_INTERPRETED_ATTACHMENT: u64 = 64 * MIB;

fn error(error: impl std::fmt::Display) -> String {
    format!("CZI: {error}")
}

/// CZI bounds validated before the reader allocates.
pub(crate) struct Plan {
    metadata_workspace: u64,
    interpreted_bytes: u64,
    images: usize,
    /// Largest full-resolution subblock: stored, decoded by the reader, and its payload.
    stored_tile_bytes: u64,
    encoded_bytes: u64,
    jpeg: bool,
}

impl Plan {
    /// Returns the plan and a reservation covering the reader's opening peak.
    pub(crate) fn new(
        source: &CheckedSource,
        budget: &Arc<Budget>,
    ) -> Result<(Self, Reservation), String> {
        let plan = scan(&source.stable_path())?;
        source.check().map_err(|e| e.to_string())?;
        // info() copies interpreted time stamps and events into every image.
        let opening_bytes =
            plan.metadata_workspace + plan.interpreted_bytes * 16 * (plan.images as u64 + 1);
        let metadata = reserve_after_reclaim(budget, opening_bytes, false)?
            .ok_or("CZI metadata parsing does not fit the application memory budget.")?;
        Ok((plan, metadata))
    }

    pub(crate) fn check_images(&self, images: &[ImageInfo]) -> Result<(), String> {
        let first = images
            .first()
            .ok_or("CZI contains no readable full-resolution image.")?;
        if images.len() > self.images {
            return Err("CZI decoded image count exceeds its validated subblock directory.".into());
        }
        // Positions share the Dimensions panel's Z, time and channel controls.
        // XY sizes may differ; each frame has its own geometry and display cache.
        if images.iter().any(|image| {
            (
                image.size_z,
                image.size_t,
                image.size_c,
                image.pixel_type,
                image.samples_per_pixel,
            ) != (
                first.size_z,
                first.size_t,
                first.size_c,
                first.pixel_type,
                first.samples_per_pixel,
            )
        }) {
            return Err("CZI scenes differ in Z, time, channel count or pixel type. Browsing requires consistent scenes; export them separately as TIFF.".into());
        }
        Ok(())
    }

    pub(crate) fn decode_peak(&self, frame_bytes: u64) -> u64 {
        // Pinned openreadout-czi 0.3.0 reopens per frame, then decodes a plane's
        // tiles on the rayon pool, at most TILE_BATCH_BYTES of output at once. Each
        // tile in flight holds its payload, the decoded block and a converted copy
        // (JPEG may decode up to four times its declared size before conforming).
        // The reader's plane and our assembled frame coexist.
        let threads = rayon::current_num_threads() as u64;
        let decoded = (threads * self.stored_tile_bytes).min(TILE_BATCH_BYTES / 3 * 4);
        let multiplier = if self.jpeg { 5 } else { 2 };
        self.metadata_workspace
            + threads * (self.encoded_bytes + MAX_SUBBLOCK_MASK)
            + decoded * multiplier
            + 2 * frame_bytes
            + FRAME_OVERHEAD
    }
}

fn read(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(bytes))
        .map_err(error)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// The segment kind, allocated size and used size at `offset`.
fn segment(file: &mut File, offset: u64) -> Result<(SegmentId, u64, u64), String> {
    let mut raw = [0; SEGMENT_HEADER_LEN as usize];
    read(file, offset, &mut raw)?;
    Ok((
        SegmentId::parse(&raw[..16]),
        u64_at(&raw, 16),
        u64_at(&raw, 24),
    ))
}

fn metadata_total(total: &mut u64, bytes: u64) -> Result<(), String> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= MAX_METADATA_BYTES)
        .ok_or(
            "CZI metadata XML, subblock and attachment directories exceed the 32 MiB input limit.",
        )?;
    Ok(())
}

// Mirrors pinned openreadout-czi 0.3.0 container.rs::CziFile::open_in, which loads
// the metadata XML, both directories, every segment header and small attachments.
// Read only headers and directory entries; pixel payloads are never read here.
fn scan(path: &Path) -> Result<Plan, String> {
    let mut file = File::open(path).map_err(error)?;
    let file_len = file.metadata().map_err(error)?.len();
    let fits =
        |offset: u64, bytes: u64| offset.checked_add(bytes).is_some_and(|end| end <= file_len);
    if !fits(0, SEGMENT_HEADER_LEN + 80) || segment(&mut file, 0)?.0 != SegmentId::File {
        return Err("CZI file does not start with a ZISRAWFILE segment.".into());
    }
    let mut header = [0; 80];
    read(&mut file, SEGMENT_HEADER_LEN, &mut header)?;
    if u32_at(&header, 48) != 0 {
        return Err("Multi-file CZI documents are unsupported. Open a single-file CZI.".into());
    }
    let directory_position = u64_at(&header, 52);
    let metadata_position = u64_at(&header, 60);
    let attachment_position = u64_at(&header, 72);
    let mut metadata_bytes = 0;

    let mut xml_bytes = 0;
    if metadata_position != 0
        && fits(metadata_position, SEGMENT_HEADER_LEN + 8)
        && segment(&mut file, metadata_position)?.0 == SegmentId::Metadata
    {
        let mut size = [0; 4];
        read(&mut file, metadata_position + SEGMENT_HEADER_LEN, &mut size)?;
        let size = u64::from(u32::from_le_bytes(size));
        if fits(metadata_position + SEGMENT_HEADER_LEN + 256, size) {
            xml_bytes = size;
            metadata_total(&mut metadata_bytes, size)?;
        }
    }

    // The reader falls back to scanning every subblock when its directory is
    // unusable. Require the directory so planning stays bounded.
    let (kind, _, directory_bytes) =
        if directory_position != 0 && fits(directory_position, SEGMENT_HEADER_LEN) {
            segment(&mut file, directory_position)?
        } else {
            (SegmentId::Unknown, 0, 0)
        };
    if kind != SegmentId::Directory
        || directory_bytes < 128
        || !fits(directory_position + SEGMENT_HEADER_LEN, directory_bytes)
    {
        return Err("CZI requires an intact subblock directory for bounded decoding; rescue scanning is unsupported.".into());
    }
    metadata_total(&mut metadata_bytes, directory_bytes)?;
    let mut directory = BufReader::new(File::open(path).map_err(error)?);
    directory
        .seek(SeekFrom::Start(directory_position + SEGMENT_HEADER_LEN))
        .map_err(error)?;
    let mut prefix = [0; 128];
    directory.read_exact(&mut prefix).map_err(error)?;
    let count = u32_at(&prefix, 0);
    let mut consumed = 128u64;
    let mut pixel_type = None;
    let mut images = BTreeSet::new();
    let (mut stored_tile_bytes, mut encoded_bytes, mut jpeg) = (0u64, 0u64, false);
    for _ in 0..count {
        let mut raw = [0; 32];
        if consumed + 32 > directory_bytes {
            return Err("CZI subblock directory is truncated.".into());
        }
        directory.read_exact(&mut raw).map_err(error)?;
        let dimension_count = u32_at(&raw, 28) as u64;
        if &raw[..2] != b"DV"
            || dimension_count > 32
            || consumed + 32 + 20 * dimension_count > directory_bytes
        {
            return Err("CZI subblock directory has a malformed entry.".into());
        }
        consumed += 32 + 20 * dimension_count;
        let mut dimensions = Vec::with_capacity(dimension_count as usize);
        for _ in 0..dimension_count {
            let mut dimension = [0; 20];
            directory.read_exact(&mut dimension).map_err(error)?;
            dimensions.push(DimensionEntry {
                dimension: dimension[0] as char,
                start: u32_at(&dimension, 4) as i32,
                size: u32_at(&dimension, 8),
                start_coordinate: 0.0,
                stored_size: u32_at(&dimension, 16),
            });
        }
        let entry = DirectoryEntry {
            pixel_type: PixelTypeId::from_raw(u32_at(&raw, 2)),
            file_position: u64_at(&raw, 6),
            file_part: u32_at(&raw, 14),
            compression: CompressionId::from_raw(u32_at(&raw, 18)),
            pyramid_type: raw[22],
            dimensions,
        };
        if entry.file_part != 0 {
            return Err("Multi-file CZI documents are unsupported. Open a single-file CZI.".into());
        }
        if !matches!(
            entry.pixel_type,
            PixelTypeId::Gray8
                | PixelTypeId::Gray16
                | PixelTypeId::Bgr24
                | PixelTypeId::Bgr48
                | PixelTypeId::Bgra32
        ) {
            return Err(format!("CZI pixel type {} is unsupported. Aperture reads unsigned 8/16-bit grayscale and 8/16-bit BGR/BGRA subblocks.", entry.pixel_type.name()));
        }
        if pixel_type.get_or_insert(entry.pixel_type) != &entry.pixel_type {
            return Err("CZI subblocks with mixed pixel types are unsupported.".into());
        }
        if !entry.is_level0() {
            continue;
        }
        // The reader's image grouping: scene, H/I/R/V/B coordinates and stored ratio.
        let mut extra: Vec<(char, i32)> = entry
            .dimensions
            .iter()
            .filter(|d| !"XYZCTSM".contains(d.dimension))
            .map(|d| (d.dimension, d.start))
            .collect();
        extra.sort_unstable();
        extra.dedup_by_key(|(d, _)| *d);
        images.insert((
            entry.index('S'),
            extra,
            (entry.stored_ratio() * 1e6).round() as i64,
        ));
        if images.len() > MAX_IMAGES {
            return Err(format!("CZI supports at most {MAX_IMAGES} scenes."));
        }
        let stored = ['X', 'Y', 'Z', 'C', 'T']
            .iter()
            .try_fold(u64::from(entry.pixel_type.bytes_per_pixel()), |n, d| {
                let size = entry.dim(*d).map_or(1, |x| {
                    if matches!(d, 'X' | 'Y') {
                        x.stored_size
                    } else {
                        x.size.max(1)
                    }
                });
                n.checked_mul(u64::from(size))
            })
            .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
            .ok_or("CZI subblock exceeds the 256 MiB decoded-tile limit.")?;
        stored_tile_bytes = stored_tile_bytes.max(stored);
        jpeg |= entry.compression == CompressionId::Jpeg;
        // The reader allocates the payload size from the subblock header.
        let mut sizes = [0; 16];
        if !fits(entry.file_position, SEGMENT_HEADER_LEN + 16)
            || segment(&mut file, entry.file_position)?.0 != SegmentId::SubBlock
        {
            return Err("CZI directory entry does not reference a subblock.".into());
        }
        read(
            &mut file,
            entry.file_position + SEGMENT_HEADER_LEN,
            &mut sizes,
        )?;
        let data_bytes = u64_at(&sizes, 8);
        let data_offset = entry.file_position
            + SEGMENT_HEADER_LEN
            + (16 + entry.encoded_len() as u64).max(256)
            + u64::from(u32_at(&sizes, 0));
        if data_bytes > MAX_FRAME_BYTES + MIB {
            return Err("CZI encoded subblock exceeds the 257 MiB input-buffer limit.".into());
        }
        if !fits(data_offset, data_bytes) {
            return Err("CZI subblock extends beyond its source file.".into());
        }
        encoded_bytes = encoded_bytes.max(data_bytes);
    }

    let (mut attachment_bytes, mut attachments, mut interpreted_bytes) = (0, 0, 0);
    if attachment_position != 0 && fits(attachment_position, SEGMENT_HEADER_LEN) {
        let (kind, _, used) = segment(&mut file, attachment_position)?;
        if kind == SegmentId::AttachmentDirectory
            && used >= 4
            && fits(attachment_position + SEGMENT_HEADER_LEN, used)
        {
            attachment_bytes = used;
            metadata_total(&mut metadata_bytes, used)?;
            let start = attachment_position + SEGMENT_HEADER_LEN;
            let mut count = [0; 4];
            read(&mut file, start, &mut count)?;
            for index in 0..u64::from(u32::from_le_bytes(count)) {
                let offset = 256 + 128 * index;
                let mut raw = [0; 128];
                if offset + 128 > used {
                    break;
                }
                read(&mut file, start + offset, &mut raw)?;
                if &raw[..2] != b"A1" {
                    break;
                }
                attachments += 1;
                let kind = String::from_utf8_lossy(&raw[40..48])
                    .trim_end_matches('\0')
                    .to_ascii_uppercase();
                let position = u64_at(&raw, 12);
                // Missing parts fall back to the master; count every candidate there.
                if matches!(kind.as_str(), "CZTIMS" | "CZEVL")
                    && fits(position, SEGMENT_HEADER_LEN + 4)
                    && segment(&mut file, position)?.0 == SegmentId::Attachment
                {
                    let mut size = [0; 4];
                    read(&mut file, position + SEGMENT_HEADER_LEN, &mut size)?;
                    let size = u64::from(u32::from_le_bytes(size));
                    if size <= MAX_INTERPRETED_ATTACHMENT {
                        interpreted_bytes += size;
                        metadata_total(&mut metadata_bytes, size)?;
                    }
                }
            }
        }
    }

    // The reader keeps one header per segment from a sequential walk.
    let mut segments = 0u64;
    let mut offset = 0u64;
    while fits(offset, SEGMENT_HEADER_LEN) {
        let (kind, allocated, _) = segment(&mut file, offset)?;
        if kind == SegmentId::Unknown {
            break;
        }
        segments += 1;
        match offset
            .checked_add(SEGMENT_HEADER_LEN + allocated)
            .filter(|end| allocated > 0 && *end <= file_len)
        {
            Some(end) => offset = end,
            None => break,
        }
    }

    // XML is copied and parsed into a DOM; directory entries are expanded, grouped
    // into scenes and cloned for each plane read. Allow generous per-byte factors.
    let metadata_workspace = xml_bytes * 32
        + directory_bytes * 8
        + u64::from(count) * 512
        + attachment_bytes * 4
        + attachments * 256
        + interpreted_bytes * 16
        + segments * 64
        + 8 * MIB;
    Ok(Plan {
        metadata_workspace,
        interpreted_bytes,
        images: images.len(),
        stored_tile_bytes,
        encoded_bytes,
        jpeg,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::frame_cache::Cache;
    use crate::frames::tests::{strip, temporary};
    use crate::frames::{self, Format, Projection, RawFrame, Selection};
    use openreadout_core::FormatReader;
    use std::sync::atomic::AtomicBool;

    /// One subblock: directory pixel type, its dimensions (name, start, size,
    /// stored size) and stored samples.
    pub(crate) struct Block {
        pub pixel_type: u32,
        pub dimensions: Vec<(char, i32, u32, u32)>,
        pub data: Vec<u8>,
    }

    pub(crate) fn block(pixel_type: u32, dims: &[(char, i32, u32)], data: Vec<u8>) -> Block {
        Block {
            pixel_type,
            dimensions: dims
                .iter()
                .map(|&(d, start, size)| (d, start, size, size))
                .collect(),
            data,
        }
    }

    fn segment(out: &mut Vec<u8>, id: &str, payload: &[u8]) -> u64 {
        let offset = out.len() as u64;
        let mut name = [0; 16];
        name[..id.len()].copy_from_slice(id.as_bytes());
        out.extend(name);
        out.extend((payload.len() as u64).to_le_bytes());
        out.extend((payload.len() as u64).to_le_bytes());
        out.extend(payload);
        offset
    }

    fn entry(block: &Block, position: u64, file_part: u32) -> Vec<u8> {
        let mut raw = b"DV".to_vec();
        raw.extend(block.pixel_type.to_le_bytes());
        raw.extend(position.to_le_bytes());
        raw.extend(file_part.to_le_bytes());
        raw.extend(0u32.to_le_bytes()); // uncompressed
        raw.extend([0; 6]); // pyramid type and reserved bytes
        raw.extend((block.dimensions.len() as u32).to_le_bytes());
        for &(dimension, start, size, stored) in &block.dimensions {
            raw.extend([dimension as u8, 0, 0, 0]);
            raw.extend(start.to_le_bytes());
            raw.extend(size.to_le_bytes());
            raw.extend(0f32.to_le_bytes());
            raw.extend(stored.to_le_bytes());
        }
        raw
    }

    /// A minimal single-file ZISRAWFILE: header, metadata XML, uncompressed
    /// subblocks and their directory, laid out as pinned openreadout-czi reads them.
    pub(crate) fn synthetic_parts(blocks: &[Block], xml: &str, file_part: u32) -> Vec<u8> {
        let mut out = Vec::new();
        segment(&mut out, "ZISRAWFILE", &[0; 512]);
        let mut metadata = vec![0; 256];
        metadata[..4].copy_from_slice(&(xml.len() as u32).to_le_bytes());
        metadata.extend(xml.as_bytes());
        let metadata_position = segment(&mut out, "ZISRAWMETADATA", &metadata);
        let mut directory = vec![0; 128];
        directory[..4].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
        for block in blocks {
            let position = out.len() as u64;
            let described = entry(block, position, file_part);
            let mut payload = vec![0; 16];
            payload[8..16].copy_from_slice(&(block.data.len() as u64).to_le_bytes());
            payload.extend(&described);
            payload.resize((16 + described.len()).max(256), 0);
            payload.extend(&block.data);
            segment(&mut out, "ZISRAWSUBBLOCK", &payload);
            directory.extend(described);
        }
        let directory_position = segment(&mut out, "ZISRAWDIRECTORY", &directory);
        let header = SEGMENT_HEADER_LEN as usize;
        out[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
        out[header + 52..header + 60].copy_from_slice(&directory_position.to_le_bytes());
        out[header + 60..header + 68].copy_from_slice(&metadata_position.to_le_bytes());
        out
    }

    pub(crate) fn synthetic(blocks: &[Block], xml: &str) -> Vec<u8> {
        synthetic_parts(blocks, xml, 0)
    }

    pub(crate) fn xml(channels: &[(&str, &str)], scenes: &[&str]) -> String {
        let channels: String = channels
            .iter()
            .enumerate()
            .map(|(i, (name, color))| {
                format!(
                    r#"<Channel Id="Channel:{i}" Name="{name}"><Color>{color}</Color></Channel>"#
                )
            })
            .collect();
        let scenes: String = scenes
            .iter()
            .enumerate()
            .map(|(i, name)| format!(r#"<Scene Index="{i}" Name="{name}"/>"#))
            .collect();
        format!(
            r#"<ImageDocument><Metadata><Information><Image><Dimensions><Channels>{channels}</Channels><S><Scenes>{scenes}</Scenes></S><T><Positions><Interval><Increment>2.5</Increment></Interval></Positions></T></Dimensions></Image></Information><Scaling><Items><Distance Id="X"><Value>2.5E-07</Value></Distance><Distance Id="Y"><Value>2.5E-07</Value></Distance><Distance Id="Z"><Value>1.5E-06</Value></Distance></Items></Scaling></Metadata></ImageDocument>"#
        )
    }

    fn sample(scene: u16, z: u16, time: u16, channel: u16, x: u16) -> u16 {
        1000 + 256 * scene + 64 * z + 16 * time + 4 * channel + x
    }

    /// Two 2×1 gray16 scenes with Z=2, T=2, C=2 and distinct original samples.
    fn stack() -> Vec<u8> {
        let mut blocks = Vec::new();
        for (s, z, t, c) in coordinates(2, 2, 2, 2) {
            let data = (0..2)
                .flat_map(|x| sample(s, z, t, c, x).to_le_bytes())
                .collect();
            blocks.push(block(
                1,
                &[
                    ('X', 0, 2),
                    ('Y', 0, 1),
                    ('C', c.into(), 1),
                    ('Z', z.into(), 1),
                    ('T', t.into(), 1),
                    ('S', s.into(), 1),
                ],
                data,
            ));
        }
        synthetic(
            &blocks,
            &xml(
                &[("DAPI", "#FF0000FF"), ("EGFP", "#FF00FF00")],
                &["Well A1", "Well B2"],
            ),
        )
    }

    fn coordinates(s: u16, z: u16, t: u16, c: u16) -> Vec<(u16, u16, u16, u16)> {
        let mut all = Vec::new();
        for scene in 0..s {
            for zi in 0..z {
                for ti in 0..t {
                    for ci in 0..c {
                        all.push((scene, zi, ti, ci));
                    }
                }
            }
        }
        all
    }

    /// The validated reader path without the shared-budget cache.
    pub(crate) fn raw(path: &Path, selection: Selection) -> Result<RawFrame, String> {
        let plan = scan(path)?;
        let mut dataset = openreadout_czi::CziReader.open(path).map_err(error)?;
        let images = dataset.info().map_err(error)?.images;
        plan.check_images(&images)?;
        frames::decode_frame(Format::Czi, dataset.as_mut(), &images, selection)
    }

    fn written(bytes: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let temp = temporary();
        let source = temp.join("synthetic.czi");
        std::fs::write(&source, bytes).unwrap();
        (temp, source)
    }

    fn samples(values: &[u16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn maps_scenes_z_time_and_channels_to_exact_original_samples() {
        let bytes = stack();
        let (temp, source) = written(&bytes);
        for (s, z, t) in [(0, 0, 0), (1, 1, 0), (0, 1, 1), (1, 1, 1)] {
            let frame = raw(
                &source,
                Selection {
                    position: s.into(),
                    z: z.into(),
                    time: t.into(),
                },
            )
            .unwrap();
            assert_eq!(
                (frame.width, frame.height, frame.bands, frame.bytes),
                (2, 1, 2, 2)
            );
            assert_eq!(
                frame.pixels,
                samples(&[
                    sample(s, z, t, 0, 0),
                    sample(s, z, t, 1, 0),
                    sample(s, z, t, 0, 1),
                    sample(s, z, t, 1, 1),
                ])
            );
            let metadata = frame.metadata;
            assert_eq!(metadata.dimensions.positions, ["Well A1", "Well B2"]);
            assert_eq!((metadata.dimensions.z, metadata.dimensions.time), (2, 2));
            assert_eq!(
                metadata
                    .channels
                    .iter()
                    .map(|c| (&*c.name, &*c.color))
                    .collect::<Vec<_>>(),
                [("DAPI", "#0000FF"), ("EGFP", "#00FF00")]
            );
            let calibration = metadata.calibration.unwrap();
            assert_eq!(calibration.pixel_width_um, Some(0.25));
            assert_eq!(calibration.pixel_height_um, Some(0.25));
            assert_eq!(calibration.z_step_um, Some(1.5));
            assert_eq!(calibration.time_increment_seconds, Some(2.5));
        }
        // The shared-budget cache normalizes each 16-bit channel for display only.
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Czi, source.clone(), Arc::clone(&budget)).unwrap();
        let output = temp.join("display.tif");
        let selection = Selection {
            position: 1,
            z: 1,
            time: 0,
        };
        let result = cache.extract_display(&output, selection).unwrap();
        let first = sample(1, 1, 0, 0, 0);
        let second = sample(1, 1, 0, 1, 0);
        assert_eq!(
            result.display_ranges,
            [[first, first + 1], [second, second + 1]]
        );
        assert_eq!(strip(&output), [0, 0, 255, 255]);
        assert!(cache.geometry(selection).unwrap().decode_peak_bytes > 2 * 8 + 8 * MIB);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn bgr_subblocks_are_returned_in_red_green_blue_order() {
        let temp = temporary();
        for (pixel_type, stored, expected) in [
            // Bgr24, two pixels stored as B,G,R.
            (
                3,
                vec![10u8, 20, 30, 40, 50, 60],
                vec![30u8, 20, 10, 60, 50, 40],
            ),
            // Bgra32 drops alpha.
            (
                9,
                vec![10, 20, 30, 255, 40, 50, 60, 128],
                vec![30, 20, 10, 60, 50, 40],
            ),
            // Bgr48, little-endian samples.
            (
                4,
                samples(&[1, 2, 3, 4, 5, 6]),
                samples(&[3, 2, 1, 6, 5, 4]),
            ),
        ] {
            let source = temp.join(format!("rgb-{pixel_type}.czi"));
            std::fs::write(
                &source,
                synthetic(
                    &[block(
                        pixel_type,
                        &[('X', 0, 2), ('Y', 0, 1), ('C', 0, 1)],
                        stored,
                    )],
                    &xml(&[("Brightfield", "#FFFFFFFF")], &[]),
                ),
            )
            .unwrap();
            let frame = raw(&source, Selection::default()).unwrap();
            assert!(frame.rgb);
            assert_eq!(frame.pixels, expected);
            assert_eq!(
                frame
                    .metadata
                    .channels
                    .iter()
                    .map(|c| &*c.name)
                    .collect::<Vec<_>>(),
                ["Red", "Green", "Blue"]
            );
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn stitches_mosaic_tiles_into_one_frame() {
        let tiles = [(0, [1u8, 2]), (2, [3, 4])].map(|(x, data)| {
            block(
                0,
                &[('X', x, 2), ('Y', 0, 1), ('C', 0, 1), ('M', x / 2, 1)],
                data.to_vec(),
            )
        });
        let (temp, source) = written(&synthetic(&tiles, &xml(&[], &[])));
        let frame = raw(&source, Selection::default()).unwrap();
        assert_eq!((frame.width, frame.height), (4, 1));
        assert_eq!(frame.pixels, [1, 2, 3, 4]);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn projects_z_from_original_samples_per_scene() {
        let (temp, source) = written(&stack());
        let budget = Budget::new(temp.join("budget")).unwrap();
        let mut cache = Cache::new(Format::Czi, source, Arc::clone(&budget)).unwrap();
        let output = temp.join("projection.tif");
        let selection = Selection {
            position: 1,
            z: 0,
            time: 1,
        };
        let result = cache
            .extract_projection(
                &output,
                selection,
                Projection::Z,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        // Z=1 has the larger samples for every channel and pixel.
        let first = sample(1, 1, 1, 0, 0);
        let second = sample(1, 1, 1, 1, 0);
        assert_eq!(
            result.display_ranges,
            [[first, first + 1], [second, second + 1]]
        );
        assert_eq!(strip(&output), [0, 0, 255, 255]);
        assert_eq!(
            (result.dimensions.position, result.dimensions.time_index),
            (1, 1)
        );
        drop(cache);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn scenes_with_different_xy_sizes_keep_their_own_geometry() {
        let blocks = [(0, 2u32), (1, 3)].map(|(scene, width)| {
            block(
                0,
                &[('X', 0, width), ('Y', 0, 1), ('S', scene, 1)],
                vec![7; width as usize],
            )
        });
        let (temp, source) = written(&synthetic(&blocks, &xml(&[], &["A", "B"])));
        let budget = Budget::new(temp.join("budget")).unwrap();
        let cache = Cache::new(Format::Czi, source, Arc::clone(&budget)).unwrap();
        let widths = [0, 1].map(|position| {
            cache
                .geometry(Selection {
                    position,
                    ..Selection::default()
                })
                .unwrap()
                .width
        });
        assert_eq!(widths, [2, 3]);
        drop(cache);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn rejects_unsupported_layouts_before_or_after_reader_metadata() {
        let temp = temporary();
        let plain = |pixel_type, scene| {
            block(
                pixel_type,
                &[('X', 0, 1), ('Y', 0, 1), ('S', scene, 1)],
                vec![0; 4],
            )
        };
        let channels = |count: i32, scene| {
            (0..count).map(move |c| {
                block(
                    0,
                    &[('X', 0, 1), ('Y', 0, 1), ('C', c, 1), ('S', scene, 1)],
                    vec![0],
                )
            })
        };
        let far =
            [(0, 0), (20000, 20000)].map(|(x, y)| block(0, &[('X', x, 1), ('Y', y, 1)], vec![0]));
        for (name, bytes, message) in [
            (
                "float",
                synthetic(&[plain(2, 0)], &xml(&[], &[])),
                "gray32_float is unsupported",
            ),
            (
                "mixed",
                synthetic(&[plain(0, 0), plain(1, 1)], &xml(&[], &[])),
                "mixed pixel types",
            ),
            (
                "inconsistent",
                synthetic(
                    &channels(2, 0).chain(channels(1, 1)).collect::<Vec<_>>(),
                    &xml(&[], &[]),
                ),
                "scenes differ",
            ),
            (
                "oversized",
                synthetic(&far, &xml(&[], &[])),
                "20001 × 20001 pixels",
            ),
            (
                "multi-file",
                synthetic_parts(&[plain(0, 0)], &xml(&[], &[]), 1),
                "Multi-file",
            ),
        ] {
            let source = temp.join(format!("{name}.czi"));
            std::fs::write(&source, &bytes).unwrap();
            let budget = Budget::new(temp.join(format!("budget-{name}"))).unwrap();
            let error = Cache::new(Format::Czi, source.clone(), Arc::clone(&budget))
                .err()
                .unwrap();
            assert!(error.contains(message), "{name}: {error}");
            assert_eq!(budget.status().unwrap().used_bytes, 0);
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
        }
        // Oversized XML and a missing directory stop before the reader allocates.
        let mut bytes = synthetic(&[plain(0, 0)], &xml(&[], &[]));
        let metadata = u64_at(&bytes, SEGMENT_HEADER_LEN as usize + 60) as usize;
        let mut large = bytes.clone();
        large[metadata + 32..metadata + 36].copy_from_slice(&(33 * MIB as u32).to_le_bytes());
        large.resize(large.len() + 33 * MIB as usize, 0);
        let source = temp.join("large-xml.czi");
        std::fs::write(&source, large).unwrap();
        assert!(scan(&source).err().unwrap().contains("32 MiB"));
        let header = SEGMENT_HEADER_LEN as usize;
        bytes[header + 52..header + 60].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&source, bytes).unwrap();
        assert!(scan(&source)
            .err()
            .unwrap()
            .contains("intact subblock directory"));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn backend_browses_czi_read_only_with_scene_geometry() {
        use crate::backend::Backend;
        use std::time::{Duration, Instant};

        fn ready(backend: &Backend) -> crate::backend::ImageState {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let state = backend.snapshot().unwrap();
                if state.phase != "loading" {
                    assert_eq!(state.phase, "ready", "{state:?}");
                    return state;
                }
                assert!(Instant::now() < deadline, "{state:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // Same native dependency requirement as the backend's TIFF regressions.
        if std::process::Command::new("vips")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let blocks = [(0, 2u32), (1, 3)].map(|(scene, width)| {
            block(
                0,
                &[('X', 0, width), ('Y', 0, 1), ('S', scene, 1)],
                (1..=width as u8).collect(),
            )
        });
        let bytes = synthetic(&blocks, &xml(&[("DAPI", "#FFFFFFFF")], &["A", "B"]));
        let (temp, source) = written(&bytes);
        assert!(crate::backend::folder_images(&source)
            .unwrap()
            .contains(&source.to_str().unwrap().to_owned()));
        let backend = Backend::new(temp.join("cache")).unwrap();
        backend.open(&source).unwrap();
        let first = ready(&backend);
        assert!(first.read_only && first.channel_mode && first.preview_ready);
        assert_eq!((first.width, first.height), (2, 1));
        assert_eq!(first.dimensions.as_ref().unwrap().positions, ["A", "B"]);
        backend
            .select_plane(
                &first.id,
                Selection {
                    position: 1,
                    ..Selection::default()
                },
            )
            .unwrap();
        let second = ready(&backend);
        assert_eq!((second.width, second.height), (3, 1));
        assert_eq!(second.dimensions.as_ref().unwrap().position, 1);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        backend.shutdown();
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    #[ignore = "requires public fixtures in test-data/czi; parent operator runs this check"]
    fn public_czi_files_match_independent_czifile_samples() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/czi");
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixtures.join("expected.json")).unwrap())
                .unwrap();
        let index =
            |value: &serde_json::Value, axis: &str| value[axis].as_u64().unwrap_or(0) as u32;
        let (mut checked, mut rejected) = (0, 0);
        for (name, file) in expected["files"].as_object().unwrap() {
            let path = fixtures.join(name);
            let scenes = file["scenes"].as_array().unwrap();
            for scene in scenes {
                let position = scene["position"].as_u64().unwrap() as u32;
                let sizes = &scene["sizes"];
                let (width, height) = (index(sizes, "X"), index(sizes, "Y"));
                let rgb = scene["samplesPerPixel"].as_u64() == Some(3);
                let channels = if rgb { 1 } else { index(sizes, "C").max(1) };
                let frame_bytes = scene["planeBytes"].as_u64().unwrap() * u64::from(channels);
                let first = Selection {
                    position,
                    ..Selection::default()
                };
                if frame_bytes > MAX_FRAME_BYTES {
                    let error = raw(&path, first).err().unwrap();
                    assert!(
                        error.contains("256 MiB decoded-frame limit"),
                        "{name}: {error}"
                    );
                    rejected += 1;
                    continue;
                }
                let mut decoded: Option<(Selection, RawFrame)> = None;
                let mut frame = |selection: Selection| {
                    if decoded.as_ref().is_none_or(|(key, _)| *key != selection) {
                        decoded = Some((selection, raw(&path, selection).unwrap()));
                    }
                    let frame = &decoded.as_ref().unwrap().1;
                    let metadata = &frame.metadata;
                    assert_eq!(metadata.dimensions.positions.len(), scenes.len(), "{name}");
                    assert_eq!(metadata.dimensions.z, index(sizes, "Z").max(1), "{name}");
                    assert_eq!(metadata.dimensions.time, index(sizes, "T").max(1), "{name}");
                    assert_eq!(
                        (frame.width, frame.height, frame.rgb),
                        (width, height, rgb),
                        "{name}"
                    );
                    if !rgb {
                        let names: Vec<_> =
                            metadata.channels.iter().map(|c| c.name.as_str()).collect();
                        assert_eq!(
                            names,
                            scene["channelNames"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|n| n.as_str().unwrap())
                                .collect::<Vec<_>>(),
                            "{name}"
                        );
                    }
                    decoded.as_ref().unwrap().1.pixels.clone()
                };
                let selection = |at: &serde_json::Value| Selection {
                    position,
                    z: index(at, "Z"),
                    time: index(at, "T"),
                };
                let bytes = if scene["dtype"] == "uint16" { 2 } else { 1 };
                let bands = if rgb { 3 } else { channels as usize };
                let value = |pixels: &[u8], offset: usize| {
                    if bytes == 2 {
                        u64::from(u16::from_le_bytes(
                            pixels[offset..offset + 2].try_into().unwrap(),
                        ))
                    } else {
                        u64::from(pixels[offset])
                    }
                };
                for plane in scene["planes"].as_array().unwrap() {
                    let at = &plane["index"];
                    let pixels = frame(selection(at));
                    let channel = index(at, "C") as usize;
                    for (sample, key) in if rgb {
                        vec![(0, 0), (1, 1), (2, 2)]
                    } else {
                        vec![(channel, 0)]
                    } {
                        let values = (0..width as usize * height as usize)
                            .map(|pixel| value(&pixels, (pixel * bands + sample) * bytes));
                        let (minimum, maximum) =
                            values.fold((u64::MAX, 0), |(lo, hi), v| (lo.min(v), hi.max(v)));
                        let (lo, hi) = if rgb {
                            (&plane["minPerSample"][key], &plane["maxPerSample"][key])
                        } else {
                            (&plane["min"], &plane["max"])
                        };
                        assert_eq!(
                            (minimum, maximum),
                            (lo.as_u64().unwrap(), hi.as_u64().unwrap()),
                            "{name} {at}"
                        );
                    }
                }
                for block in scene["blocks"].as_array().unwrap() {
                    let at = &block["index"];
                    let pixels = frame(selection(at));
                    let channel = index(at, "C") as usize;
                    let (x0, y0) = (
                        block["x"].as_u64().unwrap() as usize,
                        block["y"].as_u64().unwrap() as usize,
                    );
                    for (dy, row) in block["values"].as_array().unwrap().iter().enumerate() {
                        for (dx, expected) in row.as_array().unwrap().iter().enumerate() {
                            let pixel = (y0 + dy) * width as usize + x0 + dx;
                            let actual: Vec<u64> = if rgb {
                                (0..3)
                                    .map(|s| value(&pixels, (pixel * 3 + s) * bytes))
                                    .collect()
                            } else {
                                vec![value(&pixels, (pixel * bands + channel) * bytes)]
                            };
                            let expected: Vec<u64> = expected.as_array().map_or_else(
                                || vec![expected.as_u64().unwrap()],
                                |v| v.iter().map(|n| n.as_u64().unwrap()).collect(),
                            );
                            assert_eq!(
                                actual, expected,
                                "{name} {} {at} ({dx}, {dy})",
                                block["label"]
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        eprintln!("checked {checked} reference pixels; rejected {rejected} oversized scenes");
        assert!(checked > 0 && rejected > 0);
    }
}
