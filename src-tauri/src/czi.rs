//! Zeiss CZI checks before the pinned reader allocates, and CZI decode accounting.

use crate::frame_cache::{u32_at, u64_at, CheckedSource, FRAME_OVERHEAD, MIB};
use crate::frames::{MAX_ENCODED_FRAME_BYTES, MAX_FRAME_BYTES};
use crate::memory::{Budget, Reservation};
use openreadout_core::ImageInfo;
use openreadout_czi::{
    CompressionId, DimensionEntry, DirectoryEntry, PixelTypeId, SegmentId, SEGMENT_HEADER_LEN,
};
use openreadout_jpegxr::SampleType;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

const MAX_METADATA_BYTES: u64 = 32 * MIB;
const MAX_DIRECTORY_BYTES: u64 = 128 * MIB;
const MAX_IMAGES: usize = 4096;
// JPEG XR container and codestream headers, including tile partitions.
const JPEG_XR_HEADER_BYTES: [u64; 2] = [4096, 64 * 1024];
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
    /// Most subblocks one plane can composite: single-plane subblocks at one
    /// Z/C/T coordinate plus every subblock spanning several planes.
    plane_tiles: u64,
    /// The smallest decoded tile bounds the reader's largest batch.
    min_output_bytes: u64,
    /// Payload, mask and decode copies of the largest tile.
    max_workspace: u64,
    ordinal: usize,
    spanning: u64,
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
        let plan = scan(&source.stable_path(), budget)?;
        source.verify()?;
        let strings = {
            let _parser = budget.require(
                plan.xml.1 * 32 + 8 * MIB,
                "CZI metadata parsing does not fit the application memory budget.",
            )?;
            let strings = xml_strings(&source.stable_path(), plan.xml)?;
            source.verify()?;
            strings
        };
        let metadata = budget.require(
            plan.opening_bytes(strings),
            "CZI metadata parsing does not fit the application memory budget.",
        )?;
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
        // largest tile among that plane's subblocks), at least one and never more
        // than the plane's subblocks. That largest tile is never smaller than the
        // scene's smallest, so dividing by the smallest never under-counts.
        // The reader's plane and our assembled frame coexist.
        let tiles = self.images[position as usize];
        let batch = (rayon::current_num_threads() as u64)
            .min(tiles.plane_tiles)
            .min((TILE_BATCH_BYTES / tiles.min_output_bytes.max(1)).max(1));
        self.metadata_workspace + batch * tiles.max_workspace + 2 * frame_bytes + FRAME_OVERHEAD
    }
}

struct Source {
    file: File,
    len: u64,
}

impl Source {
    fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(error)?;
        let len = file.metadata().map_err(error)?.len();
        Ok(Self { file, len })
    }

    fn fits(&self, offset: u64, bytes: u64) -> bool {
        offset.checked_add(bytes).is_some_and(|end| end <= self.len)
    }

    fn read(&mut self, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(bytes))
            .map_err(error)
    }

    /// The segment kind, allocated size and used size at `offset`.
    fn segment(&mut self, offset: u64) -> Result<(SegmentId, u64, u64), String> {
        let mut raw = [0; SEGMENT_HEADER_LEN as usize];
        self.read(offset, &mut raw)?;
        Ok((
            SegmentId::parse(&raw[..16]),
            u64_at(&raw, 16),
            u64_at(&raw, 24),
        ))
    }
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
    Source::open(path)?.read(offset, &mut raw)?;
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
        // Each active setup also becomes a JSON string value.
        text(&e.experiment_version)
            + e.active_setups
                .iter()
                .map(|s| s.len() as u64 + 64)
                .sum::<u64>()
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

/// Records and description bytes of a `CZEVL` payload, walked exactly as pinned
/// openreadout-czi 0.3.0 attach.rs::parse_event_list does. Descriptions may overlap,
/// so their sum can exceed the payload. The reader copies every description before
/// it meets a malformed record, so reject any list it would fail on.
fn event_list(b: &[u8]) -> Result<(u64, u64), String> {
    let walk = || {
        let field = |at: usize| Some(u32_at(b.get(at..at.checked_add(4)?)?, 0));
        let count = field(4)?;
        let (mut pos, mut descriptions) = (8usize, 0u64);
        for _ in 0..count {
            let entry_size = field(pos)? as i32;
            let size = usize::try_from(field(pos.checked_add(16)?)? as i32).ok()?;
            b.get(pos + 20..(pos + 20).checked_add(size)?)?;
            descriptions += size as u64;
            let step = usize::try_from(entry_size)
                .ok()
                .filter(|step| *step >= 20)
                .unwrap_or(20 + size);
            pos = pos.checked_add(step)?;
        }
        Some((u64::from(count), descriptions))
    };
    walk().ok_or_else(|| "CZI event list attachment is malformed.".into())
}

