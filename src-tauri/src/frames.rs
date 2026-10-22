//! Selected acquisition frames, decoded in Rust into a lossless cache TIFF.

use crate::channels::{domain, Channel, MAX_CHANNELS};
use openreadout_core::{Dataset, ImageInfo, PixelType, PlaneIndex};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;
/// Largest encoded frame payload read into memory: a decoded frame plus 1 MiB.
pub(crate) const MAX_ENCODED_FRAME_BYTES: u64 = MAX_FRAME_BYTES + 1024 * 1024;
/// Samples processed between cancellation checks.
pub(crate) const CANCEL_CHUNK_SAMPLES: usize = 65536;
const MAX_NAME_CHARS: usize = 256;

/// Microscopy containers browsed one selected frame at a time. ND2 and CZI
/// decode whole frames in Rust; OME-TIFF planes are TIFF pages read by libvips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Nd2,
    Czi,
    /// Detected by content, never by extension alone.
    OmeTiff,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        [("nd2", Self::Nd2), ("czi", Self::Czi)]
            .into_iter()
            .find_map(|(name, format)| extension.eq_ignore_ascii_case(name).then_some(format))
    }

    /// Display-cache salt; change it when decoding or normalization changes.
    pub fn cache_version(self) -> &'static str {
        match self {
            Self::Nd2 => "nd2-openreadout-0.3.0-v3-16bit",
            Self::Czi => "czi-openreadout-0.3.0-v2-16bit",
            Self::OmeTiff => "ome-tiff-v1",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Nd2 => "ND2",
            Self::Czi => "CZI",
            Self::OmeTiff => "OME-TIFF",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Selection {
    pub position: u32,
    pub z: u32,
    pub time: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum Projection {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "z")]
    Z,
    #[serde(rename = "time")]
    Time,
    #[serde(rename = "zTime")]
    ZTime,
}

