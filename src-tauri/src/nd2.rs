//! Nikon ND2 checks before the pinned reader allocates, and ND2 decode accounting.

use crate::channels::MAX_CHANNELS;
use crate::frame_cache::{u32_at, u64_at, CheckedSource, FRAME_OVERHEAD, MIB};
use crate::frames::{frame_bytes as checked_frame_bytes, Format, MAX_ENCODED_FRAME_BYTES};
use crate::memory::{Budget, Reservation};
use crate::nd2_metadata;
use openreadout_core::{ImageInfo, SourceFile};
use openreadout_nd2::{meta::loops_from_lv, Attributes, LegacyFile, LoopKind, Nd2File};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

const MAX_POSITIONS: u32 = 4096;
const MAX_METADATA_BYTES: u64 = 32 * MIB;

fn error(error: impl std::fmt::Display) -> String {
    format!("ND2: {error}")
}

fn frame_bytes(width: u32, height: u32, bands: u32, bytes: usize) -> Result<usize, String> {
    checked_frame_bytes(Format::Nd2, width, height, bands, bytes)
}

pub(crate) struct Preflight {
    pub storage_bytes: u64,
    pub positions: u32,
}

// The dependency's public parser API is pinned to 0.3.0. Check storage geometry
// before its Dataset allocates/splits a whole acquisition frame, including padding.
pub(crate) fn preflight(path: &Path) -> Result<Preflight, String> {
    let mut source = SourceFile::open_local(path).map_err(error)?;
    let mut head = [0; 12];
    source.read_exact(&mut head).map_err(error)?;
    if head == openreadout_nd2::LEGACY_SIGNATURE {
        return legacy_preflight(path, &mut source);
    }
    let file = Nd2File::open(path).map_err(error)?;
    let mut metadata = |lv: &str, xml: &str| {
        file.read_chunk(&mut source, lv)
            .ok()
            .and_then(|raw| openreadout_nd2::lv_decode(&raw).ok())
            .or_else(|| {
                file.read_chunk(&mut source, xml)
                    .ok()
                    .and_then(|raw| openreadout_nd2::variant_decode(&raw).ok())
            })
    };
    let value = metadata("ImageAttributesLV!", "ImageAttributes!")
        .ok_or("ND2 has no readable image attributes.")?;
    let fields = openreadout_nd2::meta::unwrap_root(&value);
    for key in [
        "uiWidth",
        "uiHeight",
        "uiWidthBytes",
        "uiComp",
        "uiBpcInMemory",
        "uiSequenceCount",
    ] {
        if let Some(value) = fields.get(key) {
            if value.as_u64().is_none_or(|n| n > u64::from(u32::MAX)) {
                return Err(format!(
                    "ND2 image attribute {key} is invalid or exceeds 32 bits."
                ));
            }
        }
    }
    if fields
        .get("ePixelType")
        .is_some_and(|value| value.as_i64() != Some(1))
    {
        return Err("ND2 supports only unsigned integer pixel types.".into());
    }
    if fields
        .get("eCompression")
        .is_some_and(|value| !matches!(value.as_i64(), Some(0 | 2)))
    {
        return Err("ND2 supports uncompressed and lossless zlib frames; lossy or unknown compression is unsupported.".into());
    }
    let attrs = Attributes::from_lv(&value).ok_or("ND2 image dimensions are missing.")?;
    if !matches!(attrs.bits_in_memory, 8 | 16) || attrs.pixel_kind.is_some_and(|kind| kind != 1) {
        return Err("ND2 supports only unsigned 8-bit and 16-bit samples. Export this pixel type as a supported TIFF.".into());
    }
    if attrs.components == 0 || attrs.components > MAX_CHANNELS {
        return Err(format!(
            "ND2 supports at most {MAX_CHANNELS} channel samples."
        ));
    }
    let bytes = (attrs.bits_in_memory / 8) as usize;
    frame_bytes(attrs.width, attrs.height, attrs.components, bytes)?;
    if attrs.row_bytes != 0 {
        frame_bytes(attrs.row_bytes, attrs.height, 1, 1)?;
        let minimum = u64::from(attrs.width) * u64::from(attrs.components) * bytes as u64;
        if u64::from(attrs.row_bytes) < minimum {
            return Err("ND2 row stride is shorter than its declared pixels.".into());
        }
    }
    let positions = metadata("ImageMetadataLV!", "ImageMetadata!")
        .map(|experiment| experiment_bounds(&experiment))
        .transpose()?
        .unwrap_or(1);
    // The decoder allocates from the on-disk header, not the map's advertised
    // length. Check that actual length even when the two intentionally differ.
    for chunk in file
        .map
        .iter()
        .filter(|chunk| chunk.name.starts_with("ImageDataSeq|"))
    {
        source
            .seek(SeekFrom::Start(chunk.chunk_offset))
            .map_err(error)?;
        let mut header = [0; 16];
        source.read_exact(&mut header).map_err(error)?;
        if header[..4] != openreadout_nd2::CHUNK_MAGIC_BYTES {
            return Err("ND2 image chunk header has invalid magic.".into());
        }
        let name_bytes = u32_at(&header, 4);
        let data_bytes = u64_at(&header, 8);
        if data_bytes > MAX_ENCODED_FRAME_BYTES {
            return Err("ND2 encoded frame exceeds the 257 MiB input-buffer limit.".into());
        }
        if name_bytes > 65536
            || chunk
                .chunk_offset
                .checked_add(16 + u64::from(name_bytes))
                .and_then(|n| n.checked_add(data_bytes))
                .is_none_or(|end| end > file.file_len)
        {
            return Err("ND2 image chunk extends beyond its source file.".into());
        }
    }
    let storage_bytes = if attrs.row_bytes == 0 {
        u64::from(attrs.width)
            * u64::from(attrs.height)
            * u64::from(attrs.components)
            * bytes as u64
    } else {
        u64::from(attrs.row_bytes) * u64::from(attrs.height)
    };
    Ok(Preflight {
        storage_bytes,
        positions,
    })
}

