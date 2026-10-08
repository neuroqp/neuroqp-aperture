//! ImageJ PointRoi codec. Offsets follow ImageJ's RoiDecoder/RoiEncoder.
use serde::{Deserialize, Serialize};

pub const MAX_POINTS: usize = 1_000_000;
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub counter: u8,
    #[serde(default)]
    pub position: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointRoi {
    pub points: Vec<Point>,
    pub point_type: u8,
    pub size: u16,
    pub color: Option<u32>,
    pub show_labels: bool,
    #[serde(default)]
    pub position: u32,
    #[serde(default)]
    pub channel: u32,
    #[serde(default)]
    pub slice: u32,
    #[serde(default)]
    pub frame: u32,
    /// Retained on the backend, never accepted from the frontend.
    #[serde(skip)]
    pub raw: Vec<u8>,
}

fn range(data: &[u8], offset: usize, length: usize) -> Result<&[u8], String> {
    data.get(offset..offset.checked_add(length).ok_or("ROI offset overflow")?)
        .ok_or_else(|| "Truncated ImageJ ROI".into())
}
fn short(data: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_be_bytes(
        range(data, offset, 2)?.try_into().unwrap(),
    ))
}
fn int(data: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_be_bytes(
        range(data, offset, 4)?.try_into().unwrap(),
    ))
}
fn ij_short(data: &[u8], offset: usize) -> Result<i32, String> {
    let n = short(data, offset)?;
    let signed = n as i16;
    Ok(if signed < -5000 {
        n as i32
    } else {
        signed as i32
    })
}
fn put_short(data: &mut [u8], offset: usize, n: u16) {
    data[offset..offset + 2].copy_from_slice(&n.to_be_bytes());
}
fn put_int(data: &mut [u8], offset: usize, n: u32) {
    data[offset..offset + 4].copy_from_slice(&n.to_be_bytes());
}

/// None means a valid header whose ROI type is unsupported, not an empty selection.
pub fn decode(data: &[u8]) -> Result<Option<PointRoi>, String> {
    if data.len() > MAX_BYTES || range(data, 0, 64)?[..4] != *b"Iout" {
        return Err("Invalid ImageJ ROI header or excessive size".into());
    }
    if data[6] != 10 || int(data, 36)? != 0 || short(data, 48)? != 0 {
        return Ok(None);
    }
    let version = short(data, 4)?;
    let mut n = short(data, 16)? as usize;
    if n == 0 {
        n = int(data, 18)? as usize;
    }
    if n == 0 || n > MAX_POINTS {
        return Err("Invalid ImageJ point count".into());
    }
    let options = short(data, 50)?;
    let subpixel = version >= 222 && options & 128 != 0;
    let coord_end = 64 + n * if subpixel { 12 } else { 4 };
    range(data, 64, coord_end - 64)?;
    let h = int(data, 60)? as usize;
    let mut counters = 0;
    let (mut channel, mut slice, mut frame) = (0, 0, 0);
    if h != 0 {
        if h < coord_end {
            return Err("Overlapping ImageJ ROI header".into());
        }
        range(data, h, 52)?;
        channel = int(data, h + 4)?;
        slice = int(data, h + 8)?;
        frame = int(data, h + 12)?;
        for (offset, length) in [(16, 20), (40, 44)] {
            let start = int(data, h + offset)? as usize;
            let len = int(data, h + length)? as usize;
            if start != 0 && len != 0 {
                range(data, start, len.checked_mul(2).ok_or("ROI text overflow")?)?;
            }
        }
        if version >= 227 {
            counters = int(data, h + 48)? as usize;
        }
        if counters != 0 {
            range(data, counters, n * 4)?;
        }
    }
    let left = ij_short(data, 10)?;
    let top = ij_short(data, 8)?;
    let mut points = Vec::with_capacity(n);
    for i in 0..n {
        let (x, y) = if subpixel {
            (
                f32::from_bits(int(data, 64 + 4 * n + 4 * i)?) as f64,
                f32::from_bits(int(data, 64 + 8 * n + 4 * i)?) as f64,
            )
        } else {
            (
                (left + ij_short(data, 64 + 2 * i)?.max(0)) as f64,
                (top + ij_short(data, 64 + 2 * n + 2 * i)?.max(0)) as f64,
            )
        };
        if !x.is_finite() || !y.is_finite() {
            return Err("Non-finite ImageJ point coordinate".into());
        }
        let c = if counters == 0 {
            0
        } else {
            int(data, counters + 4 * i)?
        };
        points.push(Point {
            x,
            y,
            counter: c as u8,
            position: c >> 8,
        });
    }
    let color = int(data, 40)?;
    Ok(Some(PointRoi {
        points,
        point_type: if version >= 226 { data[52] } else { 0 },
        size: if version >= 226 { short(data, 34)? } else { 1 },
        color: (color != 0).then_some(color),
        show_labels: options & 1024 != 0,
        position: int(data, 56)?,
        channel,
        slice,
        frame,
        raw: data.to_vec(),
    }))
}

