//! Bound compressed LV metadata before the pinned reader expands it into JSON.
//! This walks the 0.3.0 LV layout without allocating names or decoded values.
use flate2::read::ZlibDecoder;
use std::io::Read;

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

/// Count the encoded input and every recursively inflated LV buffer. Unknown or
/// truncated opaque data stops shape inspection, as the reader's fallback does;
/// any compression already encountered must still satisfy the shared bound.
pub(crate) fn expanded_size(raw: &[u8]) -> Result<u64, String> {
    let mut used = raw.len() as u64;
    if used > MAX_BYTES {
        return Err("ND2 metadata exceeds the 32 MiB expanded input limit.".into());
    }
    // Named LV chunks bypass decode_any: their valid binary headers can resemble
    // whitespace followed by XML. Always walk LV; ordinary XML stops as syntax.
    match walk(raw, 0, &mut used) {
        Ok(()) | Err(Stop::Syntax) => Ok(used),
        Err(Stop::Limit(error)) => Err(error),
    }
}

enum Stop {
    Syntax,
    Limit(String),
}

enum Item {
    Named(usize),
    Compressed(Box<[u8]>),
}

fn walk(buf: &[u8], depth: usize, used: &mut u64) -> Result<(), Stop> {
    let mut pos = 0;
    while pos < buf.len() {
        match item(buf, pos, depth, used)? {
            Item::Named(next) => pos = next,
            Item::Compressed(inner) => {
                walk(&inner, depth + 1, used)?;
                break;
            }
        }
    }
    Ok(())
}

fn take(buf: &[u8], pos: usize, length: usize) -> Result<&[u8], Stop> {
    pos.checked_add(length)
        .and_then(|end| buf.get(pos..end))
        .ok_or(Stop::Syntax)
}

fn item(buf: &[u8], pos: usize, depth: usize, used: &mut u64) -> Result<Item, Stop> {
    let header = take(buf, pos, 2)?;
    let kind = header[0];
    if kind == 76 {
        nesting(depth)?;
        // Type 76 ignores name length; its zlib stream follows a ten-byte header.
        let packed = buf.get(pos + 10..).unwrap_or_default();
        let remaining = MAX_BYTES - *used;
        // Exact preallocation prevents read_to_end's geometric growth exceeding
        // the validator's reservation. Box the result before nested inflation so
        // unused capacity is not retained at every compression level. During the
        // conversion, old/new buffers together fit the 64 MiB validation allowance.
        let mut inflated = Vec::with_capacity(remaining as usize + 1);
        ZlibDecoder::new(packed)
            .take(remaining + 1)
            .read_to_end(&mut inflated)
            .map_err(|error| Stop::Limit(format!("ND2 compressed metadata is invalid: {error}")))?;
        if inflated.len() as u64 > remaining {
            return Err(Stop::Limit(
                "ND2 metadata exceeds the 32 MiB expanded input limit.".into(),
            ));
        }
        *used += inflated.len() as u64;
        return Ok(Item::Compressed(inflated.into_boxed_slice()));
    }
    let mut p = pos + 2;
    let name_bytes = usize::from(header[1]) * 2;
    take(buf, p, name_bytes)?;
    p += name_bytes;
    match kind {
        1..=7 => {
            let length = match kind {
                1 => 1,
                2 | 3 => 4,
                _ => 8,
            };
            take(buf, p, length)?;
            p += length;
        }
        8 => {
            while p + 1 < buf.len() && buf[p..p + 2] != [0, 0] {
                p += 2;
            }
            p = (p + 2).min(buf.len());
        }
        9 => {
            let length = u64::from_le_bytes(take(buf, p, 8)?.try_into().unwrap());
            let length = usize::try_from(length).map_err(|_| Stop::Syntax)?;
            p += 8;
            let bytes = take(buf, p, length)?;
            let plausible = bytes.len() >= 4
                && (bytes[0] == 76 || ((1..=11).contains(&bytes[0]) && bytes[1] >= 2));
            if plausible && depth < MAX_DEPTH {
                match walk(bytes, depth + 1, used) {
                    Ok(()) | Err(Stop::Syntax) => {}
                    // The library swallows speculative byte-array parse errors,
                    // but it has already inflated the bytes by then.
                    Err(error @ Stop::Limit(_)) => return Err(error),
                }
            }
            p += length;
        }
        11 => {
            nesting(depth)?;
            let count = u32::from_le_bytes(take(buf, p, 12)?[..4].try_into().unwrap()) as usize;
            p += 12;
            for _ in 0..count {
                if p >= buf.len() {
                    break;
                }
                match item(buf, p, depth + 1, used)? {
                    Item::Named(next) => p = next,
                    Item::Compressed(inner) => {
                        // This is depth+1 (rather than +2) in pinned lv.rs.
                        walk(&inner, depth + 1, used)?;
                        p = buf.len();
                    }
                }
            }
            // Child offsets are trailing data, not additional LV items.
            p = p.saturating_add(count.saturating_mul(8)).min(buf.len());
        }
        _ => return Err(Stop::Syntax),
    }
    Ok(Item::Named(p))
}

