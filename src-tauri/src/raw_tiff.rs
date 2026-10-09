//! Bounded, full-coverage area preview for ordinary uncompressed/LZW/Deflate TIFFs.
//! Unsupported layouts are deliberately left to the general TIFF decoder.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

#[link(name = "tiff")]
unsafe extern "C" {
    fn TIFFOpenOptionsAlloc() -> *mut std::ffi::c_void;
    fn TIFFOpenOptionsFree(options: *mut std::ffi::c_void);
    fn TIFFOpenOptionsSetMaxSingleMemAlloc(options: *mut std::ffi::c_void, bytes: isize);
    fn TIFFOpenExt(
        path: *const std::ffi::c_char,
        mode: *const std::ffi::c_char,
        options: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn TIFFClose(image: *mut std::ffi::c_void);
    fn TIFFReadEncodedStrip(
        image: *mut std::ffi::c_void,
        strip: u32,
        output: *mut u8,
        bytes: isize,
    ) -> isize;
}

struct Tiff(*mut std::ffi::c_void);

impl Tiff {
    fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid())?;
        // Every worker owns an independent native handle. Disable memory mapping
        // and cap individual native allocations as well as our decoded strip.
        let image = unsafe {
            let options = TIFFOpenOptionsAlloc();
            if options.is_null() {
                return Err(io::Error::other("cannot allocate TIFF decoder options"));
            }
            TIFFOpenOptionsSetMaxSingleMemAlloc(options, MAX_COMPRESSED_STRIP_BYTES as isize);
            let image = TIFFOpenExt(path.as_ptr(), c"rm".as_ptr(), options);
            TIFFOpenOptionsFree(options);
            image
        };
        if image.is_null() {
            return Err(invalid());
        }
        Ok(Self(image))
    }

    fn decode(&mut self, strip: usize, output: &mut [u8]) -> io::Result<()> {
        // The validated strip size bounds this live, writable slice. libtiff
        // reverses the predictor and returns samples in native byte order.
        let size = unsafe {
            TIFFReadEncodedStrip(
                self.0,
                strip as u32,
                output.as_mut_ptr(),
                output.len() as isize,
            )
        };
        if size != output.len() as isize {
            return Err(invalid());
        }
        Ok(())
    }
}

impl Drop for Tiff {
    fn drop(&mut self) {
        unsafe { TIFFClose(self.0) }
    }
}

const MAX_COMPRESSED_STRIP_BYTES: u64 = 16 * 1024 * 1024;

pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub bands: u32,
    pub bits: u16,
    /// Unsigned samples in native byte order, suitable for a libvips memory image.
    pub pixels: Vec<u8>,
}

struct Layout {
    width: usize,
    height: usize,
    bands: usize,
    bits: u16,
    little: bool,
    compressed: bool,
    rows_per_strip: usize,
    strips: Vec<(u64, usize, usize)>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid TIFF preview layout")
}

fn number(bytes: &[u8], little: bool) -> u64 {
    if little {
        bytes.iter().rev().fold(0, |n, b| (n << 8) | u64::from(*b))
    } else {
        bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
    }
}

fn read_at(file: &File, offset: u64, bytes: &mut [u8], len: u64) -> io::Result<()> {
    if offset
        .checked_add(bytes.len() as u64)
        .filter(|v| *v <= len)
        .is_none()
    {
        return Err(invalid());
    }
    file.read_exact_at(bytes, offset)
}

