//! TIFF byte order and header fields shared by the preview and annotation parsers.

pub(crate) fn number(bytes: &[u8], little: bool) -> u64 {
    if little {
        bytes.iter().rev().fold(0, |n, b| (n << 8) | u64::from(*b))
    } else {
        bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
    }
}

/// Byte order and BigTIFF flag from a header's first eight bytes.
pub(crate) fn header(bytes: &[u8]) -> Result<(bool, bool), &'static str> {
    let little = match &bytes[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err("Not a TIFF file"),
    };
    match number(&bytes[2..4], little) {
        42 => Ok((little, false)),
        43 if number(&bytes[4..6], little) == 8 && number(&bytes[6..8], little) == 0 => {
            Ok((little, true))
        }
        43 => Err("Unsupported BigTIFF header"),
        _ => Err("Unsupported TIFF header"),
    }
}