fn experiment_bounds(experiment: &serde_json::Value) -> Result<u32, String> {
    let mut pending = vec![experiment];
    let mut counts = 0u32;
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "uiCount" {
                        let count = value
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or("ND2 experiment count is invalid or exceeds 32 bits.")?;
                        counts = counts
                            .checked_add(count)
                            .ok_or("ND2 experiment counts overflow.")?;
                    }
                    if key == "pSubLoops"
                        && (value.as_object().is_some_and(|v| !v.is_empty())
                            || value.as_array().is_some_and(|v| !v.is_empty()))
                    {
                        return Err("ND2 time sub-loops are unsupported; their dimensions cannot be mapped safely.".into());
                    }
                    pending.push(value);
                }
            }
            serde_json::Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    let loops = loops_from_lv(experiment);
    let mut count = 1u32;
    let mut positions = 1u32;
    for item in loops {
        if matches!(item.kind, LoopKind::CustomLoop | LoopKind::Other(_)) {
            return Err("ND2 custom experiment loops are unsupported; their meaning cannot be mapped safely to time or Z.".into());
        }
        count = count
            .checked_mul(item.count)
            .filter(|n| *n > 0)
            .ok_or("ND2 experiment dimensions overflow or are empty.")?;
        if item.kind == LoopKind::XyPositionLoop {
            positions = positions
                .checked_mul(item.count)
                .filter(|n| *n <= MAX_POSITIONS)
                .ok_or("ND2 supports at most 4096 positions.")?;
        }
    }
    Ok(positions)
}