fn parse(file: &File) -> io::Result<Option<Layout>> {
    let len = file.metadata()?.len();
    if len < 8 {
        return Ok(None);
    }
    let mut header = [0; 16];
    read_at(file, 0, &mut header[..8], len)?;
    let little = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Ok(None),
    };
    let big = match number(&header[2..4], little) {
        42 => false,
        43 => {
            if number(&header[4..6], little) != 8 || number(&header[6..8], little) != 0 {
                return Ok(None);
            }
            read_at(file, 8, &mut header[8..], len)?;
            true
        }
        _ => return Ok(None),
    };
    let word = if big { 8 } else { 4 };
    let count_size = if big { 8 } else { 2 };
    let entry_size = if big { 20 } else { 12 };
    let directory = number(
        &header[if big { 8 } else { 4 }..if big { 16 } else { 8 }],
        little,
    );
    let mut count = [0; 8];
    read_at(file, directory, &mut count[..count_size], len)?;
    let count = number(&count[..count_size], little);
    if count == 0 || count > 256 {
        return Ok(None);
    }
    let mut entries = vec![0; count as usize * entry_size + word];
    read_at(
        file,
        directory
            .checked_add(count_size as u64)
            .ok_or_else(invalid)?,
        &mut entries,
        len,
    )?;
    if number(&entries[count as usize * entry_size..], little) != 0 {
        return Ok(None); // Multi-page/channel semantics belong to the general reader.
    }
    let mut tags = BTreeMap::<u16, Vec<u64>>::new();
    let mut metadata_bytes = Some(0_u64);
    for entry in entries[..count as usize * entry_size].chunks_exact(entry_size) {
        let tag = number(&entry[..2], little) as u16;
        // libtiff reads descriptive tags too. Bound every directory field before
        // opening native handles, including fields the pixel parser ignores.
        let metadata_size = match number(&entry[2..4], little) {
            1 | 2 | 6 | 7 => Some(1),
            3 | 8 => Some(2),
            4 | 9 | 11 | 13 => Some(4),
            5 | 10 | 12 => Some(8),
            16..=18 if big => Some(8),
            _ => None,
        };
        metadata_bytes = metadata_bytes.and_then(|total| {
            let bytes = number(&entry[4..4 + word], little).checked_mul(metadata_size?)?;
            total.checked_add(bytes)
        });
        if !matches!(
            tag,
            256 | 257
                | 258
                | 259
                | 262
                | 266
                | 273
                | 274
                | 277
                | 278
                | 279
                | 284
                | 317
                | 322
                | 323
                | 324
                | 325
                | 330
                | 338
                | 339
        ) {
            continue;
        }
        if tags.contains_key(&tag) {
            return Ok(None);
        }
        let size = match number(&entry[2..4], little) {
            3 => 2,
            4 => 4,
            16 if big => 8,
            _ => return Ok(None),
        };
        let n = number(&entry[4..4 + word], little);
        let Some(bytes) = n.checked_mul(size) else {
            return Ok(None);
        };
        if bytes == 0 || bytes > 1024 * 1024 {
            return Ok(None);
        }
        let mut data = vec![0; bytes as usize];
        let value = &entry[4 + word..4 + word * 2];
        if bytes <= word as u64 {
            data.copy_from_slice(&value[..bytes as usize]);
        } else {
            read_at(file, number(value, little), &mut data, len)?;
        }
        tags.insert(
            tag,
            data.chunks_exact(size as usize)
                .map(|v| number(v, little))
                .collect(),
        );
    }
    let scalar = |tag, default| match tags.get(&tag) {
        None => Some(default),
        Some(v) if v.len() == 1 => Some(v[0]),
        _ => None,
    };
    let (Some(width), Some(height), Some(bands), Some(rows)) = (
        scalar(256, 0),
        scalar(257, 0),
        scalar(277, 1),
        scalar(278, u64::MAX),
    ) else {
        return Ok(None);
    };
    let compression = scalar(259, 1);
    let compressed = matches!(compression, Some(5 | 8 | 32946));
    if compressed && metadata_bytes.is_none_or(|bytes| bytes > 4 * 1024 * 1024) {
        return Ok(None);
    }
    let predictor = scalar(317, 1);
    if width == 0
        || height == 0
        || width > 1_000_000
        || height > 1_000_000
        || rows == 0
        || !matches!(bands, 1 | 3)
        || !matches!(compression, Some(1 | 5 | 8 | 32946))
        || scalar(266, 1) != Some(1)
        || scalar(274, 1) != Some(1)
        || scalar(284, 1) != Some(1)
        || !matches!(predictor, Some(1 | 2))
        || (!compressed && predictor != Some(1))
        || scalar(262, 0) != Some(if bands == 1 { 1 } else { 2 })
        || [322, 323, 324, 325, 330, 338]
            .iter()
            .any(|tag| tags.contains_key(tag))
    {
        return Ok(None);
    }
    if tags
        .get(&339)
        .is_some_and(|v| v.len() != bands as usize || v.iter().any(|n| *n != 1))
    {
        return Ok(None);
    }
    let Some(bits) = tags.get(&258) else {
        return Ok(None);
    };
    if bits.len() != bands as usize
        || !matches!(bits[0], 8 | 16)
        || bits.iter().any(|b| *b != bits[0])
    {
        return Ok(None);
    }
    let row_bytes = width
        .checked_mul(bands)
        .and_then(|v| v.checked_mul(bits[0] / 8))
        .ok_or_else(invalid)?;
    if row_bytes > 16 * 1024 * 1024 {
        return Ok(None);
    }
    let rows = rows.min(height);
    let expected = height.div_ceil(rows) as usize;
    let (Some(offsets), Some(counts)) = (tags.get(&273), tags.get(&279)) else {
        return Ok(None);
    };
    if offsets.len() != expected || counts.len() != expected {
        return Ok(None);
    }
    let mut strips = Vec::with_capacity(expected);
    for (index, (&offset, &count)) in offsets.iter().zip(counts).enumerate() {
        let strip_rows = rows.min(height - index as u64 * rows);
        let required = strip_rows.checked_mul(row_bytes).ok_or_else(invalid)?;
        if count == 0
            || (!compressed && count != required)
            || (compressed
                && (count > MAX_COMPRESSED_STRIP_BYTES || required > MAX_COMPRESSED_STRIP_BYTES))
            || offset.checked_add(count).is_none_or(|v| v > len)
        {
            return Ok(None);
        }
        strips.push((offset, strip_rows as usize, count as usize));
    }
    Ok(Some(Layout {
        width: width as usize,
        height: height as usize,
        bands: bands as usize,
        bits: bits[0] as u16,
        little,
        compressed,
        rows_per_strip: rows as usize,
        strips,
    }))
}