impl Projection {
    pub fn projects_z(self) -> bool {
        matches!(self, Self::Z | Self::ZTime)
    }
    pub fn projects_time(self) -> bool {
        matches!(self, Self::Time | Self::ZTime)
    }
    pub fn canonical(self, selection: Selection) -> Selection {
        Selection {
            z: if self.projects_z() { 0 } else { selection.z },
            time: if self.projects_time() {
                0
            } else {
                selection.time
            },
            ..selection
        }
    }
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
pub struct ExtractedFrame {
    pub dimensions: Dimensions,
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub calibration: Option<Calibration>,
    #[serde(default)]
    pub display_ranges: Vec<[u16; 2]>,
    pub sample_bits: u8,
    pub rgb: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    pub pixel_width_um: Option<f64>,
    pub pixel_height_um: Option<f64>,
    pub z_step_um: Option<f64>,
    pub time_increment_seconds: Option<f64>,
}

pub(crate) fn frame_bytes(
    format: Format,
    width: u32,
    height: u32,
    bands: u32,
    bytes: usize,
) -> Result<usize, String> {
    let total = u128::from(width) * u128::from(height) * u128::from(bands) * bytes as u128;
    if total == 0 {
        return Err(format!("{format} selected frame has invalid dimensions."));
    }
    if total > u128::from(MAX_FRAME_BYTES) {
        // ponytail: whole-frame decoding; large stitched scenes need region streaming.
        return Err(format!(
            "{format} selected frame is {width} × {height} pixels with {bands} samples of {bytes} byte(s) ({:.1} MiB), above the 256 MiB decoded-frame limit. Export a smaller frame or region as TIFF.",
            total as f64 / (1024.0 * 1024.0)
        ));
    }
    Ok(total as usize)
}

pub(crate) fn truncated_name(name: &str) -> String {
    name.chars().take(MAX_NAME_CHARS).collect()
}

pub(crate) fn is_hex_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

pub(crate) fn description(
    format: Format,
    images: &[ImageInfo],
    selection: Selection,
) -> Result<ExtractedFrame, String> {
    let image = images
        .get(selection.position as usize)
        .ok_or_else(|| format!("{format} position is out of range."))?;
    if selection.z >= image.size_z || selection.time >= image.size_t {
        return Err(format!("{format} Z slice or timepoint is out of range."));
    }
    if !matches!(image.pixel_type, PixelType::Uint8 | PixelType::Uint16) {
        return Err(format!(
            "{format} supports only unsigned 8-bit and 16-bit samples."
        ));
    }
    if image.size_c == 0 || image.size_c > MAX_CHANNELS {
        return Err(format!("{format} supports 1–{MAX_CHANNELS} channels."));
    }
    let rgb = image.samples_per_pixel == 3;
    if !matches!(image.samples_per_pixel, 1 | 3) || (rgb && image.size_c != 1) {
        return Err(format!("{format} mixed RGB acquisition channels and non-grayscale/non-RGB sample layouts are unsupported. Export individual channels as TIFF."));
    }
    let bands = if rgb { 3 } else { image.size_c };
    let sample_bits = 8 * image.pixel_type.bytes_per_sample() as u8;
    frame_bytes(
        format,
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
            domain: domain(sample_bits),
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
                        .map(truncated_name)
                        .unwrap_or_else(|| format!("Channel {}", c + 1)),
                    color: channel
                        .and_then(|c| c.color.as_deref())
                        .filter(|s| is_hex_color(s))
                        .unwrap_or(if image.size_c == 1 {
                            "#ffffff"
                        } else {
                            colors[c as usize % colors.len()]
                        })
                        .into(),
                    domain: domain(sample_bits),
                }
            })
            .collect()
    };
    let calibrated = image.physical_size.x.is_some()
        || image.physical_size.y.is_some()
        || image.physical_size.z.is_some()
        || image.time_increment_s.is_some();
    let result = ExtractedFrame {
        dimensions: Dimensions {
            positions: images
                .iter()
                .enumerate()
                .map(|(p, image)| {
                    image
                        .name
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(truncated_name)
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
        sample_bits,
        rgb,
    };
    if serde_json::to_vec(&result)
        .map_err(|e| format!("{format}: {e}"))?
        .len()
        > 48 * 1024
    {
        return Err(format!(
            "{format} position/channel names exceed the 48 KiB metadata limit."
        ));
    }
    Ok(result)
}

pub(crate) struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub bands: u32,
    pub bytes: usize,
    pub rgb: bool,
    pub pixels: Vec<u8>,
}

pub(crate) fn decode_frame(
    format: Format,
    dataset: &mut dyn Dataset,
    images: &[ImageInfo],
    selection: Selection,
) -> Result<RawFrame, String> {
    let bands = description(format, images, selection)?.channels.len() as u32;
    let image = &images[selection.position as usize];
    let bytes = image.pixel_type.bytes_per_sample();
    let length = frame_bytes(format, image.size_x, image.size_y, bands, bytes)?;
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
            .map_err(|e| format!("{format}: {e}"))?;
        if plane.width != image.size_x
            || plane.height != image.size_y
            || plane.pixel_type != image.pixel_type
            || plane.samples_per_pixel != image.samples_per_pixel
            || plane.data.len()
                != frame_bytes(
                    format,
                    plane.width,
                    plane.height,
                    plane.samples_per_pixel,
                    bytes,
                )?
        {
            return Err(format!(
                "{format} decoded plane does not match its image metadata."
            ));
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
    })
}

pub(crate) fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        Err("Loading cancelled.".into())
    } else {
        Ok(())
    }
}