/// A legacy JPEG2000 box's type, total length and header length at `offset`.
fn legacy_box(
    source: &mut (impl Read + Seek),
    offset: u64,
    file_len: u64,
) -> Result<([u8; 4], u64, u64), String> {
    source.seek(SeekFrom::Start(offset)).map_err(error)?;
    let mut header = [0; 16];
    source.read_exact(&mut header[..8]).map_err(error)?;
    let (length, header_bytes) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
        0 => (file_len - offset, 8),
        1 => {
            source.read_exact(&mut header[8..]).map_err(error)?;
            (u64::from_be_bytes(header[8..].try_into().unwrap()), 16)
        }
        n => (u64::from(n), 8),
    };
    Ok((header[4..8].try_into().unwrap(), length, header_bytes))
}

fn legacy_preflight(path: &Path, source: &mut SourceFile) -> Result<Preflight, String> {
    let file = LegacyFile::open(path).map_err(error)?;
    let (height, width, channels, bits) = file.image_header(source).map_err(error)?;
    if bits > 16 || channels == 0 || u32::from(channels) > MAX_CHANNELS {
        return Err("ND2 legacy JPEG2000 requires unsigned samples of at most 16 bits and at most 32 channel samples.".into());
    }
    let mut storage_bytes = frame_bytes(width, height, u32::from(channels), 2)? as u64;
    let positions = file
        .xml(source, "AIM1", 0)
        .or_else(|| file.xml(source, "AIMD", 0))
        .map(|experiment| experiment_bounds(&experiment))
        .transpose()?
        .unwrap_or(1);
    // Check every codestream's encoded size, SIZ geometry and signedness before
    // the JPEG2000 decoder allocates its coefficient/sample planes. Its codec
    // also enforces its own fixed output and compression-ratio ceilings.
    for stream in file.codestreams() {
        let (_, length, header_bytes) = legacy_box(source, stream.box_offset, file.file_len)?;
        if length < header_bytes + 42 || length > MAX_ENCODED_FRAME_BYTES {
            return Err(
                "ND2 legacy codestream is truncated or exceeds the encoded-frame limit.".into(),
            );
        }
        let mut siz = [0; 42];
        source.read_exact(&mut siz).map_err(error)?;
        let (cw, ch, components) = openreadout_nd2::legacy::codestream_size(&siz)
            .ok_or("ND2 legacy codestream has no readable SIZ geometry.")?;
        if (cw, ch) != (width, height) || components == 0 || u32::from(components) > MAX_CHANNELS {
            return Err("ND2 legacy codestream geometry disagrees with the image header.".into());
        }
        storage_bytes = storage_bytes.max(frame_bytes(cw, ch, u32::from(components), 2)? as u64);
        let mut precision = vec![0; usize::from(components) * 3];
        source.read_exact(&mut precision).map_err(error)?;
        if precision
            .as_chunks::<3>()
            .0
            .iter()
            .any(|c| c[0] & 0x80 != 0 || (c[0] & 0x7f) + 1 > 16)
        {
            return Err(
                "ND2 legacy signed or greater-than-16-bit JPEG2000 samples are unsupported.".into(),
            );
        }
    }
    Ok(Preflight {
        storage_bytes,
        positions,
    })
}

// Pinned openreadout-nd2 0.3.0 dataset.rs::open_chunked parses these through
// lv()/decode_any(). Its XML-only fallbacks never inflate LV data. Keep this
// list aligned when updating the reader; other chunks still count toward raw caps.
fn parsed_metadata_chunk(name: &[u8]) -> bool {
    matches!(
        name,
        b"ImageAttributesLV!"
            | b"ImageMetadataLV!"
            | b"ImageMetadataSeqLV|0!"
            | b"ImageCalibrationLV|0!"
            | b"ImageTextInfoLV!"
            | b"ImageEventsLV!"
            | b"ImageEvents!"
            | b"CustomData|ExperimentEventsV1_0!"
            | b"CustomData|RoiMetadata_v1!"
            | b"CustomDataVar|CustomDataV2_0!"
            | b"CustomDataVar|AppInfo_V1_0!"
    )
}