/// A faithful area average of every source pixel. No spatial sampling is used.
#[cfg(test)]
pub fn preview(path: &Path, max_edge: u32) -> io::Result<Option<Preview>> {
    preview_with_threads(
        path,
        max_edge,
        std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(4),
    )
}

pub fn preview_with_threads(
    path: &Path,
    max_edge: u32,
    threads: usize,
) -> io::Result<Option<Preview>> {
    let file = File::open(path)?;
    let Some(layout) = parse(&file)? else {
        return Ok(None);
    };
    if max_edge == 0 || max_edge > 2048 {
        return Err(invalid());
    }
    let scale = (f64::from(max_edge) / layout.width.max(layout.height) as f64).min(1.0);
    let width = ((layout.width as f64 * scale).round() as usize).max(1);
    let height = ((layout.height as f64 * scale).round() as usize).max(1);
    let sx = layout.width as f64 / width as f64;
    let sy = layout.height as f64 / height as f64;
    let threads = threads
        .clamp(1, if layout.compressed { 16 } else { 8 })
        .min(height);
    let sums = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for index in 0..threads {
            let begin_y = height * index / threads;
            let end_y = height * (index + 1) / threads;
            let layout = &layout;
            let file = &file;
            workers.push(scope.spawn(move || {
                let decoder = layout.compressed.then(|| Tiff::open(path)).transpose()?;
                reduce_rows(file, layout, width, sx, sy, (begin_y, end_y), decoder)
            }));
        }
        workers
            .into_iter()
            .map(|worker| worker.join().map_err(|_| invalid())?)
            .collect::<io::Result<Vec<_>>>()
    })?;
    let result = sums.into_iter().flatten();
    let sample_bytes = usize::from(layout.bits / 8);
    let mut pixels = Vec::with_capacity(width * height * layout.bands * sample_bytes);
    for sum in result {
        let value = (sum / (sx * sy)).round();
        if sample_bytes == 1 {
            pixels.push(value.clamp(0.0, 255.0) as u8)
        } else {
            pixels.extend_from_slice(&(value.clamp(0.0, 65535.0) as u16).to_ne_bytes())
        }
    }
    Ok(Some(Preview {
        width: width as u32,
        height: height as u32,
        bands: layout.bands as u32,
        bits: layout.bits,
        pixels,
    }))
}