/// Tile columns of a JPEG XR stream whose headers probe() accepted: the decoder
/// keeps entropy contexts and quantizers per column, up to 4096.
fn jpeg_xr_columns(head: &[u8]) -> Result<u64, String> {
    let stream = if head.starts_with(b"WMPHOTO") {
        Some(head)
    } else {
        // The II BC container's image-offset tag locates the codestream. The
        // decoder takes the last such tag; refuse ambiguous containers.
        let ifd = u32_at(head, 4) as usize;
        let count = head
            .get(ifd..ifd + 2)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        let offsets: Vec<usize> = (0..usize::from(count))
            .filter_map(|i| {
                let at = ifd + 2 + 12 * i;
                (head.get(at..at + 2)? == [0xC0, 0xBC])
                    .then(|| head.get(at + 8..at + 12).map(|v| u32_at(v, 0) as usize))
                    .flatten()
            })
            .collect();
        if offsets.len() > 1 {
            return Err("CZI JPEG XR container has duplicate image-offset tags.".into());
        }
        offsets.first().and_then(|offset| head.get(*offset..))
    };
    let Some(stream) = stream else {
        return Ok(4096);
    };
    if stream.get(9).is_some_and(|flags| flags & 0x80 == 0) {
        return Ok(1);
    }
    // Width and height follow the 12-byte signature and flags, then 12-bit nv.
    let start = 96
        + if stream.get(10).is_some_and(|flags| flags & 0x80 != 0) {
            32
        } else {
            64
        };
    (start..start + 12)
        .try_fold(0u64, |value, bit| {
            let byte = stream.get(bit / 8)?;
            Some(value << 1 | u64::from(byte >> (7 - bit % 8) & 1))
        })
        .map_or(Ok(4096), |nv| Ok(nv + 1))
}

/// Probes a JPEG XR payload's headers, reading more only when that could complete them.
fn jpeg_xr_probe(
    source: &mut Source,
    offset: u64,
    bytes: u64,
) -> Result<(openreadout_jpegxr::Info, Vec<u8>), String> {
    let mut probed = Err(openreadout_jpegxr::Error::Decode(String::new()));
    let mut head = Vec::new();
    for limit in JPEG_XR_HEADER_BYTES {
        head = vec![0; bytes.min(limit) as usize];
        source.read(offset, &mut head)?;
        probed = openreadout_jpegxr::probe(&head);
        if !matches!(probed, Err(openreadout_jpegxr::Error::Decode(_))) || bytes <= limit {
            break;
        }
    }
    let info = probed.map_err(|e| match e {
        openreadout_jpegxr::Error::Unsupported(_) => {
            format!("CZI JPEG XR subblock uses an unsupported format: {e}")
        }
        _ => format!("CZI JPEG XR subblock header is unreadable: {e}"),
    })?;
    Ok((info, head))
}