pub(crate) struct Bounds {
    pub(crate) expanded_bytes: u64,
    pub(crate) metadata_workspace: u64,
    pub(crate) encoded_bytes: u64,
    pub(crate) legacy: bool,
}

impl Bounds {
    // Walk legacy boxes or stream the modern chunk map before allocating the
    // reader's parsed metadata. Pixel payloads are skipped, never read here.
    pub(crate) fn scan(path: &Path) -> Result<Self, String> {
        Self::scan_inner(path, false)
    }

    pub(crate) fn validated(path: &Path) -> Result<Self, String> {
        Self::scan_inner(path, true)
    }

    fn scan_inner(path: &Path, validate: bool) -> Result<Self, String> {
        let mut file = File::open(path).map_err(error)?;
        let file_len = file.metadata().map_err(error)?.len();
        let mut signature = [0; 12];
        file.read_exact(&mut signature).map_err(error)?;
        if signature != openreadout_nd2::LEGACY_SIGNATURE {
            return Self::scan_chunk_map(path, file, file_len, validate);
        }
        let mut offset = 0u64;
        let mut metadata_bytes = 0u64;
        let mut encoded_bytes = 0u64;
        let mut entries = 0u64;
        while file_len.saturating_sub(offset) >= 8 {
            let (kind, length, header_bytes) = legacy_box(&mut file, offset, file_len)?;
            if header_bytes == 16 && length < 16 {
                return Err("ND2 legacy box has an invalid extended length.".into());
            }
            if length < 8 {
                return Err("ND2 legacy box has an invalid length.".into());
            }
            let pixels = &kind == b"jp2c";
            if offset.checked_add(length).is_none_or(|end| end > file_len) {
                return Err("ND2 chunk extends beyond its source file.".into());
            }
            if pixels {
                if length > MAX_ENCODED_FRAME_BYTES + 65552 {
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
            expanded_bytes: metadata_bytes,
            metadata_workspace: metadata_bytes * 128 + entries * 256 + 8 * MIB,
            encoded_bytes,
            legacy: true,
        })
    }
    fn scan_chunk_map(
        path: &Path,
        mut file: File,
        file_len: u64,
        validate: bool,
    ) -> Result<Self, String> {
        fn read(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(bytes))
                .map_err(error)
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
            let name_len = u32_at(&raw, 4) as usize;
            let length = u64_at(&raw, 8);
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
            file.read_exact(&mut name).map_err(error)?;
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
        let mut map = BufReader::new(File::open(path).map_err(error)?);
        map.seek(SeekFrom::Start(map_start)).map_err(error)?;
        let mut cursor = map_start;
        let map_end = map_start + map_bytes;
        let mut metadata_bytes = map_bytes;
        let mut expanded_bytes = map_bytes;
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
                map.read_exact(&mut byte).map_err(error)?;
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
            map.read_exact(&mut item).map_err(error)?;
            cursor += 16;
            let (mut actual_name, payload_start, length) =
                header(&mut file, u64_at(&item, 0), file_len)?;
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
                if length > MAX_ENCODED_FRAME_BYTES {
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
                let expanded = if validate && parsed_metadata_chunk(&actual_name) {
                    let mut raw = vec![0; length as usize];
                    read(&mut file, payload_start, &mut raw)?;
                    nd2_metadata::expanded_size(&raw).map_err(|error| {
                        let name: String = String::from_utf8_lossy(&actual_name)
                            .chars()
                            .take(256)
                            .collect();
                        format!("ND2 metadata chunk {name}: {error}")
                    })?
                } else {
                    length
                };
                expanded_bytes = expanded_bytes
                    .checked_add(expanded)
                    .filter(|count| *count <= MAX_METADATA_BYTES)
                    .ok_or("ND2 metadata and chunk map exceed the 32 MiB expanded input limit.")?;
            }
            entries += 1;
        }
        if entries == 0 || !has_attributes {
            return Err("ND2 requires a nonempty chunk map containing image attributes; rescue scanning is unsupported by bounded decoding.".into());
        }
        Ok(Self {
            expanded_bytes,
            metadata_workspace: expanded_bytes * 128 + entries * 256 + 8 * MIB,
            encoded_bytes,
            legacy: false,
        })
    }
}