pub fn encode(roi: &PointRoi) -> Result<Vec<u8>, String> {
    let n = roi.points.len();
    if n == 0 || n > MAX_POINTS || roi.point_type > 3 || roi.size > 6 {
        return Err("Invalid ImageJ point count, type or size".into());
    }
    for p in &roi.points {
        if !p.x.is_finite()
            || !p.y.is_finite()
            || !(p.x as f32).is_finite()
            || !(p.y as f32).is_finite()
            || p.position > 0x00ff_ffff
        {
            return Err("ImageJ point coordinate or slice is out of range".into());
        }
    }
    if !roi.raw.is_empty() {
        let mut original = decode(&roi.raw)?.ok_or("Original ROI is not a point selection")?;
        original.raw.clear();
        let mut current = roi.clone();
        current.raw.clear();
        if original == current {
            return Ok(roi.raw.clone());
        }
    }
    let h = 64 + n * 12;
    let mut data = vec![0; h + 64];
    if !roi.raw.is_empty() {
        data[..64].copy_from_slice(range(&roi.raw, 0, 64)?);
        let old_h = int(&roi.raw, 60)? as usize;
        if old_h != 0 {
            // Keep every existing metadata byte; relocate only documented absolute offsets.
            data.truncate(h);
            data.extend_from_slice(range(&roi.raw, old_h, roi.raw.len() - old_h)?);
            if data.len() < h + 64 {
                data.resize(h + 64, 0);
            }
            for offset in [16, 40, 48] {
                let old = int(&roi.raw, old_h + offset)? as usize;
                if old != 0 {
                    if old < old_h {
                        return Err("Cannot safely relocate unusual ImageJ metadata".into());
                    }
                    put_int(&mut data, h + offset, (h + old - old_h) as u32);
                }
            }
        }
    }
    data[..4].copy_from_slice(b"Iout");
    let version = short(&data, 4)?.max(228);
    put_short(&mut data, 4, version);
    data[6] = 10;
    put_short(&mut data, 16, if n <= 65535 { n as u16 } else { 0 });
    put_int(&mut data, 18, if n > 65535 { n as u32 } else { 0 });
    put_int(&mut data, 36, 0);
    put_short(&mut data, 48, 0);
    put_short(&mut data, 34, roi.size);
    data[52] = roi.point_type;
    put_int(&mut data, 40, roi.color.unwrap_or(0));
    let options = (short(&data, 50)? | 128) & !1024;
    put_short(
        &mut data,
        50,
        options | if roi.show_labels { 1024 } else { 0 },
    );
    put_int(&mut data, 56, roi.position);
    put_int(&mut data, 60, h as u32);
    put_int(&mut data, h + 4, roi.channel);
    put_int(&mut data, h + 8, roi.slice);
    put_int(&mut data, h + 12, roi.frame);
    let c = data.len();
    put_int(&mut data, h + 48, c as u32);
    data.resize(c + n * 4, 0);
    if data.len() > MAX_BYTES {
        return Err("ImageJ ROI exceeds size limit".into());
    }
    for (i, p) in roi.points.iter().enumerate() {
        put_int(&mut data, 64 + 4 * n + 4 * i, (p.x as f32).to_bits());
        put_int(&mut data, 64 + 8 * n + 4 * i, (p.y as f32).to_bits());
        put_int(&mut data, c + 4 * i, (p.position << 8) | p.counter as u32);
    }
    // Integer bounds are advisory; ImageJ reads the absolute float coordinates above.
    for offset in [8, 10, 12, 14] {
        put_short(&mut data, offset, 0);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> PointRoi {
        PointRoi {
            points: vec![Point {
                x: 123.25,
                y: 456.75,
                counter: 2,
                position: 7,
            }],
            point_type: 2,
            size: 3,
            color: Some(0xffff8000),
            show_labels: true,
            position: 0,
            channel: 0,
            slice: 0,
            frame: 0,
            raw: vec![],
        }
    }
    #[test]
    fn roundtrip_and_reject_truncation() {
        let bytes = encode(&sample()).unwrap();
        let decoded = decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded.points, sample().points);
        assert_eq!(encode(&decoded).unwrap(), bytes);
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err());
        }
        let mut corrupt = bytes.clone();
        put_int(&mut corrupt, 60, u32::MAX);
        assert!(decode(&corrupt).is_err());
        corrupt = bytes.clone();
        put_short(&mut corrupt, 16, 0);
        put_int(&mut corrupt, 18, u32::MAX);
        assert!(decode(&corrupt).is_err());
        corrupt = bytes;
        corrupt[6] = 1;
        assert!(decode(&corrupt).unwrap().is_none());
    }
    #[test]
    fn imagej_generated_fixture() {
        let bytes = include_bytes!("fixtures/imagej-points.roi");
        let mut roi = decode(bytes).unwrap().unwrap();
        assert_eq!(roi.points[0], sample().points[0]);
        assert_eq!(roi.point_type, 2);
        assert_eq!(roi.size, 3);
        assert_eq!(roi.color, Some(0xffff8000));
        assert_eq!(encode(&roi).unwrap(), bytes);
        roi.points.pop();
        let encoded = encode(&roi).unwrap();
        assert_eq!(decode(&encoded).unwrap().unwrap().points, roi.points);
    }

    #[test]
    fn editing_keeps_metadata() {
        let mut bytes = encode(&sample()).unwrap();
        let h = int(&bytes, 60).unwrap() as usize;
        let name = bytes.len();
        bytes.extend_from_slice(&[0, 65, 0, 66]);
        put_int(&mut bytes, h + 16, name as u32);
        put_int(&mut bytes, h + 20, 2);
        let mut roi = decode(&bytes).unwrap().unwrap();
        roi.points.push(Point {
            x: 3.5,
            y: 9.25,
            counter: 4,
            position: 2,
        });
        let saved = encode(&roi).unwrap();
        let h = int(&saved, 60).unwrap() as usize;
        let name = int(&saved, h + 16).unwrap() as usize;
        assert_eq!(&saved[name..name + 4], &[0, 65, 0, 66]);
        assert_eq!(decode(&saved).unwrap().unwrap().points, roi.points);
    }
}