/// Payload, mask and decode copies of one full-resolution subblock, after
/// checking its header against the directory entry.
fn subblock_workspace(
    source: &mut Source,
    entry: &DirectoryEntry,
    (samples, sample): (u64, SampleType),
    stored: u64,
) -> Result<u64, String> {
    // The reader allocates the payload size from the subblock header and
    // locates the payload after its embedded entry, reading up to 256 bytes.
    let (kind, _, used) = if source.fits(entry.file_position, SEGMENT_HEADER_LEN) {
        source.segment(entry.file_position)?
    } else {
        (SegmentId::Unknown, 0, 0)
    };
    let header_bytes = used.clamp(48, 256);
    if kind != SegmentId::SubBlock
        || !source.fits(entry.file_position + SEGMENT_HEADER_LEN, header_bytes)
    {
        return Err("CZI directory entry does not reference a subblock.".into());
    }
    let mut sizes = vec![0; header_bytes as usize];
    source.read(entry.file_position + SEGMENT_HEADER_LEN, &mut sizes)?;
    let disagrees = "CZI subblock header disagrees with its directory entry.";
    let embedded = u32_at(&sizes, 16 + 28) as usize;
    let embedded_bytes = 32 + 20 * embedded;
    if &sizes[16..18] != b"DV" || embedded > 32 || 16 + embedded_bytes > sizes.len() {
        return Err(disagrees.into());
    }
    let embedded_dimension = |name: char| {
        (0..embedded)
            .map(|k| &sizes[48 + 20 * k..68 + 20 * k])
            .find(|d| d[0] as char == name)
            .map(|d| u32_at(d, 16))
    };
    let (width, height) = (
        entry.dim('X').map(|x| x.stored_size),
        entry.dim('Y').map(|y| y.stored_size),
    );
    if PixelTypeId::from_raw(u32_at(&sizes, 16 + 2)) != entry.pixel_type
        || CompressionId::from_raw(u32_at(&sizes, 16 + 18)) != entry.compression
        || embedded_dimension('X') != width
        || embedded_dimension('Y') != height
    {
        return Err(disagrees.into());
    }
    let (width, height) = (width.unwrap_or(1), height.unwrap_or(1));
    let data_bytes = u64_at(&sizes, 8);
    let data_offset = entry.file_position
        + SEGMENT_HEADER_LEN
        + (16 + embedded_bytes as u64).max(256)
        + u64::from(u32_at(&sizes, 0));
    if data_bytes > MAX_ENCODED_FRAME_BYTES {
        return Err("CZI encoded subblock exceeds the 257 MiB input-buffer limit.".into());
    }
    if !source.fits(data_offset, data_bytes) {
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
            let (info, head) = jpeg_xr_probe(source, data_offset, data_bytes)?;
            let layout = (info.channels, info.sample);
            if (info.width, info.height) != (width, height)
                || (layout != (samples as u32, sample)
                    && !(entry.pixel_type == PixelTypeId::Bgra32 && layout == (4, SampleType::U8)))
            {
                return Err("CZI JPEG XR subblock geometry or pixel format disagrees with its directory entry.".into());
            }
            // Pinned openreadout-jpegxr 0.3.0 decoder.rs: output, compaction and
            // conversion; a u64 index entry per payload byte at most; per tile
            // column about 3 KiB of entropy context and 8 KiB of quantizers;
            // and two macroblock rows of i32 coefficients plus prediction for up
            // to 16 internal channels, with windowing margins.
            3 * stored
                + 8 * data_bytes
                + jpeg_xr_columns(&head)? * 12 * 1024
                + (u64::from(width).div_ceil(16) + 8) * 34 * 1024
        }
        _ => 2 * stored,
    };
    Ok(data_bytes + mask + copies)
}

/// The attachment directory and the attachments the reader interprets.
#[derive(Default)]
struct Attachments {
    directory_bytes: u64,
    entries: u64,
    time_stamp_bytes: u64,
    event_bytes: u64,
    events: u64,
    descriptions: u64,
}

fn attachments(
    source: &mut Source,
    position: u64,
    budget: &Arc<Budget>,
    metadata_bytes: &mut u64,
) -> Result<Attachments, String> {
    let mut found = Attachments::default();
    if position == 0 || !source.fits(position, SEGMENT_HEADER_LEN) {
        return Ok(found);
    }
    let (kind, _, used) = source.segment(position)?;
    if kind != SegmentId::AttachmentDirectory
        || used < 4
        || !source.fits(position + SEGMENT_HEADER_LEN, used)
    {
        return Ok(found);
    }
    found.directory_bytes = used;
    metadata_total(metadata_bytes, used)?;
    let start = position + SEGMENT_HEADER_LEN;
    let mut count = [0; 4];
    source.read(start, &mut count)?;
    for index in 0..u64::from(u32::from_le_bytes(count)) {
        let offset = 256 + 128 * index;
        let mut raw = [0; 128];
        if offset + 128 > used {
            break;
        }
        source.read(start + offset, &mut raw)?;
        if &raw[..2] != b"A1" {
            break;
        }
        found.entries += 1;
        let kind = String::from_utf8_lossy(&raw[40..48])
            .trim_end_matches('\0')
            .to_ascii_uppercase();
        let position = u64_at(&raw, 12);
        // Missing parts fall back to the master; count every candidate there.
        if !matches!(kind.as_str(), "CZTIMS" | "CZEVL")
            || !source.fits(position, SEGMENT_HEADER_LEN + 4)
            || source.segment(position)?.0 != SegmentId::Attachment
        {
            continue;
        }
        let mut size = [0; 4];
        source.read(position + SEGMENT_HEADER_LEN, &mut size)?;
        let size = u64::from(u32::from_le_bytes(size));
        if size > MAX_INTERPRETED_ATTACHMENT {
            continue;
        }
        metadata_total(metadata_bytes, size)?;
        if kind == "CZTIMS" {
            found.time_stamp_bytes += size;
        } else if source.fits(position + SEGMENT_HEADER_LEN + 256, size) {
            let _payload = budget.require(
                size,
                "CZI event list validation does not fit the application memory budget.",
            )?;
            let mut payload = vec![0; size as usize];
            source.read(position + SEGMENT_HEADER_LEN + 256, &mut payload)?;
            let (count, bytes) = event_list(&payload)?;
            found.event_bytes += size;
            found.events += count;
            found.descriptions += bytes;
        }
    }
    Ok(found)
}