/// ND2 bounds validated before the reader allocates.
pub(crate) struct Plan {
    bounds: Bounds,
    storage_bytes: u64,
    positions: u32,
}

impl Plan {
    /// Returns the plan and a reservation covering the reader's opening peak.
    pub(crate) fn new(
        source: &CheckedSource,
        budget: &Arc<Budget>,
    ) -> Result<(Self, Reservation), String> {
        // First reject oversized raw blocks without reading their payloads.
        Bounds::scan(&source.stable_path())?;
        source.verify()?;
        let bounds = {
            let _validation = budget.require(
                64 * MIB,
                "ND2 metadata validation does not fit the application memory budget.",
            )?;
            let bounds = Bounds::validated(&source.stable_path())?;
            source.verify()?;
            bounds
        };
        let preflight = {
            let _parser = budget.require(
                bounds.metadata_workspace,
                "ND2 metadata parsing does not fit the application memory budget.",
            )?;
            source.verify()?;
            preflight(&source.stable_path())?
        };
        source.verify()?;
        // info() clones position-specific trees as well as global metadata. Pay
        // for that opening peak before calling it, then retain only typed summaries.
        let opening_bytes = bounds.metadata_workspace
            + bounds.expanded_bytes * 128 * (u64::from(preflight.positions) + 1);
        let metadata = budget.require(
            opening_bytes,
            "ND2 position metadata expansion does not fit the application memory budget.",
        )?;
        Ok((
            Self {
                bounds,
                storage_bytes: preflight.storage_bytes,
                positions: preflight.positions,
            },
            metadata,
        ))
    }