fn reduce_rows(
    file: &File,
    layout: &Layout,
    width: usize,
    sx: f64,
    sy: f64,
    (begin_y, end_y): (usize, usize),
    mut decoder: Option<Tiff>,
) -> io::Result<Vec<f64>> {
    let mut result = vec![0.0_f64; width * (end_y - begin_y) * layout.bands];
    let mut prefix = vec![0_u64; (layout.width + 1) * layout.bands];
    let sample_bytes = usize::from(layout.bits / 8);
    let row_bytes = layout.width * layout.bands * sample_bytes;
    let batch_rows = (4 * 1024 * 1024 / row_bytes).max(1);
    let mut batch = if layout.compressed {
        Vec::new()
    } else {
        vec![0; batch_rows * row_bytes]
    };
    let mut decoded = Vec::new();
    let little = if layout.compressed {
        cfg!(target_endian = "little")
    } else {
        layout.little
    };
    let file_len = file.metadata()?.len();
    let source_begin = (begin_y as f64 * sy).floor() as usize;
    let source_end = ((end_y as f64 * sy).ceil() as usize).min(layout.height);
    for (strip, &(offset, rows, _count)) in layout.strips.iter().enumerate() {
        let strip_begin = strip * layout.rows_per_strip;
        let start_row = source_begin.saturating_sub(strip_begin).min(rows);
        let end_row = source_end.saturating_sub(strip_begin).min(rows);
        if start_row == end_row {
            continue;
        }
        if let Some(decoder) = &mut decoder {
            decoded.resize(rows * row_bytes, 0);
            decoder.decode(strip, &mut decoded)?;
        }
        for start in (start_row..end_row).step_by(batch_rows) {
            let n = batch_rows.min(end_row - start);
            let bytes = if layout.compressed {
                &decoded[start * row_bytes..(start + n) * row_bytes]
            } else {
                read_at(
                    file,
                    offset + (start * row_bytes) as u64,
                    &mut batch[..n * row_bytes],
                    file_len,
                )?;
                &batch[..n * row_bytes]
            };
            for (local_y, row) in bytes.chunks_exact(row_bytes).enumerate() {
                match (layout.bands, sample_bytes, little) {
                    (1, 1, _) => {
                        let mut sum = 0;
                        for (x, &sample) in row.iter().enumerate() {
                            sum += u64::from(sample);
                            prefix[x + 1] = sum;
                        }
                    }
                    (1, 2, true) => {
                        let mut sum = 0;
                        for (x, sample) in row.as_chunks::<2>().0.iter().enumerate() {
                            sum += u64::from(u16::from_le_bytes([sample[0], sample[1]]));
                            prefix[x + 1] = sum;
                        }
                    }
                    (1, 2, false) => {
                        let mut sum = 0;
                        for (x, sample) in row.as_chunks::<2>().0.iter().enumerate() {
                            sum += u64::from(u16::from_be_bytes([sample[0], sample[1]]));
                            prefix[x + 1] = sum;
                        }
                    }
                    _ => {
                        let mut sums = [0; 3];
                        for x in 0..layout.width {
                            for b in 0..3 {
                                let i = (x * 3 + b) * sample_bytes;
                                let value = if sample_bytes == 1 {
                                    u64::from(row[i])
                                } else {
                                    number(&row[i..i + 2], little)
                                };
                                sums[b] += value;
                                prefix[(x + 1) * 3 + b] = sums[b];
                            }
                        }
                    }
                }
                let y = strip * layout.rows_per_strip + start + local_y;
                let first = ((y as f64 / sy).floor() as usize).max(begin_y).min(end_y);
                let last = (((y + 1) as f64 / sy).ceil() as usize).min(end_y);
                for oy in first..last {
                    let weight =
                        ((y + 1) as f64).min((oy + 1) as f64 * sy) - (y as f64).max(oy as f64 * sy);
                    if weight <= 0.0 {
                        continue;
                    }
                    for ox in 0..width {
                        let left = ox as f64 * sx;
                        let right = ((ox + 1) as f64 * sx).min(layout.width as f64);
                        let a = left.floor() as usize;
                        let z = right.floor() as usize;
                        for b in 0..layout.bands {
                            let p = |index| prefix[index * layout.bands + b] as f64;
                            let left_sum = p(a) + (left - a as f64) * (p(a + 1) - p(a));
                            let right_sum = p(z)
                                + if z < layout.width {
                                    (right - z as f64) * (p(z + 1) - p(z))
                                } else {
                                    0.0
                                };
                            result[((oy - begin_y) * width + ox) * layout.bands + b] +=
                                (right_sum - left_sum) * weight;
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[link(name = "tiff")]
    unsafe extern "C" {
        fn TIFFSetField(image: *mut std::ffi::c_void, tag: u32, ...) -> std::ffi::c_int;
        fn TIFFWriteEncodedStrip(
            image: *mut std::ffi::c_void,
            strip: u32,
            input: *mut u8,
            bytes: isize,
        ) -> isize;
    }

    fn compressed_fixture(
        config: (bool, bool, u16, u16, bool, u16),
        width: usize,
        height: usize,
        rows: usize,
        values: &[u16],
    ) -> Fixture {
        use std::os::unix::ffi::OsStrExt;
        let (big, little, bands, bits, predictor, compression) = config;
        let f = Fixture(std::env::temp_dir().join(format!(
            "aperture-compressed-{}-{}.tif",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let path = std::ffi::CString::new(f.0.as_os_str().as_bytes()).unwrap();
        let mode = std::ffi::CString::new(format!(
            "w{}{}",
            if big { "8" } else { "" },
            if little { "l" } else { "b" }
        ))
        .unwrap();
        // Exercise the same native codecs using independently generated sources.
        unsafe {
            let image = Tiff(TIFFOpenExt(
                path.as_ptr(),
                mode.as_ptr(),
                std::ptr::null_mut(),
            ));
            assert!(!image.0.is_null());
            for (tag, value) in [
                (256, width as u32),
                (257, height as u32),
                (278, rows as u32),
            ] {
                assert_eq!(TIFFSetField(image.0, tag, value), 1);
            }
            for (tag, value) in [
                (258, bits),
                (259, compression),
                (262, if bands == 1 { 1 } else { 2 }),
                (277, bands),
                (284, 1),
                (317, if predictor { 2 } else { 1 }),
            ] {
                assert_eq!(TIFFSetField(image.0, tag, std::ffi::c_int::from(value)), 1);
            }
            for (strip, values) in values.chunks(width * bands as usize * rows).enumerate() {
                let mut bytes = values
                    .iter()
                    .flat_map(|v| {
                        if bits == 8 {
                            vec![*v as u8]
                        } else {
                            v.to_ne_bytes().to_vec()
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    TIFFWriteEncodedStrip(
                        image.0,
                        strip as u32,
                        bytes.as_mut_ptr(),
                        bytes.len() as isize
                    ),
                    bytes.len() as isize
                );
            }
        }
        // Relocate strips with gaps so decoding cannot assume contiguous bytes.
        // The oversized-strip fixture deliberately exceeds the preview bound.
        if let Some(layout) = parse(&File::open(&f.0).unwrap()).unwrap() {
            let mut data = std::fs::read(&f.0).unwrap();
            let word = if big { 8 } else { 4 };
            let count_size = if big { 8 } else { 2 };
            let entry_size = if big { 20 } else { 12 };
            let directory = number(
                &data[if big { 8 } else { 4 }..if big { 16 } else { 8 }],
                little,
            ) as usize;
            let count = number(&data[directory..directory + count_size], little) as usize;
            let entry = (0..count)
                .map(|i| directory + count_size + i * entry_size)
                .find(|i| number(&data[*i..*i + 2], little) == 273)
                .unwrap();
            let size = match number(&data[entry + 2..entry + 4], little) {
                3 => 2,
                4 => 4,
                16 => 8,
                _ => unreachable!(),
            };
            let field = entry + 4 + word;
            let offsets = if size * layout.strips.len() <= word {
                field
            } else {
                number(&data[field..field + word], little) as usize
            };
            for (i, &(offset, _, bytes)) in layout.strips.iter().enumerate() {
                let strip = data[offset as usize..offset as usize + bytes].to_vec();
                data.extend_from_slice(&[0; 3]);
                let offset = if little {
                    (data.len() as u64).to_le_bytes()
                } else {
                    (data.len() as u64).to_be_bytes()
                };
                data[offsets + i * size..offsets + (i + 1) * size].copy_from_slice(if little {
                    &offset[..size]
                } else {
                    &offset[8 - size..]
                });
                data.extend_from_slice(&strip);
            }
            std::fs::write(&f.0, data).unwrap();
        }
        f
    }

    #[test]
    fn compressed_strips_predictor_endian_rgb_and_thread_partitions_match_full_area() {
        for big in [false, true] {
            for little in [false, true] {
                for bands in [1, 3] {
                    for bits in [8, 16] {
                        for predictor in [false, true] {
                            for compression in [5, 8, 32946] {
                                let values = (0..19 * 23 * bands)
                                    .map(|i| ((u32::from(i) * 1031) % (1_u32 << bits)) as u16)
                                    .collect::<Vec<_>>();
                                let f = compressed_fixture(
                                    (big, little, bands, bits, predictor, compression),
                                    19,
                                    23,
                                    3,
                                    &values,
                                );
                                let full = preview_with_threads(&f.0, 23, 16).unwrap().unwrap();
                                let expected = values
                                    .iter()
                                    .flat_map(|v| {
                                        if bits == 8 {
                                            vec![*v as u8]
                                        } else {
                                            v.to_ne_bytes().to_vec()
                                        }
                                    })
                                    .collect::<Vec<_>>();
                                assert_eq!(full.pixels, expected);
                                let reference = preview_with_threads(&f.0, 19, 1).unwrap().unwrap();
                                for threads in [2, 4, 8, 16] {
                                    assert_eq!(
                                        reference.pixels,
                                        preview_with_threads(&f.0, 19, threads)
                                            .unwrap()
                                            .unwrap()
                                            .pixels
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn corrupt_and_wrong_size_compressed_data_are_errors() {
        let f = compressed_fixture((false, true, 1, 8, false, 32946), 2, 2, 2, &[1, 2, 3, 4]);
        assert_eq!(preview(&f.0, 2).unwrap().unwrap().pixels, [1, 2, 3, 4]);
        let mut bytes = std::fs::read(&f.0).unwrap();
        let layout = parse(&File::open(&f.0).unwrap()).unwrap().unwrap();
        bytes[layout.strips[0].0 as usize] ^= 0xff;
        std::fs::write(&f.0, bytes).unwrap();
        assert!(preview(&f.0, 2).is_err());
        let f = compressed_fixture((false, true, 1, 8, false, 8), 2, 2, 2, &[1, 2, 3]);
        assert!(preview(&f.0, 2).is_err());
    }

    #[test]
    fn oversized_compressed_strip_falls_back_before_allocation() {
        let f = compressed_fixture((true, true, 1, 16, false, 8), 1_000_000, 17, 17, &[1]);
        assert!(preview(&f.0, 1).unwrap().is_none());
    }

    #[test]
    fn ignored_metadata_is_bounded_before_opening_compressed_native_handles() {
        for (kind, count) in [(2_u16, 2 * 1024 * 1024 + 1), (15, 1), (12, u32::MAX)] {
            let f = fixture(false, true, 2, 2, 8, &[1, 2, 3, 4], None);
            let mut bytes = std::fs::read(&f.0).unwrap();
            let n = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
            let mut compression = 0;
            for i in 0..n {
                let entry = 10 + i * 12;
                let tag = number(&bytes[entry..entry + 2], true);
                if tag == 259 {
                    compression = entry + 8;
                }
                if matches!(tag, 266 | 339) {
                    bytes[entry..entry + 2].copy_from_slice(&(65000 + i as u16).to_le_bytes());
                    bytes[entry + 2..entry + 4].copy_from_slice(&kind.to_le_bytes());
                    bytes[entry + 4..entry + 8].copy_from_slice(&count.to_le_bytes());
                    bytes[entry + 8..entry + 12].copy_from_slice(&4096_u32.to_le_bytes());
                }
            }
            std::fs::write(&f.0, &bytes).unwrap();
            File::options()
                .write(true)
                .open(&f.0)
                .unwrap()
                .set_len(6 * 1024 * 1024)
                .unwrap();
            assert_eq!(preview(&f.0, 2).unwrap().unwrap().pixels, [1, 2, 3, 4]);
            bytes[compression..compression + 2].copy_from_slice(&8_u16.to_le_bytes());
            std::fs::write(&f.0, &bytes).unwrap();
            File::options()
                .write(true)
                .open(&f.0)
                .unwrap()
                .set_len(6 * 1024 * 1024)
                .unwrap();
            assert!(preview(&f.0, 2).unwrap().is_none());
        }
    }

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    // Generates both TIFF directory representations and endian orders.
    fn fixture(
        big: bool,
        little: bool,
        width: u32,
        height: u32,
        bits: u16,
        values: &[u16],
        changed: Option<(u16, u64)>,
    ) -> Fixture {
        let word = if big { 8 } else { 4 };
        let entry_size = if big { 20 } else { 12 };
        let header_size = if big { 16 } else { 8 };
        let count_size = if big { 8 } else { 2 };
        let mut tags = vec![
            (256, 4, width as u64),
            (257, 4, height as u64),
            (258, 3, bits as u64),
            (259, 3, 1),
            (262, 3, 1),
            (266, 3, 1),
            (273, if big { 16 } else { 4 }, 0),
            (274, 3, 1),
            (277, 3, 1),
            (278, 4, height as u64),
            (
                279,
                if big { 16 } else { 4 },
                (width * height * (u32::from(bits) / 8)) as u64,
            ),
            (284, 3, 1),
            (339, 3, 1),
        ];
        let offset = header_size + count_size + tags.len() * entry_size + word;
        tags.iter_mut().find(|t| t.0 == 273).unwrap().2 = offset as u64;
        if let Some((tag, value)) = changed {
            tags.iter_mut().find(|t| t.0 == tag).unwrap().2 = value;
        }
        let mut data = Vec::new();
        let push = |out: &mut Vec<u8>, value: u64, size: usize| {
            let raw = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            out.extend_from_slice(if little {
                &raw[..size]
            } else {
                &raw[8 - size..]
            });
        };
        data.extend_from_slice(if little { b"II" } else { b"MM" });
        push(&mut data, if big { 43 } else { 42 }, 2);
        if big {
            push(&mut data, 8, 2);
            push(&mut data, 0, 2);
        }
        push(&mut data, header_size as u64, word);
        push(&mut data, tags.len() as u64, count_size);
        for (tag, kind, value) in tags {
            push(&mut data, u64::from(tag), 2);
            push(&mut data, kind, 2);
            push(&mut data, 1, word);
            let size = if kind == 3 {
                2
            } else if kind == 4 {
                4
            } else {
                8
            };
            push(&mut data, value, size);
            data.resize(data.len() + word - size, 0);
        }
        push(&mut data, 0, word);
        for &value in values {
            push(&mut data, value as u64, (bits / 8) as usize);
        }
        let path = std::env::temp_dir().join(format!(
            "aperture-raw-{}-{}.tif",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, data).unwrap();
        Fixture(path)
    }

    #[test]
    fn both_directory_sizes_and_byte_orders_preserve_unsigned_16_bit() {
        for big in [false, true] {
            for little in [false, true] {
                let f = fixture(
                    big,
                    little,
                    3,
                    2,
                    16,
                    &[0, 256, 65535, 1234, 3456, 54321],
                    None,
                );
                let image = preview(&f.0, 1024).unwrap().unwrap();
                let values: Vec<_> = image
                    .pixels
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|v| u16::from_ne_bytes([v[0], v[1]]))
                    .collect();
                assert_eq!(values, [0, 256, 65535, 1234, 3456, 54321]);
                assert_eq!(
                    (image.width, image.height, image.bands, image.bits),
                    (3, 2, 1, 16)
                );
            }
        }
    }

    #[test]
    fn fractional_area_grid_covers_last_row_and_column() {
        let f = fixture(false, true, 5, 3, 8, &(0..15).collect::<Vec<_>>(), None);
        assert_eq!(
            preview_with_threads(&f.0, 2, 1).unwrap().unwrap().pixels,
            [6, 8]
        );
        let f = fixture(
            false,
            true,
            5,
            3,
            8,
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255],
            None,
        );
        assert_eq!(preview(&f.0, 2).unwrap().unwrap().pixels, [0, 34]);
    }

    #[test]
    fn parallel_partitioning_is_identical_to_single_thread() {
        let values = (0..323)
            .map(|v| ((v * 37) % 256) as u16)
            .collect::<Vec<_>>();
        let f = fixture(true, false, 19, 17, 8, &values, None);
        let single = preview_with_threads(&f.0, 7, 1).unwrap().unwrap();
        for threads in [2, 4, 8] {
            assert_eq!(
                single.pixels,
                preview_with_threads(&f.0, 7, threads)
                    .unwrap()
                    .unwrap()
                    .pixels
            );
        }
    }

    #[test]
    fn rgb_channels_and_non_contiguous_strips_are_preserved() {
        let mut bytes = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        let tags = [
            (256_u16, 4_u16, 1_u32, 2_u32),
            (257, 4, 1, 2),
            (258, 3, 3, 146),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, 2, 152),
            (274, 3, 1, 1),
            (277, 3, 1, 3),
            (278, 4, 1, 1),
            (279, 4, 2, 160),
            (284, 3, 1, 1),
        ];
        bytes.extend_from_slice(&(tags.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in tags {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&kind.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(bytes.len(), 146);
        bytes.extend_from_slice(&[8, 0, 8, 0, 8, 0]);
        for value in [168_u32, 181, 6, 6] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 10, 20, 30, 40, 50]);
        bytes.extend_from_slice(&[0; 7]);
        bytes.extend_from_slice(&[60, 70, 80, 90, 100, 110]);
        let f = Fixture(std::env::temp_dir().join(format!(
            "aperture-raw-rgb-{}-{}.tif",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::write(&f.0, bytes).unwrap();
        assert_eq!(preview(&f.0, 1).unwrap().unwrap().pixels, [45, 55, 65]);
        assert_eq!(
            preview(&f.0, 2).unwrap().unwrap().pixels,
            [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110]
        );
    }

    #[test]
    fn incompatible_layouts_fall_back_instead_of_guessing() {
        for changed in [
            (259, 7),
            (262, 0),
            (266, 2),
            (274, 6),
            (277, 2),
            (284, 2),
            (339, 3),
            (273, u32::MAX as u64),
            (279, 1),
        ] {
            let f = fixture(false, true, 2, 2, 8, &[1, 2, 3, 4], Some(changed));
            assert!(preview(&f.0, 1024).unwrap().is_none(), "tag {}", changed.0);
        }
    }

    #[test]
    fn truncated_directory_and_overflowing_offset_are_errors() {
        let f = fixture(true, true, 2, 2, 8, &[1, 2, 3, 4], None);
        let mut bytes = std::fs::read(&f.0).unwrap();
        bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&f.0, &bytes).unwrap();
        assert!(preview(&f.0, 1024).is_err());
        std::fs::write(&f.0, &bytes[..16]).unwrap();
        assert!(preview(&f.0, 1024).is_err());
    }
}