pub(crate) fn write_frame(
    frame: &RawFrame,
    description: ExtractedFrame,
    output: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<ExtractedFrame, String> {
    check_cancel(cancel)?;
    let mut result = description;
    // The display cache keeps original samples; 16-bit ranges are image metadata.
    if frame.bytes == 2 {
        result.display_ranges =
            sample_ranges(&frame.pixels, frame.bands as usize, frame.rgb, cancel)?;
    }
    let pixels = frame.pixels.as_slice();
    let header = tiff_header(
        frame.width,
        frame.height,
        frame.bands as u16,
        (frame.bytes * 8) as u16,
        frame.rgb,
        pixels.len() as u32,
    );
    let error = |e: std::io::Error| format!("Display cache: {e}");
    // Never truncate an existing path, particularly a source accidentally passed
    // as the cache destination. The caller owns this worker's cache directory.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(error)?;
    let written = (|| {
        file.write_all(&header).map_err(error)?;
        for block in pixels.chunks(CANCEL_CHUNK_SAMPLES * frame.bytes) {
            check_cancel(cancel)?;
            file.write_all(block).map_err(error)?;
        }
        check_cancel(cancel)
    })();
    if let Err(failure) = written {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(failure);
    }
    Ok(result)
}

/// Sampled minimum and maximum per channel; RGB shares one range.
fn sample_ranges(
    pixels: &[u8],
    bands: usize,
    rgb: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<[u16; 2]>, String> {
    let mut ranges = vec![[u16::MAX, 0]; if rgb { 1 } else { bands }];
    for (index, sample) in pixels.as_chunks::<2>().0.iter().enumerate() {
        if index % CANCEL_CHUNK_SAMPLES == 0 {
            check_cancel(cancel)?;
        }
        let value = u16::from_le_bytes(*sample);
        let range = &mut ranges[if rgb { 0 } else { index % bands }];
        range[0] = range[0].min(value);
        range[1] = range[1].max(value);
    }
    check_cancel(cancel)?;
    if rgb {
        ranges = vec![ranges[0]; bands];
    }
    Ok(ranges)
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
            "aperture-frames-{}-{}",
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

    #[test]
    fn projection_modes_have_stable_names_and_canonical_coordinates() {
        let selection = Selection {
            position: 7,
            z: 3,
            time: 5,
        };
        for (projection, name, z, time) in [
            (Projection::None, "none", 3, 5),
            (Projection::Z, "z", 0, 5),
            (Projection::Time, "time", 3, 0),
            (Projection::ZTime, "zTime", 0, 0),
        ] {
            assert_eq!(serde_json::to_value(projection).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<Projection>(serde_json::json!(name)).unwrap(),
                projection
            );
            assert_eq!(
                projection.canonical(selection),
                Selection {
                    position: 7,
                    z,
                    time
                }
            );
        }
        assert_eq!(Projection::default(), Projection::None);
    }

    #[test]
    fn preserves_unsigned_samples_and_rejects_invalid_dimensions() {
        let low_signal: Vec<u8> = [83u16, 1000, 90, 1500, 97, 2000]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(
            sample_ranges(&low_signal, 2, false, None).unwrap(),
            [[83, 97], [1000, 2000]]
        );
        let rgb = [10u16, 20, 30]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(sample_ranges(&rgb, 3, true, None).unwrap(), [[10, 30]; 3]);
        // The display TIFF stores the original 16-bit samples unchanged.
        let temp = temporary();
        let output = temp.join("display.tif");
        let mut image = ImageInfo::new(0, 3, 1, PixelType::Uint16);
        image.size_c = 2;
        let described = description(Format::Nd2, &[image], Selection::default()).unwrap();
        assert_eq!((described.sample_bits, described.rgb), (16, false));
        assert!(described.channels.iter().all(|c| c.domain == [0, 65535]));
        let frame = RawFrame {
            width: 3,
            height: 1,
            bands: 2,
            bytes: 2,
            rgb: false,
            pixels: low_signal.clone(),
        };
        let written = write_frame(&frame, described, &output, None).unwrap();
        assert_eq!(written.display_ranges, [[83, 97], [1000, 2000]]);
        assert_eq!(strip(&output), low_signal);
        std::fs::remove_dir_all(temp).unwrap();
        let mut pixels = vec![0; 8];
        interleave(&mut pixels, &[0x34, 0x12, 0xcd, 0xab], 2, 0, 2);
        interleave(&mut pixels, &[0xff, 0x00, 0x00, 0x80], 2, 1, 2);
        assert_eq!(pixels, [0x34, 0x12, 0xff, 0, 0xcd, 0xab, 0, 0x80]);
        assert!(frame_bytes(Format::Nd2, u32::MAX, u32::MAX, 32, 2).is_err());
        assert!(frame_bytes(Format::Nd2, 0, 32, 1, 1).is_err());
        assert!(frame_bytes(Format::Nd2, 16384, 16384, 2, 1).is_err());
        let mut image = ImageInfo::new(0, 2, 1, PixelType::Uint16);
        image.size_c = 2;
        image.size_z = 3;
        image.size_t = 4;
        let images = vec![image];
        for selection in [
            Selection {
                position: 1,
                ..Selection::default()
            },
            Selection {
                z: 3,
                ..Selection::default()
            },
            Selection {
                time: 4,
                ..Selection::default()
            },
        ] {
            assert!(description(Format::Nd2, &images, selection).is_err());
        }
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