fn nesting(depth: usize) -> Result<(), Stop> {
    if depth >= MAX_DEPTH {
        Err(Stop::Limit(
            "ND2 metadata nesting exceeds 64 levels.".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn scalar() -> Vec<u8> {
        // Named unsigned value "n" = 7.
        let mut raw = vec![3, 2, b'n', 0, 0, 0];
        raw.extend(7u32.to_le_bytes());
        raw
    }

    fn compressed(raw: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(raw).unwrap();
        let mut result = vec![76; 10];
        result.extend(encoder.finish().unwrap());
        result
    }

    fn level(child: &[u8]) -> Vec<u8> {
        let mut raw = vec![11, 2, b'L', 0, 0, 0];
        raw.extend(1u32.to_le_bytes());
        raw.extend(0u64.to_le_bytes());
        raw.extend(child);
        raw.extend(0u64.to_le_bytes());
        raw
    }

    fn byte_array(child: &[u8]) -> Vec<u8> {
        let mut raw = vec![9, 2, b'b', 0, 0, 0];
        raw.extend((child.len() as u64).to_le_bytes());
        raw.extend(child);
        raw
    }

    fn xml_like_byte_array(child: &[u8]) -> Vec<u8> {
        // Both header bytes are TAB; the UTF-16 name starts with '<'.
        let mut raw = vec![9, 9];
        for unit in "<payload\0".encode_utf16() {
            raw.extend(unit.to_le_bytes());
        }
        raw.extend((child.len() as u64).to_le_bytes());
        raw.extend(child);
        raw
    }

    #[test]
    fn xml_like_lv_headers_still_account_for_nested_compression() {
        let leaf = scalar();
        let raw = xml_like_byte_array(&compressed(&leaf));
        assert_eq!(
            openreadout_nd2::lv_decode(&raw).unwrap()["<payload"]["n"],
            7
        );
        assert_eq!(
            expanded_size(&raw).unwrap(),
            (raw.len() + leaf.len()) as u64
        );
    }

    #[test]
    fn counts_compression_at_root_level_byte_array_and_nested_blocks() {
        let leaf = scalar();
        let packed = compressed(&leaf);
        for raw in [packed.clone(), level(&packed), byte_array(&packed)] {
            assert_eq!(
                expanded_size(&raw).unwrap(),
                (raw.len() + leaf.len()) as u64
            );
        }
        let nested = compressed(&packed);
        assert_eq!(
            expanded_size(&nested).unwrap(),
            (nested.len() + packed.len() + leaf.len()) as u64
        );
        // A later unknown item makes byte-array parsing fall back to opaque data,
        // but earlier inflation must still be included in the shared count.
        let mut child = byte_array(&packed);
        child.extend([10, 0]);
        let raw = byte_array(&child);
        assert_eq!(
            expanded_size(&raw).unwrap(),
            (raw.len() + leaf.len()) as u64
        );
    }

    #[test]
    fn rejects_a_small_stream_that_inflates_beyond_the_shared_limit() {
        // Produce the bomb without allocating its uncompressed contents.
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..513 {
            encoder.write_all(&[0; 65536]).unwrap();
        }
        let mut raw = vec![76; 10];
        raw.extend(encoder.finish().unwrap());
        assert!(raw.len() < 1024 * 1024);
        assert!(expanded_size(&raw).unwrap_err().contains("32 MiB"));
        // The same compression cannot escape validation inside speculative bytes.
        assert!(expanded_size(&byte_array(&raw)).is_err());
        assert!(expanded_size(&xml_like_byte_array(&raw)).is_err());
        assert!(expanded_size(&level(&raw)).is_err());

        // Separate speculative arrays share the same expansion allowance.
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..257 {
            encoder.write_all(&[0; 65536]).unwrap();
        }
        let mut packed = vec![76; 10];
        packed.extend(encoder.finish().unwrap());
        let mut siblings = byte_array(&packed);
        siblings.extend(byte_array(&packed));
        assert!(expanded_size(&siblings).unwrap_err().contains("32 MiB"));
    }

    #[test]
    fn opaque_values_xml_names_and_strings_are_not_compressed_blocks() {
        for raw in [
            vec![0, 76, 76, 76],
            byte_array(&[0, 76, 76, 76]),
            b"\xef\xbb\xbf \n<variant value='LLLL'/>".to_vec(),
            vec![8, 2, b'L', 0, 0, 0, 76, 0, 0, 0],
        ] {
            assert_eq!(expanded_size(&raw).unwrap(), raw.len() as u64);
        }
    }

    #[test]
    fn rejects_excessive_nesting_and_invalid_compression() {
        let mut raw = scalar();
        for _ in 0..64 {
            raw = level(&raw);
        }
        assert_eq!(expanded_size(&raw).unwrap(), raw.len() as u64);
        raw = level(&raw);
        assert!(expanded_size(&raw).unwrap_err().contains("64 levels"));
        assert!(expanded_size(&[76; 26]).is_err());
    }
}
