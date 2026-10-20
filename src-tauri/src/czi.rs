//! Zeiss CZI checks before the pinned reader allocates, and CZI decode accounting.

use crate::frame_cache::{reserve_after_reclaim, CheckedSource, FRAME_OVERHEAD, MIB};
use crate::frames::MAX_FRAME_BYTES;
use crate::memory::{Budget, Reservation};
use openreadout_core::ImageInfo;
use openreadout_czi::{
    CompressionId, DimensionEntry, DirectoryEntry, PixelTypeId, SegmentId, SEGMENT_HEADER_LEN,
};
use openreadout_jpegxr::SampleType;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

const MAX_METADATA_BYTES: u64 = 32 * MIB;
const MAX_DIRECTORY_BYTES: u64 = 128 * MIB;
const MAX_IMAGES: usize = 4096;
const JPEG_XR_HEADER_BYTES: u64 = 64 * 1024;
// Pinned openreadout-czi 0.3.0 dataset.rs and attach.rs constants: one plane's
// tiles decode in batches of at most this many output bytes; subblock masks,
// TimeStamps/EventList attachments and listed channels up to these sizes are read.
const TILE_BATCH_BYTES: u64 = 256 * MIB;
const MAX_SUBBLOCK_MASK: u64 = MIB;
const MAX_INTERPRETED_ATTACHMENT: u64 = 64 * MIB;
const MAX_LISTED_CHANNELS: u64 = 1 << 16;

fn error(error: impl std::fmt::Display) -> String {
    format!("CZI: {error}")
}

/// Full-resolution subblocks of one reader image.
#[derive(Clone, Copy)]
struct Tiles {
    count: u64,
    /// The smallest decoded tile bounds the reader's largest batch.
    min_output_bytes: u64,
    /// Payload, mask and decode copies of the largest tile.
    max_workspace: u64,
}

/// CZI bounds validated before the reader allocates.
pub(crate) struct Plan {
    metadata_workspace: u64,
    xml: (u64, u64),
    /// Time stamps, events and listed channels copied into every image by info().
    image_copies: u64,
    /// In the reader's image order: scene, H/I/R/V/B coordinates, stored ratio.
    images: Vec<Tiles>,
}

impl Plan {
    /// Returns the plan and a reservation covering the reader's opening peak.
    pub(crate) fn new(
        source: &CheckedSource,
        budget: &Arc<Budget>,
    ) -> Result<(Self, Reservation), String> {
        let plan = scan(&source.stable_path())?;
        source.check().map_err(|e| e.to_string())?;
        let strings = {
            let _parser = reserve_after_reclaim(budget, plan.xml.1 * 32 + 8 * MIB, false)?
                .ok_or("CZI metadata parsing does not fit the application memory budget.")?;
            let strings = xml_strings(&source.stable_path(), plan.xml)?;
            source.check().map_err(|e| e.to_string())?;
            strings
        };
        let metadata = reserve_after_reclaim(budget, plan.opening_bytes(strings), false)?
            .ok_or("CZI metadata parsing does not fit the application memory budget.")?;
        Ok((plan, metadata))
    }

    /// info() clones XML-derived strings and interpreted attachments into every image.
    fn opening_bytes(&self, xml_strings: u64) -> u64 {
        let per_image = 2 * xml_strings + self.image_copies + 16 * 1024;
        self.metadata_workspace + per_image * (self.images.len() as u64 + 1)
    }