/// Segments in the reader's sequential walk, which keeps one header for each.
fn segment_count(source: &mut Source) -> Result<u64, String> {
    let mut segments = 0u64;
    let mut offset = 0u64;
    while source.fits(offset, SEGMENT_HEADER_LEN) {
        let (kind, allocated, _) = source.segment(offset)?;
        if kind == SegmentId::Unknown {
            break;
        }
        segments += 1;
        let end = offset
            .checked_add(SEGMENT_HEADER_LEN)
            .and_then(|start| start.checked_add(allocated))
            .filter(|end| *end <= source.len)
            .ok_or("CZI segment extends beyond its source file.")?;
        if allocated == 0 {
            break;
        }
        offset = end;
    }
    Ok(segments)
}

// Mirrors pinned openreadout-czi 0.3.0 container.rs::CziFile::open_in, which loads
// the metadata XML, both directories, every segment header and small attachments.
// Read only headers and directory entries; pixel payloads are never read here.
fn scan(path: &Path, budget: &Arc<Budget>) -> Result<Plan, String> {
    let mut source = Source::open(path)?;
    if !source.fits(0, SEGMENT_HEADER_LEN + 80) || source.segment(0)?.0 != SegmentId::File {
        return Err("CZI file does not start with a ZISRAWFILE segment.".into());
    }
    let mut header = [0; 80];
    source.read(SEGMENT_HEADER_LEN, &mut header)?;
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
        && source.fits(metadata_position, SEGMENT_HEADER_LEN + 8)
        && source.segment(metadata_position)?.0 == SegmentId::Metadata
    {
        let mut size = [0; 4];
        source.read(metadata_position + SEGMENT_HEADER_LEN, &mut size)?;
        let size = u64::from(u32::from_le_bytes(size));
        let offset = metadata_position + SEGMENT_HEADER_LEN + 256;
        if source.fits(offset, size) {
            xml = (offset, size);
            metadata_total(&mut metadata_bytes, size)?;
        }
    }

    // The reader falls back to scanning every subblock when its directory is
    // unusable. Require the directory so planning stays bounded.
    let (kind, _, directory_bytes) =
        if directory_position != 0 && source.fits(directory_position, SEGMENT_HEADER_LEN) {
            source.segment(directory_position)?
        } else {
            (SegmentId::Unknown, 0, 0)
        };
    if kind != SegmentId::Directory
        || directory_bytes < 128
        || !source.fits(directory_position + SEGMENT_HEADER_LEN, directory_bytes)
    {
        return Err("CZI requires an intact subblock directory for bounded decoding; rescue scanning is unsupported.".into());
    }
    if directory_bytes > MAX_DIRECTORY_BYTES {
        return Err("CZI subblock directory exceeds the 128 MiB input limit.".into());
    }
    // Plane counts grow with directory entries, each at least 32 bytes on disk.
    let _planes = budget.require(
        directory_bytes * 2 + MIB,
        "CZI directory validation does not fit the application memory budget.",
    )?;
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
    let mut planes = HashMap::new();
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
        let workspace = subblock_workspace(&mut source, &entry, (samples, sample), stored)?;
        let ordinal = images.len();
        let tiles = images.entry(key).or_insert(Tiles {
            plane_tiles: 0,
            min_output_bytes: u64::MAX,
            max_workspace: 0,
            ordinal,
            spanning: 0,
        });
        if ['Z', 'C', 'T']
            .iter()
            .any(|d| entry.dim(*d).is_some_and(|x| x.size > 1))
        {
            tiles.spanning += 1;
        } else {
            let count = planes
                .entry((
                    tiles.ordinal,
                    entry.index('Z'),
                    entry.index('C'),
                    entry.index('T'),
                ))
                .or_insert(0u64);
            *count += 1;
            tiles.plane_tiles = tiles.plane_tiles.max(*count);
        }
        tiles.min_output_bytes = tiles.min_output_bytes.min(output);
        tiles.max_workspace = tiles.max_workspace.max(workspace);
    }

    let attached = attachments(
        &mut source,
        attachment_position,
        budget,
        &mut metadata_bytes,
    )?;
    let segments = segment_count(&mut source)?;

    // XML is copied and parsed into a DOM; directory entries are expanded, grouped
    // into scenes and cloned for each plane read. Interpreted attachments are read
    // whole and kept as values. Lossy UTF-8 can triple description bytes; while
    // converting, the payload, the doubled string capacity (up to 4x) and the
    // final description (3x) can coexist. Allow generous factors.
    let metadata_workspace = xml.1 * 32
        + directory_bytes * 8
        + u64::from(count) * 512
        + attached.directory_bytes * 4
        + attached.entries * 256
        + attached.time_stamp_bytes * 16
        + attached.event_bytes * 5
        + attached.events * 64
        + attached.descriptions * 3
        + segments * 64
        + 8 * MIB;
    // info() lists every channel of a scene's span and expands time stamps and
    // each event record, with its description, into JSON values for every image.
    let channels = channel_spans
        .values()
        .map(|(low, high)| (high - low + 1).clamp(1, MAX_LISTED_CHANNELS as i64) as u64)
        .max()
        .unwrap_or(1);
    Ok(Plan {
        metadata_workspace,
        xml,
        image_copies: channels * 512
            + attached.time_stamp_bytes * 8
            + attached.events * 640
            + attached.descriptions * 3,
        images: images
            .into_values()
            .map(|tiles| Tiles {
                plane_tiles: tiles.plane_tiles + tiles.spanning,
                ..tiles
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_cache::Cache;
    use crate::frames::tests::{strip, temporary};
    use crate::frames::{self, ExtractedFrame, Format, Projection, RawFrame, Selection};
    use openreadout_core::FormatReader;
    use std::sync::atomic::AtomicBool;

    /// One full-resolution subblock: directory pixel type and compression, its
    /// dimensions (name, start, size) and stored samples.
    struct Block {
        pixel_type: u32,
        compression: u32,
        dimensions: Vec<(char, i32, u32)>,
        /// Dimensions of the entry embedded in the subblock, when they differ.
        embedded: Option<Vec<(char, i32, u32)>>,
        data: Vec<u8>,
    }

    fn block(pixel_type: u32, dims: &[(char, i32, u32)], data: Vec<u8>) -> Block {
        Block {
            pixel_type,
            compression: 0,
            dimensions: dims.to_vec(),
            embedded: None,
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

    fn entry(
        block: &Block,
        dimensions: &[(char, i32, u32)],
        position: u64,
        file_part: u32,
    ) -> Vec<u8> {
        let mut raw = b"DV".to_vec();
        raw.extend(block.pixel_type.to_le_bytes());
        raw.extend(position.to_le_bytes());
        raw.extend(file_part.to_le_bytes());
        raw.extend(block.compression.to_le_bytes());
        raw.extend([0; 6]); // pyramid type and reserved bytes
        raw.extend((dimensions.len() as u32).to_le_bytes());
        for &(dimension, start, size) in dimensions {
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
            let described = entry(block, &block.dimensions, position, file_part);
            let embedded = block.embedded.as_deref().unwrap_or(&block.dimensions);
            let embedded = entry(block, embedded, position, file_part);
            let mut payload = vec![0; 16];
            payload[8..16].copy_from_slice(&(block.data.len() as u64).to_le_bytes());
            payload.extend(&embedded);
            payload.resize((16 + embedded.len()).max(256), 0);
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

    /// Append attachment segments and their directory to a synthetic file.
    fn with_attachments(mut out: Vec<u8>, attachments: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut directory = vec![0; 256];
        directory[..4].copy_from_slice(&(attachments.len() as u32).to_le_bytes());
        for (kind, data) in attachments {
            let mut payload = vec![0; 256];
            payload[..4].copy_from_slice(&(data.len() as u32).to_le_bytes());
            payload.extend(data);
            let position = segment(&mut out, "ZISRAWATTACH", &payload);
            let mut entry = vec![0; 128];
            entry[..2].copy_from_slice(b"A1");
            entry[12..20].copy_from_slice(&position.to_le_bytes());
            entry[40..40 + kind.len()].copy_from_slice(kind.as_bytes());
            directory.extend(entry);
        }
        let position = segment(&mut out, "ZISRAWATTDIR", &directory);
        let header = SEGMENT_HEADER_LEN as usize;
        out[header + 72..header + 80].copy_from_slice(&position.to_le_bytes());
        out
    }

    /// The validated plan, with a scratch shared budget next to the source.
    fn planned(path: &Path) -> Result<Plan, String> {
        scan(path, &Budget::new(path.with_extension("budget")).unwrap())
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
    fn raw(path: &Path, selection: Selection) -> Result<(RawFrame, ExtractedFrame), String> {
        let plan = planned(path)?;
        let mut dataset = openreadout_czi::CziReader.open(path).map_err(error)?;
        let images = dataset.info().map_err(error)?.images;
        plan.check_images(&images)?;
        let frame = frames::decode_frame(Format::Czi, dataset.as_mut(), &images, selection)?;
        Ok((frame, frames::description(Format::Czi, &images, selection)?))
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
            let (frame, metadata) = raw(
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
        let result = cache
            .extract_display(&output, selection, false, None)
            .unwrap();
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
            let (frame, metadata) = raw(&source, Selection::default()).unwrap();
            assert!(frame.rgb);
            assert_eq!(frame.pixels, expected);
            assert_eq!(
                metadata
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
        let (frame, _) = raw(&source, Selection::default()).unwrap();
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
        assert!(planned(&source)
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
        assert!(planned(&source).err().unwrap().contains("32 MiB"));
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
        assert!(planned(&source).err().unwrap().contains("128 MiB"));
        bytes[header + 52..header + 60].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&source, bytes).unwrap();
        assert!(planned(&source)
            .err()
            .unwrap()
            .contains("intact subblock directory"));
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// A bare gray8 JPEG XR codestream header: no tiling, short header, DC only.
    fn jpeg_xr_header(width: u16, height: u16) -> Vec<u8> {
        jpeg_xr_stream(width, height, 0, 0x80)
    }

    /// A bare gray8 codestream header with `columns` tile columns (0 = untiled)
    /// and the given short-header/alpha flag byte.
    fn jpeg_xr_stream(width: u16, height: u16, columns: u8, flags: u8) -> Vec<u8> {
        let mut raw = b"WMPHOTO\0".to_vec();
        // Tiled streams require an index table.
        raw.extend([0x10, if columns > 0 { 0x84 } else { 0 }, flags, 0x01]);
        raw.extend((width - 1).to_be_bytes());
        raw.extend((height - 1).to_be_bytes());
        if columns > 0 {
            // 12-bit vertical and horizontal tile counts, then 8-bit column widths.
            let nv = u32::from(columns - 1);
            raw.extend((nv << 12).to_be_bytes()[1..].iter());
            raw.extend(vec![1; usize::from(columns - 1)]);
        }
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
        let plan = planned(&source).unwrap();
        assert_eq!(plan.images.len(), 1);
        assert_eq!(plan.images[0].plane_tiles, 1);
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
            plane_tiles: 3,
            min_output_bytes: MIB,
            max_workspace: 3 * MIB,
            ordinal: 0,
            spanning: 0,
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
            (jpeg_xr_stream(2, 1, 0, 0x81), Some("unsupported format")),
        ] {
            let mut tile = block(0, &[('X', 0, 2), ('Y', 0, 1)], header);
            tile.compression = 4;
            std::fs::write(&source, synthetic(&[tile], &xml(&[], &[]))).unwrap();
            match (planned(&source), error) {
                (Ok(plan), None) => assert_eq!(plan.images[0].plane_tiles, 1),
                (Err(actual), Some(expected)) => assert!(actual.contains(expected), "{actual}"),
                (result, _) => panic!("{:?}", result.err()),
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn jpeg_xr_tile_columns_are_read_from_the_codestream_header() {
        let tiled = jpeg_xr_stream(64, 16, 4, 0x80);
        assert!(openreadout_jpegxr::probe(&tiled).is_ok());
        assert_eq!(jpeg_xr_columns(&tiled), Ok(4));
        assert_eq!(jpeg_xr_columns(&jpeg_xr_header(2, 1)), Ok(1));
        // An II BC container whose two image-offset tags disagree is ambiguous.
        let mut container = b"II\xBC\x01\x08\0\0\0\x02\0".to_vec();
        for offset in [64u32, 96] {
            container.extend([0xC0, 0xBC, 4, 0, 1, 0, 0, 0]);
            container.extend(offset.to_le_bytes());
        }
        container.resize(128, 0);
        let error = jpeg_xr_columns(&container).err().unwrap();
        assert!(error.contains("duplicate image-offset tags"), "{error}");
    }

    #[test]
    fn payload_offset_follows_the_embedded_subblock_entry() {
        let temp = temporary();
        let source = temp.join("embedded.czi");
        // Ten extra outer dimensions move the outer entry past 256 bytes; the
        // embedded entry, which the reader uses, stays short.
        let mut outer = vec![('X', 0, 2), ('Y', 0, 1)];
        outer.extend("ADEFGJKLNO".chars().map(|d| (d, 0, 1)));
        for (compression, data) in [(0, vec![5, 6]), (4, jpeg_xr_header(2, 1))] {
            let mut tile = block(0, &outer, data.clone());
            tile.compression = compression;
            tile.embedded = Some(vec![('X', 0, 2), ('Y', 0, 1)]);
            std::fs::write(&source, synthetic(&[tile], &xml(&[], &[]))).unwrap();
            planned(&source).unwrap();
            if compression == 0 {
                assert_eq!(raw(&source, Selection::default()).unwrap().0.pixels, data);
            }
        }
        let mut tile = block(0, &[('X', 0, 2), ('Y', 0, 1)], vec![0; 2]);
        tile.embedded = Some(vec![('X', 0, 3), ('Y', 0, 1)]);
        std::fs::write(&source, synthetic(&[tile], &xml(&[], &[]))).unwrap();
        let error = planned(&source).err().unwrap();
        assert!(error.contains("subblock header disagrees"), "{error}");
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn decode_batches_count_tiles_per_plane_not_per_scene() {
        let temp = temporary();
        let source = temp.join("stack.czi");
        let mut blocks = Vec::new();
        for z in 0..20 {
            for c in 0..2 {
                blocks.push(block(
                    0,
                    &[('X', 0, 1), ('Y', 0, 1), ('Z', z, 1), ('C', c, 1)],
                    vec![0],
                ));
            }
        }
        // One subblock spanning every Z plane of channel 0 joins each plane.
        let spanning = block(
            0,
            &[('X', 0, 1), ('Y', 0, 1), ('Z', 0, 20), ('C', 0, 1)],
            vec![0; 20],
        );
        std::fs::write(&source, synthetic(&blocks, &xml(&[], &[]))).unwrap();
        let plan = planned(&source).unwrap();
        assert_eq!(plan.images[0].plane_tiles, 1);
        let tiles = plan.images[0];
        assert_eq!(
            plan.decode_peak(0, 2),
            plan.metadata_workspace + tiles.max_workspace + 4 + FRAME_OVERHEAD
        );
        blocks.push(spanning);
        std::fs::write(&source, synthetic(&blocks, &xml(&[], &[]))).unwrap();
        assert_eq!(planned(&source).unwrap().images[0].plane_tiles, 2);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn overlapping_event_descriptions_are_reserved_per_image() {
        let temp = temporary();
        let source = temp.join("events.czi");
        // 100 records of 20 bytes whose 64 KiB descriptions all overlap the tail.
        let (records, text) = (100usize, 64 * 1024usize);
        let mut events = vec![0; 8];
        events[4..8].copy_from_slice(&(records as u32).to_le_bytes());
        for _ in 0..records {
            let mut record = [0; 20];
            record[..4].copy_from_slice(&20i32.to_le_bytes());
            record[16..20].copy_from_slice(&(text as i32).to_le_bytes());
            events.extend(record);
        }
        events.resize(events.len() + text, b'e');
        let parsed = openreadout_czi::parse_event_list(&events).unwrap();
        let actual: usize = parsed.iter().map(|event| event.description.len()).sum();
        assert_eq!((parsed.len(), actual), (records, records * text));
        assert_eq!(
            event_list(&events),
            Ok((records as u64, (records * text) as u64))
        );
        let bytes = with_attachments(
            synthetic(
                &[block(0, &[('X', 0, 1), ('Y', 0, 1)], vec![0])],
                &xml(&[], &[]),
            ),
            &[("CZEVL", events)],
        );
        std::fs::write(&source, bytes).unwrap();
        let plan = planned(&source).unwrap();
        assert!(
            plan.image_copies >= 3 * actual as u64,
            "{}",
            plan.image_copies
        );
        assert!(plan.opening_bytes(0) >= 2 * 3 * actual as u64);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn invalid_utf8_event_descriptions_reserve_their_conversion_peak() {
        let temp = temporary();
        let source = temp.join("lossy-events.czi");
        let text = 1024 * 1024usize;
        let mut events = vec![0; 8];
        events[4..8].copy_from_slice(&1u32.to_le_bytes());
        let mut record = [0; 20];
        record[..4].copy_from_slice(&((20 + text) as i32).to_le_bytes());
        record[16..20].copy_from_slice(&(text as i32).to_le_bytes());
        events.extend(record);
        events.resize(events.len() + text, 0xFF);
        let payload = events.len() as u64;
        let bytes = with_attachments(
            synthetic(
                &[block(0, &[('X', 0, 1), ('Y', 0, 1)], vec![0])],
                &xml(&[], &[]),
            ),
            &[("CZEVL", events)],
        );
        std::fs::write(&source, bytes).unwrap();
        // Payload, doubled lossy capacity (4x) and final description (3x) coexist.
        let plan = planned(&source).unwrap();
        assert!(
            plan.metadata_workspace >= payload + 7 * text as u64 + 8 * MIB,
            "{}",
            plan.metadata_workspace
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn malformed_event_lists_are_rejected_before_the_reader_copies_them() {
        let temp = temporary();
        let source = temp.join("bad-events.czi");
        // Two valid records, then a third whose description runs past the payload.
        let mut events = vec![0; 8];
        events[4..8].copy_from_slice(&3u32.to_le_bytes());
        for size in [4i32, 4, 1024] {
            let mut record = [0; 20];
            record[..4].copy_from_slice(&(20 + size.min(4)).to_le_bytes());
            record[16..20].copy_from_slice(&size.to_le_bytes());
            events.extend(record);
            events.extend(b"text");
        }
        assert!(openreadout_czi::parse_event_list(&events).is_err());
        let bytes = with_attachments(
            synthetic(
                &[block(0, &[('X', 0, 1), ('Y', 0, 1)], vec![0])],
                &xml(&[], &[]),
            ),
            &[("CZEVL", events)],
        );
        std::fs::write(&source, &bytes).unwrap();
        let error = planned(&source).err().unwrap();
        assert!(
            error.contains("event list attachment is malformed"),
            "{error}"
        );
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
        let plan = planned(&source).unwrap();
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
        use crate::backend::{Backend, Phase};
        use std::time::{Duration, Instant};

        fn ready(backend: &Backend) -> crate::backend::ImageState {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let state = backend.snapshot().unwrap();
                if state.phase != Phase::Loading {
                    assert_eq!(state.phase, Phase::Ready, "{state:?}");
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
        drop(backend);
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
                // The reservation for real files stays well inside the default budget.
                let peak = planned(&path).unwrap().decode_peak(position, frame_bytes);
                assert!(
                    peak < crate::memory::DEFAULT_LIMIT_BYTES / 4,
                    "{name}: {peak}"
                );
                let mut decoded: Option<(Selection, (RawFrame, ExtractedFrame))> = None;
                let mut frame = |selection: Selection| {
                    if decoded.as_ref().is_none_or(|(key, _)| *key != selection) {
                        decoded = Some((selection, raw(&path, selection).unwrap()));
                    }
                    let (frame, metadata) = &decoded.as_ref().unwrap().1;
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
                    frame.pixels.clone()
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