    pub(crate) fn check_images(&self, images: &[ImageInfo]) -> Result<(), String> {
        if images.len() > self.positions as usize {
            return Err(
                "ND2 decoded position count exceeds its validated experiment loops.".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn decode_peak(&self, frame_bytes: u64) -> u64 {
        // Pinned openreadout 0.3.0: modern decoding has a padded input, zlib Vec
        // growth and split channel planes alongside our assembled frame. Legacy
        // JPEG2000 additionally keeps i32 coefficient, reconstruction and sample
        // planes; allow 32 bytes of codec storage per 16-bit storage byte.
        let multiplier = if self.bounds.legacy { 32 } else { 4 };
        self.bounds.metadata_workspace
            + self.bounds.encoded_bytes
            + self.storage_bytes.max(frame_bytes) * multiplier
            + frame_bytes
            + FRAME_OVERHEAD
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::frames::tests::{strip, temporary};
    use crate::frames::{self, ExtractedFrame, Selection};
    use openreadout_core::{Dataset, FormatReader};
    use openreadout_nd2::Nd2Reader;

    type Opened = (Box<dyn Dataset>, Vec<ImageInfo>, u64);

    fn open(path: &Path) -> Result<Opened, String> {
        let preflight = preflight(path)?;
        let dataset = Nd2Reader.open(path).map_err(error)?;
        let info = dataset.info().map_err(error)?;
        if info.images.len() > MAX_POSITIONS as usize {
            return Err("ND2 supports at most 4096 positions.".into());
        }
        Ok((dataset, info.images, preflight.storage_bytes))
    }

    fn inspect(path: &Path, selection: Selection) -> Result<frames::Dimensions, String> {
        let (_, images, _) = open(path)?;
        Ok(frames::description(Format::Nd2, &images, selection)?.dimensions)
    }

    fn extract(
        input: &Path,
        output: &Path,
        selection: Selection,
    ) -> Result<ExtractedFrame, String> {
        let (mut dataset, images, _) = open(input)?;
        let frame = frames::decode_frame(Format::Nd2, dataset.as_mut(), &images, selection)?;
        drop(dataset);
        let description = frames::description(Format::Nd2, &images, selection)?;
        frames::write_frame(&frame, description, output, false, None)
    }

    pub(crate) fn synthetic(bits: u32, rgb: bool) -> Vec<u8> {
        fn number(key: &str, value: u32) -> String {
            format!("<{key} runtype=\"lx_uint32\" value=\"{value}\"/>")
        }
        fn experiment(kind: u32, child: &str) -> String {
            format!(
                "{}<uLoopPars>{}</uLoopPars><ppNextLevelEx><i0>{child}</i0></ppNextLevelEx>",
                number("eType", kind),
                number("uiCount", 2)
            )
        }
        fn xml(body: &str) -> Vec<u8> {
            format!("<variant><no_name>{body}</no_name></variant>").into_bytes()
        }
        let bands = if rgb { 3 } else { 2 };
        let attrs: String = [
            ("uiWidth", 2),
            ("uiHeight", 1),
            ("uiComp", bands),
            ("uiWidthBytes", 2 * bands * bits / 8),
            ("uiBpcInMemory", bits),
            ("uiBpcSignificant", bits),
            ("uiSequenceCount", 8),
            ("ePixelType", 1),
            ("eCompression", 2),
        ]
        .into_iter()
        .map(|(key, value)| number(key, value))
        .collect();
        let mut chunks = vec![
            (
                openreadout_nd2::FILE_SIGNATURE.to_string(),
                b"Ver2.0".to_vec(),
            ),
            ("ImageAttributes!".into(), xml(&attrs)),
            (
                "ImageMetadata!".into(),
                xml(&experiment(1, &experiment(2, &experiment(4, "")))),
            ),
        ];
        if rgb {
            chunks.push(("ImageMetadataSeq|0!".into(), xml("<sPicturePlanes><sPlaneNew><a0><uiCompCount runtype=\"lx_uint32\" value=\"3\"/></a0></sPlaneNew></sPicturePlanes>")));
        }
        for frame in 0..8 {
            let mut pixels = vec![0; 8]; // timestamp
            for sample in 1..=2 * bands {
                let value = (frame * 10 + sample) as u16;
                if bits == 16 {
                    pixels.extend(value.to_le_bytes());
                } else {
                    pixels.push(value as u8);
                }
            }
            chunks.push((format!("ImageDataSeq|{frame}!"), pixels));
        }
        let mut bytes = Vec::new();
        let mut map = Vec::new();
        for (name, payload) in chunks {
            map.extend(name.as_bytes());
            map.extend((bytes.len() as u64).to_le_bytes());
            map.extend((payload.len() as u64).to_le_bytes());
            bytes.extend(openreadout_nd2::CHUNK_MAGIC_BYTES);
            bytes.extend((name.len() as u32).to_le_bytes());
            bytes.extend((payload.len() as u64).to_le_bytes());
            bytes.extend(name.as_bytes());
            bytes.extend(payload);
        }
        let map_offset = bytes.len() as u64;
        let name = openreadout_nd2::FILE_MAP_NAME;
        bytes.extend(openreadout_nd2::CHUNK_MAGIC_BYTES);
        bytes.extend((name.len() as u32).to_le_bytes());
        bytes.extend((map.len() as u64).to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(map);
        bytes.extend(map_offset.to_le_bytes());
        bytes
    }

    pub(crate) fn synthetic_samples(bits: u32, rgb: bool, frames: &[&[u16]; 8]) -> Vec<u8> {
        let mut bytes = synthetic(bits, rgb);
        let expected_samples = if rgb { 6 } else { 4 };
        for (frame, samples) in frames.iter().enumerate() {
            assert_eq!(samples.len(), expected_samples);
            let name = format!("ImageDataSeq|{frame}!");
            let start = bytes
                .windows(name.len())
                .position(|v| v == name.as_bytes())
                .unwrap()
                + name.len()
                + 8;
            for (index, value) in samples.iter().enumerate() {
                if bits == 16 {
                    bytes[start + index * 2..start + index * 2 + 2]
                        .copy_from_slice(&value.to_le_bytes());
                } else {
                    bytes[start + index] = u8::try_from(*value).unwrap();
                }
            }
        }
        bytes
    }
    #[test]
    fn extracts_selected_pzt_all_channels_and_rgb_without_changing_source() {
        let temp = temporary();
        for (bits, rgb) in [(8, false), (16, false), (8, true), (16, true)] {
            let source = temp.join(format!("source-{bits}-{rgb}.nd2"));
            let output = source.with_extension("tif");
            let bytes = synthetic(bits, rgb);
            std::fs::write(&source, &bytes).unwrap();
            let selection = Selection {
                position: 1,
                z: 1,
                time: 1,
            };
            let result = extract(&source, &output, selection).unwrap();
            // The pinned reader exposes common Z/time extents for every position.
            let (_, images, _) = open(&source).unwrap();
            assert!(images
                .iter()
                .all(|image| (image.size_z, image.size_t) == (2, 2)));
            assert_eq!(
                (
                    result.dimensions.positions.len(),
                    result.dimensions.z,
                    result.dimensions.time
                ),
                (2, 2, 2)
            );
            assert_eq!(
                (
                    result.dimensions.position,
                    result.dimensions.z_index,
                    result.dimensions.time_index
                ),
                (1, 1, 1)
            );
            let samples: Vec<u16> = if rgb {
                vec![73, 72, 71, 76, 75, 74]
            } else {
                vec![71, 72, 73, 74]
            };
            let expected: Vec<u8> = samples
                .iter()
                .flat_map(|n| {
                    if bits == 16 {
                        n.to_le_bytes().to_vec()
                    } else {
                        vec![*n as u8]
                    }
                })
                .collect();
            assert_eq!(strip(&output), expected);
            assert_eq!(result.channels.len(), if rgb { 3 } else { 2 });
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
            assert!(extract(&source, &source, selection).is_err());
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn rejects_oversized_actual_frame_header_even_when_chunk_map_is_small() {
        let temp = temporary();
        let source = temp.join("wrong-header.nd2");
        let output = temp.join("wrong-header.tif");
        let mut bytes = synthetic(16, false);
        let name = b"ImageDataSeq|0!";
        let name_offset = bytes.windows(name.len()).position(|v| v == name).unwrap();
        bytes[name_offset - 8..name_offset]
            .copy_from_slice(&(MAX_ENCODED_FRAME_BYTES + 1).to_le_bytes());
        std::fs::write(&source, &bytes).unwrap();
        assert!(extract(&source, &output, Selection::default())
            .unwrap_err()
            .contains("257 MiB"));
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn backend_browses_all_axes_and_reuses_correct_channel_caches() {
        use crate::backend::{channel_asset, read_asset, Backend, ImageState};
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        fn ready(backend: &Backend) -> ImageState {
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

        fn preview(backend: &Backend, channel: u32) -> Vec<u8> {
            let state = backend.snapshot().unwrap();
            let job = backend.current.lock().unwrap().clone().unwrap();
            let asset = channel_asset(
                &format!("/{}/channels/{channel}/preview.png", state.id),
                &state.id,
                state.width,
                state.height,
                state.channels.len(),
            )
            .unwrap();
            read_asset(&job, asset).unwrap()
        }

        // Same native dependency requirement as the backend's TIFF regressions.
        if std::process::Command::new("vips")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let temp = temporary();
        let source = temp.join("dimensions.nd2");
        let bytes = synthetic(8, false);
        std::fs::write(&source, &bytes).unwrap();
        let backend = Backend::new(temp.join("cache")).unwrap();
        backend.open(&source).unwrap();
        let first = ready(&backend);
        assert!(first.read_only && first.channel_mode && first.preview_ready);
        assert_eq!((first.width, first.height, first.channels.len()), (2, 1, 2));
        let initial = [preview(&backend, 0), preview(&backend, 1)];
        assert_ne!(initial[0], initial[1]);
        let selected = Selection {
            position: 1,
            z: 1,
            time: 1,
        };
        backend.select_plane(&first.id, selected).unwrap();
        let changed = ready(&backend);
        let dimensions = changed.dimensions.as_ref().unwrap();
        assert_eq!(
            (
                dimensions.position,
                dimensions.z_index,
                dimensions.time_index
            ),
            (1, 1, 1)
        );
        let channel_png = [preview(&backend, 0), preview(&backend, 1)];
        assert_ne!(channel_png, initial);
        let selected_cache = backend
            .current
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .dir
            .lock()
            .unwrap()
            .clone();
        let check = temp.join("selected-channel.png");
        std::fs::write(&check, &channel_png[1]).unwrap();
        let pixel = std::process::Command::new("vips")
            .arg("getpoint")
            .arg(&check)
            .args(["0", "0"])
            .output()
            .unwrap();
        assert!(pixel.status.success());
        assert_eq!(
            String::from_utf8(pixel.stdout)
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap(),
            72
        );
        assert!(backend
            .select_plane(&changed.id, Selection { z: 2, ..selected })
            .is_err());
        assert_eq!(backend.snapshot().unwrap().id, changed.id);
        assert!(!backend
            .current
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel
            .load(Ordering::Relaxed));
        backend
            .select_plane(&changed.id, Selection::default())
            .unwrap();
        let reset = ready(&backend);
        assert_eq!([preview(&backend, 0), preview(&backend, 1)], initial);
        backend.select_plane(&reset.id, selected).unwrap();
        ready(&backend);
        assert_eq!([preview(&backend, 0), preview(&backend, 1)], channel_png);
        assert_eq!(
            *backend
                .current
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .dir
                .lock()
                .unwrap(),
            selected_cache
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        backend.shutdown();
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    #[ignore = "requires public fixtures in test-data/nd2; parent operator runs this check"]
    fn public_nd2_metadata_and_independent_expected_pixels() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/nd2");
        let dims = inspect(
            &fixtures.join("dims_p4z5t3c2y32x32.nd2"),
            Selection {
                position: 3,
                z: 4,
                time: 2,
            },
        )
        .unwrap();
        assert_eq!((dims.z, dims.time), (5, 3));
        assert_eq!(
            dims.positions,
            [
                "point name 1",
                "point name 2",
                "point name 3",
                "point name 4"
            ]
        );
        let temp = temporary();
        for (position, expected) in [
            (
                0,
                [
                    [151, 131, 130, 120],
                    [124, 125, 131, 126],
                    [123, 121, 121, 138],
                    [119, 123, 141, 114],
                ],
            ),
            (
                1,
                [
                    [133, 121, 128, 128],
                    [136, 130, 141, 137],
                    [115, 125, 115, 101],
                    [132, 141, 131, 120],
                ],
            ),
        ] {
            let output = temp.join(format!("compressed-{position}.tif"));
            extract(
                &fixtures.join("compressed_lossless.nd2"),
                &output,
                Selection {
                    position,
                    ..Selection::default()
                },
            )
            .unwrap();
            let pixels = strip(&output);
            for (dy, row) in expected.into_iter().enumerate() {
                for (dx, value) in row.into_iter().enumerate() {
                    let offset = ((100 + dy) * 256 + 100 + dx) * 2;
                    assert_eq!(
                        u16::from_le_bytes(pixels[offset..offset + 2].try_into().unwrap()),
                        value
                    );
                }
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }
}