    pub(crate) fn check_images(&self, images: &[ImageInfo]) -> Result<(), String> {
        let first = images
            .first()
            .ok_or("CZI contains no readable full-resolution image.")?;
        if images.len() != self.images.len() {
            return Err(
                "CZI decoded images disagree with its validated subblock directory.".into(),
            );
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

    pub(crate) fn decode_peak(&self, position: u32, frame_bytes: u64) -> u64 {
        // Pinned openreadout-czi 0.3.0 reopens per frame, then decodes a plane's
        // tiles on the rayon pool in batches of min(threads, TILE_BATCH_BYTES /
        // largest plane tile), at least one and never more than the plane's tiles.
        // The reader's plane and our assembled frame coexist.
        let tiles = self.images[position as usize];
        let batch = (rayon::current_num_threads() as u64)
            .min(tiles.count)
            .min((TILE_BATCH_BYTES / tiles.min_output_bytes.max(1)).max(1));
        self.metadata_workspace + batch * tiles.max_workspace + 2 * frame_bytes + FRAME_OVERHEAD
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
        .ok_or("CZI metadata XML and attachments exceed the 32 MiB input limit.")?;
    Ok(())
}

/// Strings image_info() can copy from the parsed metadata XML into one image.
fn xml_strings(path: &Path, (offset, bytes): (u64, u64)) -> Result<u64, String> {
    if bytes == 0 {
        return Ok(0);
    }
    let mut raw = vec![0; bytes as usize];
    read(&mut File::open(path).map_err(error)?, offset, &mut raw)?;
    let Some(xml) =
        openreadout_czi::xml::parse_image_xml(String::from_utf8_lossy(&raw).trim_end_matches('\0'))
    else {
        return Ok(0);
    };
    let text = |value: &Option<String>| value.as_ref().map_or(0, |s| s.len() as u64);
    let channels: u64 = xml
        .channels
        .iter()
        .flat_map(|c| {
            [
                &c.id,
                &c.channel_name,
                &c.color_argb,
                &c.fluor,
                &c.acquisition_mode,
                &c.illumination_type,
                &c.contrast_method,
                &c.dye_name,
                &c.detector_id,
            ]
        })
        .map(text)
        .sum();
    // Each image copies only its own scene's name and well.
    let scene = xml
        .scenes
        .values()
        .map(|s| text(&s.scene_name) + text(&s.well_name) + text(&s.well_id))
        .chain(xml.scene_names.values().map(|name| name.len() as u64))
        .max()
        .unwrap_or(0);
    let experiment = xml.experiment.as_ref().map_or(0, |e| {
        text(&e.experiment_version) + e.active_setups.iter().map(|s| s.len() as u64).sum::<u64>()
    });
    let global = [
        &xml.pixel_type,
        &xml.acquisition_time,
        &xml.objective.objective_name,
        &xml.objective.immersion,
        &xml.microscope_name,
        &xml.user_name,
        &xml.application_name,
        &xml.application_version,
    ]
    .into_iter()
    .map(text)
    .sum::<u64>()
        + xml.detectors.values().map(|d| d.len() as u64).sum::<u64>();
    Ok(channels + scene + experiment + global)
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
        return Err(
            "Multi-file CZI documents and part references are unsupported. Open a single-file CZI."
                .into(),
        );
    }
    let directory_position = u64_at(&header, 52);
    let metadata_position = u64_at(&header, 60);
    let attachment_position = u64_at(&header, 72);
    let mut metadata_bytes = 0;

    let mut xml = (0, 0);
    if metadata_position != 0
        && fits(metadata_position, SEGMENT_HEADER_LEN + 8)
        && segment(&mut file, metadata_position)?.0 == SegmentId::Metadata
    {
        let mut size = [0; 4];
        read(&mut file, metadata_position + SEGMENT_HEADER_LEN, &mut size)?;
        let size = u64::from(u32::from_le_bytes(size));
        let offset = metadata_position + SEGMENT_HEADER_LEN + 256;
        if fits(offset, size) {
            xml = (offset, size);
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
    if directory_bytes > MAX_DIRECTORY_BYTES {
        return Err("CZI subblock directory exceeds the 128 MiB input limit.".into());
    }
    let mut directory = BufReader::new(File::open(path).map_err(error)?);
    directory
        .seek(SeekFrom::Start(directory_position + SEGMENT_HEADER_LEN))
        .map_err(error)?;
    let mut prefix = [0; 128];
    directory.read_exact(&mut prefix).map_err(error)?;
    let count = u32_at(&prefix, 0);
    let mut consumed = 128u64;
    let mut pixel_type = None;
    let mut images = BTreeMap::new();
    let mut channel_spans = BTreeMap::new();
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
            return Err("Multi-file CZI documents and part references are unsupported. Open a single-file CZI.".into());
        }
        // Samples per pixel and sample type the reader returns for each tile.
        let (samples, sample) = match entry.pixel_type {
            PixelTypeId::Gray8 => (1, SampleType::U8),
            PixelTypeId::Gray16 => (1, SampleType::U16),
            PixelTypeId::Bgr24 | PixelTypeId::Bgra32 => (3, SampleType::U8),
            PixelTypeId::Bgr48 => (3, SampleType::U16),
            other => return Err(format!("CZI pixel type {} is unsupported. Aperture reads unsigned 8/16-bit grayscale and 8/16-bit BGR/BGRA subblocks.", other.name())),
        };
        if pixel_type.get_or_insert(entry.pixel_type) != &entry.pixel_type {
            return Err("CZI subblocks with mixed pixel types are unsupported.".into());
        }
        if !entry.is_level0() {
            continue;
        }
        // The reader's image grouping, in its order: scene, H/I/R/V/B coordinates
        // and stored ratio.
        let mut extra: Vec<(char, i32)> = entry
            .dimensions
            .iter()
            .filter(|d| !"XYZCTSM".contains(d.dimension))
            .map(|d| (d.dimension, d.start))
            .collect();
        extra.sort_unstable();
        extra.dedup_by_key(|(d, _)| *d);
        let key = (
            entry.index('S'),
            extra,
            (entry.stored_ratio() * 1e6).round() as i64,
        );
        if !images.contains_key(&key) && images.len() == MAX_IMAGES {
            return Err(format!("CZI supports at most {MAX_IMAGES} scenes."));
        }
        let channels = entry.dim('C').map_or(1, |c| i64::from(c.size.max(1)));
        let span = channel_spans
            .entry(entry.index('S'))
            .or_insert((i64::MAX, i64::MIN));
        span.0 = span.0.min(i64::from(entry.index('C')));
        span.1 = span.1.max(i64::from(entry.index('C')) + channels - 1);
        let (width, height) = (
            entry.dim('X').map_or(1, |x| x.stored_size),
            entry.dim('Y').map_or(1, |y| y.stored_size),
        );
        let pixels = ['Z', 'C', 'T']
            .iter()
            .try_fold(u64::from(width) * u64::from(height), |n, d| {
                n.checked_mul(u64::from(entry.dim(*d).map_or(1, |x| x.size.max(1))))
            });
        let stored = pixels
            .and_then(|n| n.checked_mul(u64::from(entry.pixel_type.bytes_per_pixel())))
            .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
            .ok_or("CZI subblock exceeds the 256 MiB decoded-tile limit.")?;
        let output = pixels.unwrap() * samples * sample.bytes() as u64;
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
        let mask = u64::from(u32_at(&sizes, 4));
        let mask = if (32..=MAX_SUBBLOCK_MASK).contains(&mask) {
            mask
        } else {
            0
        };
        // Decoding holds the block and a converted copy. JPEG may decode up to four
        // times its declared size before conforming. JPEG XR is decoded at its own
        // codestream size, so require that to match the directory entry.
        let copies = match entry.compression {
            CompressionId::Jpeg => 5 * stored,
            CompressionId::JpegXr => {
                let mut head = vec![0; data_bytes.min(JPEG_XR_HEADER_BYTES) as usize];
                read(&mut file, data_offset, &mut head)?;
                let info = openreadout_jpegxr::probe(&head)
                    .map_err(|e| format!("CZI JPEG XR subblock header is unreadable: {e}"))?;
                let layout = (info.channels, info.sample);
                if (info.width, info.height) != (width, height)
                    || (layout != (samples as u32, sample)
                        && !(entry.pixel_type == PixelTypeId::Bgra32
                            && layout == (4, SampleType::U8)))
                {
                    return Err("CZI JPEG XR subblock geometry or pixel format disagrees with its directory entry.".into());
                }
                // Decoder output, compaction and conversion, plus two macroblock
                // rows of 32-bit coefficients for up to four channels.
                3 * stored + (u64::from(width).div_ceil(16) + 8) * 2048 * 6
            }
            _ => 2 * stored,
        };
        let tiles = images.entry(key).or_insert(Tiles {
            count: 0,
            min_output_bytes: u64::MAX,
            max_workspace: 0,
        });
        tiles.count += 1;
        tiles.min_output_bytes = tiles.min_output_bytes.min(output);
        tiles.max_workspace = tiles.max_workspace.max(data_bytes + mask + copies);
    }

    let (mut attachment_bytes, mut attachments) = (0, 0);
    let (mut time_stamp_bytes, mut event_bytes) = (0, 0);
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
                let interpreted = match kind.as_str() {
                    "CZTIMS" => &mut time_stamp_bytes,
                    "CZEVL" => &mut event_bytes,
                    _ => continue,
                };
                // Missing parts fall back to the master; count every candidate there.
                if fits(position, SEGMENT_HEADER_LEN + 4)
                    && segment(&mut file, position)?.0 == SegmentId::Attachment
                {
                    let mut size = [0; 4];
                    read(&mut file, position + SEGMENT_HEADER_LEN, &mut size)?;
                    let size = u64::from(u32::from_le_bytes(size));
                    if size <= MAX_INTERPRETED_ATTACHMENT {
                        *interpreted += size;
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
        let end = offset
            .checked_add(SEGMENT_HEADER_LEN)
            .and_then(|start| start.checked_add(allocated))
            .filter(|end| *end <= file_len)
            .ok_or("CZI segment extends beyond its source file.")?;
        if allocated == 0 {
            break;
        }
        offset = end;
    }

    // XML is copied and parsed into a DOM; directory entries are expanded, grouped
    // into scenes and cloned for each plane read. Interpreted attachments are kept
    // as values. Allow generous per-byte factors.
    let interpreted_bytes = time_stamp_bytes + event_bytes;
    let metadata_workspace = xml.1 * 32
        + directory_bytes * 8
        + u64::from(count) * 512
        + attachment_bytes * 4
        + attachments * 256
        + interpreted_bytes * 16
        + segments * 64
        + 8 * MIB;
    // info() lists every channel of a scene's span and expands time stamps and
    // each 20-byte event record into JSON values for every image.
    let channels = channel_spans
        .values()
        .map(|(low, high)| (high - low + 1).clamp(1, MAX_LISTED_CHANNELS as i64) as u64)
        .max()
        .unwrap_or(1);
    Ok(Plan {
        metadata_workspace,
        xml,
        image_copies: channels * 512 + time_stamp_bytes * 8 + event_bytes * 64,
        images: images.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_cache::Cache;
    use crate::frames::tests::{strip, temporary};
    use crate::frames::{self, Format, Projection, RawFrame, Selection};
    use openreadout_core::FormatReader;
    use std::sync::atomic::AtomicBool;

    /// One full-resolution subblock: directory pixel type and compression, its
    /// dimensions (name, start, size) and stored samples.
    struct Block {
        pixel_type: u32,
        compression: u32,
        dimensions: Vec<(char, i32, u32)>,
        data: Vec<u8>,
    }

    fn block(pixel_type: u32, dims: &[(char, i32, u32)], data: Vec<u8>) -> Block {
        Block {
            pixel_type,
            compression: 0,
            dimensions: dims.to_vec(),
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
        raw.extend(block.compression.to_le_bytes());
        raw.extend([0; 6]); // pyramid type and reserved bytes
        raw.extend((block.dimensions.len() as u32).to_le_bytes());
        for &(dimension, start, size) in &block.dimensions {
            raw.extend([dimension as u8, 0, 0, 0]);
            raw.extend(start.to_le_bytes());
            raw.extend(size.to_le_bytes());
            raw.extend(0f32.to_le_bytes());
            raw.extend(size.to_le_bytes());
        }
        raw
    }

    /// A minimal single-file ZISRAWFILE: header, metadata XML, uncompressed
    /// subblocks and their directory, laid out as pinned openreadout-czi reads them.
    fn synthetic_parts(blocks: &[Block], xml: &str, file_part: u32) -> Vec<u8> {
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

    fn synthetic(blocks: &[Block], xml: &str) -> Vec<u8> {
        synthetic_parts(blocks, xml, 0)
    }

    fn xml(channels: &[(&str, &str)], scenes: &[&str]) -> String {
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
    fn raw(path: &Path, selection: Selection) -> Result<RawFrame, String> {
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
        // Only the first position is oversized: browsing cannot start there.
        let first = far
            .iter()
            .map(|tile| {
                let mut dimensions = tile.dimensions.clone();
                dimensions.push(('S', 0, 1));
                block(0, &dimensions, vec![0])
            })
            .chain([block(0, &[('X', 0, 1), ('Y', 0, 1), ('S', 1, 1)], vec![0])])
            .collect::<Vec<_>>();
        let source = temp.join("first-oversized.czi");
        std::fs::write(&source, synthetic(&first, &xml(&[], &[]))).unwrap();
        let budget = Budget::new(temp.join("budget-first")).unwrap();
        let error = Cache::new(Format::Czi, source, budget).err().unwrap();
        assert!(error.contains("starts at the first position"), "{error}");
        // A segment whose end overflows must not wrap the header walk.
        let mut overflowing = synthetic(&[plain(0, 0)], &xml(&[], &[]));
        let mut deleted = [0; 32];
        deleted[..7].copy_from_slice(b"DELETED");
        deleted[16..24].copy_from_slice(&(u64::MAX - 31).to_le_bytes());
        overflowing.extend(deleted);
        let source = temp.join("overflowing-segment.czi");
        std::fs::write(&source, overflowing).unwrap();
        assert!(scan(&source)
            .err()
            .unwrap()
            .contains("segment extends beyond"));
        // Oversized XML and directories and a missing directory stop before the reader allocates.
        let mut bytes = synthetic(&[plain(0, 0)], &xml(&[], &[]));
        let metadata = u64_at(&bytes, SEGMENT_HEADER_LEN as usize + 60) as usize;
        let mut large = bytes.clone();
        large[metadata + 32..metadata + 36].copy_from_slice(&(33 * MIB as u32).to_le_bytes());
        large.resize(large.len() + 33 * MIB as usize, 0);
        let source = temp.join("large-xml.czi");
        std::fs::write(&source, large).unwrap();
        assert!(scan(&source).err().unwrap().contains("32 MiB"));
        let header = SEGMENT_HEADER_LEN as usize;
        let directory = u64_at(&bytes, header + 52) as usize;
        let mut large = bytes.clone();
        large[directory + 24..directory + 32].copy_from_slice(&(129 * MIB).to_le_bytes());
        std::fs::write(&source, large).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(directory as u64 + 32 + 129 * MIB)
            .unwrap();
        assert!(scan(&source).err().unwrap().contains("128 MiB"));
        bytes[header + 52..header + 60].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&source, bytes).unwrap();
        assert!(scan(&source)
            .err()
            .unwrap()
            .contains("intact subblock directory"));
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// A bare gray8 JPEG XR codestream header: no tiling, short header, DC only.
    fn jpeg_xr_header(width: u16, height: u16) -> Vec<u8> {
        let mut raw = b"WMPHOTO\0".to_vec();
        raw.extend([0x10, 0x00, 0x80, 0x01]);
        raw.extend((width - 1).to_be_bytes());
        raw.extend((height - 1).to_be_bytes());
        raw.extend([0x03, 0x00, 0, 0, 0, 0]);
        raw
    }

    #[test]
    fn decode_reservation_follows_the_readers_tile_batches() {
        let temp = temporary();
        let source = temp.join("large-tile.czi");
        // One sparse 8192 × 8192 gray16 tile: 128 MiB decoded and stored.
        let mut bytes = synthetic(
            &[block(1, &[('X', 0, 8192), ('Y', 0, 8192)], Vec::new())],
            &xml(&[], &[]),
        );
        let subblock = 544 + 32 + 256 + xml(&[], &[]).len();
        bytes[subblock + 32 + 8..subblock + 32 + 16].copy_from_slice(&(128 * MIB).to_le_bytes());
        std::fs::write(&source, &bytes).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        file.set_len(bytes.len() as u64 + 128 * MIB).unwrap();
        drop(file);
        let plan = scan(&source).unwrap();
        assert_eq!(plan.images.len(), 1);
        assert_eq!(plan.images[0].count, 1);
        // Payload, decoded tile, converted copy, reader plane and assembled frame:
        // a single tile is never multiplied by the thread count.
        let peak = plan.decode_peak(0, 128 * MIB);
        assert!(peak >= 5 * 128 * MIB, "{peak}");
        assert!(
            peak < 5 * 128 * MIB + plan.metadata_workspace + MIB,
            "{peak}"
        );
        // Many small tiles batch up to the thread count, but never past the tiles.
        let small = Tiles {
            count: 3,
            min_output_bytes: MIB,
            max_workspace: 3 * MIB,
        };
        let many = Plan {
            images: vec![small],
            ..plan
        };
        let batch = (rayon::current_num_threads() as u64).min(3);
        assert_eq!(
            many.decode_peak(0, 0),
            many.metadata_workspace + batch * 3 * MIB + FRAME_OVERHEAD
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn jpeg_xr_headers_must_match_their_directory_entries() {
        let temp = temporary();
        let source = temp.join("jpeg-xr.czi");
        for (header, error) in [
            (jpeg_xr_header(2, 1), None),
            (
                jpeg_xr_header(4096, 4096),
                Some("disagrees with its directory entry"),
            ),
            (vec![0; 16], Some("header is unreadable")),
        ] {
            let mut tile = block(0, &[('X', 0, 2), ('Y', 0, 1)], header);
            tile.compression = 4;
            std::fs::write(&source, synthetic(&[tile], &xml(&[], &[]))).unwrap();
            match (scan(&source), error) {
                (Ok(plan), None) => assert_eq!(plan.images[0].count, 1),
                (Err(actual), Some(expected)) => assert!(actual.contains(expected), "{actual}"),
                (result, _) => panic!("{:?}", result.err()),
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn opening_reserves_xml_strings_copied_into_every_image() {
        let temp = temporary();
        let source = temp.join("long-names.czi");
        let name = "n".repeat(2 * MIB as usize);
        let blocks: Vec<_> = (0..64)
            .map(|scene| block(0, &[('X', 0, 1), ('Y', 0, 1), ('S', scene, 1)], vec![0]))
            .collect();
        let bytes = synthetic(&blocks, &xml(&[(&name, "#FFFFFFFF")], &[]));
        std::fs::write(&source, &bytes).unwrap();
        let plan = scan(&source).unwrap();
        let strings = xml_strings(&source, plan.xml).unwrap();
        assert!(strings >= 2 * MIB);
        assert!(plan.opening_bytes(strings) >= 65 * 2 * strings);
        let budget = Budget::new(temp.join("budget")).unwrap();
        budget.set_limit(crate::memory::MIN_LIMIT_BYTES).unwrap();
        let error = Cache::new(Format::Czi, source.clone(), Arc::clone(&budget))
            .err()
            .unwrap();
        assert!(
            error.contains("does not fit the application memory budget"),
            "{error}"
        );
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
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
