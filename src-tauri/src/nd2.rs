//! Selected ND2 acquisition frames, decoded in Rust into a lossless cache TIFF.

use crate::channels::{Channel, MAX_CHANNELS};
#[cfg(test)]
use openreadout_core::FormatReader;
use openreadout_core::{Dataset, ImageInfo, PixelType, PlaneIndex, SourceFile};
#[cfg(test)]
use openreadout_nd2::Nd2Reader;
use openreadout_nd2::{meta::loops_from_lv, Attributes, LegacyFile, LoopKind, Nd2File};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;
const MAX_POSITIONS: u32 = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Selection {
    pub position: u32,
    pub z: u32,
    pub time: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Dimensions {
    pub positions: Vec<String>,
    pub z: u32,
    pub time: u32,
    pub position: u32,
    pub z_index: u32,
    pub time_index: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtractedNd2 {
    pub dimensions: Dimensions,
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub calibration: Option<Calibration>,
    #[serde(default)]
    pub display_ranges: Vec<[u16; 2]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    pub pixel_width_um: Option<f64>,
    pub pixel_height_um: Option<f64>,
    pub z_step_um: Option<f64>,
    pub time_increment_seconds: Option<f64>,
}

fn error(error: impl std::fmt::Display) -> String {
    format!("ND2: {error}")
}

fn frame_bytes(width: u32, height: u32, bands: u32, bytes: usize) -> Result<usize, String> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(u64::from(bands)))
        .and_then(|n| n.checked_mul(bytes as u64))
        .filter(|n| *n > 0 && *n <= MAX_FRAME_BYTES)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "ND2 selected frame exceeds the 256 MiB decoded-frame limit or has invalid dimensions. Export a smaller frame as TIFF.".into())
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
        let name_bytes = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let data_bytes = u64::from_le_bytes(header[8..].try_into().unwrap());
        if data_bytes > MAX_FRAME_BYTES + 1024 * 1024 {
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

fn legacy_preflight(path: &Path, source: &mut SourceFile) -> Result<Preflight, String> {
    let file = LegacyFile::open(path).map_err(error)?;
    let (height, width, channels, bits) = file.image_header(source).map_err(error)?;
    if bits > 16 || channels == 0 || u32::from(channels) > MAX_CHANNELS {
        return Err("ND2 legacy JPEG2000 requires unsigned samples of at most 16 bits and at most 32 channel samples.".into());
    }
    frame_bytes(width, height, u32::from(channels), 2)?;
    let positions = file
        .xml(source, "AIM1", 0)
        .or_else(|| file.xml(source, "AIMD", 0))
        .map(|experiment| experiment_bounds(&experiment))
        .transpose()?
        .unwrap_or(1);
    // Check every codestream's encoded size, SIZ geometry and signedness before
    // the JPEG2000 decoder allocates its coefficient/sample planes. Its codec
    // also enforces its own fixed output and compression-ratio ceilings.
    let mut storage_bytes = frame_bytes(width, height, u32::from(channels), 2)? as u64;
    for stream in file.codestreams() {
        source
            .seek(SeekFrom::Start(stream.box_offset))
            .map_err(error)?;
        let mut header = [0; 16];
        source.read_exact(&mut header[..8]).map_err(error)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (length, header_bytes) = match length {
            0 => (file.file_len - stream.box_offset, 8),
            1 => {
                source.read_exact(&mut header[8..]).map_err(error)?;
                (u64::from_be_bytes(header[8..].try_into().unwrap()), 16)
            }
            length => (u64::from(length), 8),
        };
        if length < header_bytes + 42 || length > MAX_FRAME_BYTES + 1024 * 1024 {
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

pub(crate) fn description(
    images: &[ImageInfo],
    selection: Selection,
) -> Result<ExtractedNd2, String> {
    let image = images
        .get(selection.position as usize)
        .ok_or("ND2 position is out of range.")?;
    if selection.z >= image.size_z || selection.time >= image.size_t {
        return Err("ND2 Z slice or timepoint is out of range.".into());
    }
    if !matches!(image.pixel_type, PixelType::Uint8 | PixelType::Uint16) {
        return Err("ND2 supports only unsigned 8-bit and 16-bit samples.".into());
    }
    if image.size_c == 0 || image.size_c > MAX_CHANNELS {
        return Err(format!("ND2 supports 1–{MAX_CHANNELS} channels."));
    }
    let rgb = image.samples_per_pixel == 3;
    if !matches!(image.samples_per_pixel, 1 | 3) || (rgb && image.size_c != 1) {
        return Err("ND2 mixed RGB acquisition channels and non-grayscale/non-RGB sample layouts are unsupported. Export individual channels as TIFF.".into());
    }
    let bands = if rgb { 3 } else { image.size_c };
    frame_bytes(
        image.size_x,
        image.size_y,
        bands,
        image.pixel_type.bytes_per_sample(),
    )?;
    let channels = if rgb {
        [
            ("Red", "#ff0000"),
            ("Green", "#00ff00"),
            ("Blue", "#0000ff"),
        ]
        .into_iter()
        .map(|(name, color)| Channel {
            name: name.into(),
            color: color.into(),
        })
        .collect()
    } else {
        let colors = [
            "#ff0000", "#00ff00", "#0000ff", "#ff00ff", "#00ffff", "#ffff00",
        ];
        (0..image.size_c)
            .map(|c| {
                let channel = image.channels.get(c as usize);
                Channel {
                    name: channel
                        .and_then(|c| c.name.as_deref())
                        .filter(|s| !s.trim().is_empty())
                        .map(|name| name.chars().take(256).collect())
                        .unwrap_or_else(|| format!("Channel {}", c + 1)),
                    color: channel
                        .and_then(|c| c.color.as_deref())
                        .filter(|s| {
                            s.len() == 7
                                && s.starts_with('#')
                                && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
                        })
                        .unwrap_or(if image.size_c == 1 {
                            "#ffffff"
                        } else {
                            colors[c as usize % colors.len()]
                        })
                        .into(),
                }
            })
            .collect()
    };
    let calibrated = image.physical_size.x.is_some()
        || image.physical_size.y.is_some()
        || image.physical_size.z.is_some()
        || image.time_increment_s.is_some();
    let result = ExtractedNd2 {
        dimensions: Dimensions {
            positions: images
                .iter()
                .enumerate()
                .map(|(p, image)| {
                    image
                        .name
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(|name| name.chars().take(256).collect())
                        .unwrap_or_else(|| format!("Position {}", p + 1))
                })
                .collect(),
            z: image.size_z,
            time: image.size_t,
            position: selection.position,
            z_index: selection.z,
            time_index: selection.time,
        },
        channels,
        calibration: calibrated.then_some(Calibration {
            pixel_width_um: image.physical_size.x,
            pixel_height_um: image.physical_size.y,
            z_step_um: image.physical_size.z,
            time_increment_seconds: image.time_increment_s,
        }),
        display_ranges: Vec::new(),
    };
    if serde_json::to_vec(&result).map_err(error)?.len() > 48 * 1024 {
        return Err("ND2 position/channel names exceed the 48 KiB metadata limit.".into());
    }
    Ok(result)
}

#[cfg(test)]
type Opened = (Box<dyn Dataset>, Vec<ImageInfo>, u64);

#[cfg(test)]
fn open(path: &Path) -> Result<Opened, String> {
    let preflight = preflight(path)?;
    let dataset = Nd2Reader.open(path).map_err(error)?;
    let info = dataset.info().map_err(error)?;
    if info.images.len() > MAX_POSITIONS as usize {
        return Err("ND2 supports at most 4096 positions.".into());
    }
    Ok((dataset, info.images, preflight.storage_bytes))
}

#[cfg(test)]
fn inspect(path: &Path, selection: Selection) -> Result<Dimensions, String> {
    let (_, images, _) = open(path)?;
    Ok(description(&images, selection)?.dimensions)
}

#[cfg(test)]
pub fn extract(input: &Path, output: &Path, selection: Selection) -> Result<ExtractedNd2, String> {
    extract_frame(input, output, selection, false)
}

pub(crate) struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub bands: u32,
    pub bytes: usize,
    pub rgb: bool,
    pub pixels: Vec<u8>,
    pub metadata: ExtractedNd2,
}

pub(crate) fn decode_frame(
    dataset: &mut dyn Dataset,
    images: &[ImageInfo],
    selection: Selection,
) -> Result<RawFrame, String> {
    let metadata = description(images, selection)?;
    let image = &images[selection.position as usize];
    let bytes = image.pixel_type.bytes_per_sample();
    let bands = metadata.channels.len() as u32;
    let length = frame_bytes(image.size_x, image.size_y, bands, bytes)?;
    // ponytail: bounded whole-frame assembly until the reader offers bounded regions.
    let mut pixels = vec![0u8; length];
    for c in 0..image.size_c {
        let plane = dataset
            .read_plane(
                selection.position,
                PlaneIndex {
                    c,
                    z: selection.z,
                    t: selection.time,
                },
            )
            .map_err(error)?;
        if plane.width != image.size_x
            || plane.height != image.size_y
            || plane.pixel_type != image.pixel_type
            || plane.samples_per_pixel != image.samples_per_pixel
            || plane.data.len()
                != frame_bytes(plane.width, plane.height, plane.samples_per_pixel, bytes)?
        {
            return Err("ND2 decoded plane does not match its image metadata.".into());
        }
        if image.samples_per_pixel == 3 {
            pixels = plane.data;
        } else {
            interleave(&mut pixels, &plane.data, bands as usize, c as usize, bytes);
        }
    }
    Ok(RawFrame {
        width: image.size_x,
        height: image.size_y,
        bands,
        bytes,
        rgb: image.samples_per_pixel == 3,
        pixels,
        metadata,
    })
}

#[cfg(test)]
fn extract_frame(
    input: &Path,
    output: &Path,
    selection: Selection,
    display: bool,
) -> Result<ExtractedNd2, String> {
    let (mut dataset, images, _) = open(input)?;
    let frame = decode_frame(dataset.as_mut(), &images, selection)?;
    drop(dataset);
    write_frame(&frame, output, display)
}

pub(crate) fn write_frame(
    frame: &RawFrame,
    output: &Path,
    display: bool,
) -> Result<ExtractedNd2, String> {
    let mut result = frame.metadata.clone();
    // Normalize only the disposable display cache; original frame samples stay intact.
    let normalized;
    let (pixels, output_bits) = if display && frame.bytes == 2 {
        let (samples, ranges) = display_samples(&frame.pixels, frame.bands as usize, frame.rgb);
        normalized = samples;
        result.display_ranges = ranges;
        (normalized.as_slice(), 8)
    } else {
        (frame.pixels.as_slice(), (frame.bytes * 8) as u16)
    };
    let header = tiff_header(
        frame.width,
        frame.height,
        frame.bands as u16,
        output_bits,
        frame.rgb,
        pixels.len() as u32,
    );
    // Never truncate an existing path, particularly a source accidentally passed
    // as the cache destination. The caller owns this worker's cache directory.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(error)?;
    if let Err(failure) = file.write_all(&header).and_then(|_| file.write_all(pixels)) {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(error(failure));
    }
    Ok(result)
}

fn display_samples(pixels: &[u8], bands: usize, rgb: bool) -> (Vec<u8>, Vec<[u16; 2]>) {
    let mut ranges = vec![[u16::MAX, 0]; if rgb { 1 } else { bands }];
    for (index, sample) in pixels.as_chunks::<2>().0.iter().enumerate() {
        let value = u16::from_le_bytes(*sample);
        let range = &mut ranges[if rgb { 0 } else { index % bands }];
        range[0] = range[0].min(value);
        range[1] = range[1].max(value);
    }
    let display = pixels
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let value = u32::from(u16::from_le_bytes(*sample));
            let [minimum, maximum] = ranges[if rgb { 0 } else { index % bands }].map(u32::from);
            if maximum == minimum {
                if value == 0 {
                    0
                } else {
                    255
                }
            } else {
                (((value - minimum) * 255 + (maximum - minimum) / 2) / (maximum - minimum)) as u8
            }
        })
        .collect();
    if rgb {
        ranges = vec![ranges[0]; bands];
    }
    (display, ranges)
}

fn interleave(output: &mut [u8], plane: &[u8], bands: usize, channel: usize, bytes: usize) {
    for (pixel, sample) in output
        .chunks_exact_mut(bands * bytes)
        .zip(plane.chunks_exact(bytes))
    {
        pixel[channel * bytes..(channel + 1) * bytes].copy_from_slice(sample);
    }
}

// The cache is a classic, little-endian, uncompressed TIFF. Its single strip is
// bounded by MAX_FRAME_BYTES. Display samples and scientific source pixels stay separate.
fn tiff_header(width: u32, height: u32, bands: u16, bits: u16, rgb: bool, length: u32) -> Vec<u8> {
    let extra = if rgb { 0 } else { bands - 1 };
    let count = if extra > 0 { 12 } else { 11 };
    let value_start = 8 + 2 + count * 12 + 4;
    let mut values = Vec::<u8>::new();
    let mut shorts = |value: u16, count: u16| {
        if count <= 2 {
            u32::from(value)
                | if count == 2 {
                    u32::from(value) << 16
                } else {
                    0
                }
        } else {
            let offset = value_start + values.len() as u32;
            for _ in 0..count {
                values.extend(value.to_le_bytes());
            }
            offset
        }
    };
    let bits_value = shorts(bits, bands);
    let format_value = shorts(1, bands);
    let extras_value = shorts(0, extra);
    let offset = value_start + values.len() as u32;
    let mut tags = vec![
        (256u16, 4u16, 1u32, width),
        (257, 4, 1, height),
        (258, 3, u32::from(bands), bits_value),
        (259, 3, 1, 1),
        (262, 3, 1, if rgb { 2 } else { 1 }),
        (273, 4, 1, offset),
        (277, 3, 1, u32::from(bands)),
        (278, 4, 1, height),
        (279, 4, 1, length),
        (284, 3, 1, 1),
        (339, 3, u32::from(bands), format_value),
    ];
    if extra > 0 {
        tags.push((338, 3, u32::from(extra), extras_value));
    }
    tags.sort_by_key(|tag| tag.0);
    let mut header = b"II\x2a\0\x08\0\0\0".to_vec();
    header.extend((tags.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in tags {
        header.extend(tag.to_le_bytes());
        header.extend(kind.to_le_bytes());
        header.extend(count.to_le_bytes());
        header.extend(value.to_le_bytes());
    }
    header.extend(0u32.to_le_bytes());
    header.extend(values);
    header
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temporary() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aperture-nd2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    pub(crate) fn strip(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).unwrap();
        let count = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
        let entry = bytes[10..10 + count * 12]
            .as_chunks::<12>()
            .0
            .iter()
            .find(|entry| entry[..2] == 273u16.to_le_bytes())
            .unwrap();
        let offset = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        bytes[offset..].to_vec()
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
            .copy_from_slice(&(MAX_FRAME_BYTES + 1024 * 1024 + 1).to_le_bytes());
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

    #[test]
    fn preserves_unsigned_samples_and_rejects_invalid_dimensions() {
        let low_signal: Vec<u8> = [83u16, 1000, 90, 1500, 97, 2000]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let (display, ranges) = display_samples(&low_signal, 2, false);
        assert_eq!(display, [0, 0, 128, 128, 255, 255]);
        assert_eq!(ranges, [[83, 97], [1000, 2000]]);
        let (rgb, ranges) = display_samples(
            &[10u16, 20, 30]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
            3,
            true,
        );
        assert_eq!(rgb, [0, 128, 255]);
        assert_eq!(ranges, [[10, 30]; 3]);
        assert_eq!(display_samples(&[0, 0, 0, 0], 1, false).0, [0, 0]);
        assert_eq!(display_samples(&[83, 0, 83, 0], 1, false).0, [255, 255]);
        let mut pixels = vec![0; 8];
        interleave(&mut pixels, &[0x34, 0x12, 0xcd, 0xab], 2, 0, 2);
        interleave(&mut pixels, &[0xff, 0x00, 0x00, 0x80], 2, 1, 2);
        assert_eq!(pixels, [0x34, 0x12, 0xff, 0, 0xcd, 0xab, 0, 0x80]);
        assert!(frame_bytes(u32::MAX, u32::MAX, 32, 2).is_err());
        assert!(frame_bytes(0, 32, 1, 1).is_err());
        assert!(frame_bytes(16384, 16384, 2, 1).is_err());
        let mut image = ImageInfo::new(0, 2, 1, PixelType::Uint16);
        image.size_c = 2;
        image.size_z = 3;
        image.size_t = 4;
        let images = vec![image];
        assert!(description(
            &images,
            Selection {
                position: 1,
                ..Selection::default()
            }
        )
        .is_err());
        assert!(description(
            &images,
            Selection {
                z: 3,
                ..Selection::default()
            }
        )
        .is_err());
        assert!(description(
            &images,
            Selection {
                time: 4,
                ..Selection::default()
            }
        )
        .is_err());
        let header = tiff_header(2, 1, 2, 16, false, 8);
        let strip = header[10..]
            .as_chunks::<12>()
            .0
            .iter()
            .find(|entry| entry[..2] == 273u16.to_le_bytes())
            .unwrap();
        assert_eq!(
            u32::from_le_bytes(strip[8..12].try_into().unwrap()) as usize,
            header.len()
        );
    }
}
