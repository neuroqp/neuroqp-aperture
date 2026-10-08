//! ImageJ metadata edits without decoding or changing TIFF pixel blocks.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const COUNTS: u64 = 50838;
const METADATA: u64 = 50839;
const ROI: u32 = 0x726f6920;
const OVERLAY: u32 = 0x6f766572;
// ponytail: bound annotation metadata to 32 MiB; increase only for measured demand.
const LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Default, Debug)]
pub struct TiffAnnotations {
    pub active_roi: Option<Vec<u8>>,
    pub overlays: Vec<Vec<u8>>,
}

type MetadataBlocks = Vec<(u32, Vec<Vec<u8>>)>;

struct Tiff {
    file: File,
    len: u64,
    little: bool,
    big: bool,
    entries: Vec<Vec<u8>>,
    next: Vec<u8>,
}

impl Tiff {
    fn number(&self, b: &[u8]) -> u64 {
        if self.little {
            b.iter().rev().fold(0, |v, b| (v << 8) | u64::from(*b))
        } else {
            b.iter().fold(0, |v, b| (v << 8) | u64::from(*b))
        }
    }
    fn bytes(&self, n: u64, size: usize) -> Result<Vec<u8>, String> {
        if size < 8 && n >= (1u64 << (size * 8)) {
            return Err("TIFF offset or count exceeds its format limit".into());
        }
        let b = if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        };
        Ok(if self.little {
            b[..size].to_vec()
        } else {
            b[8 - size..].to_vec()
        })
    }
    fn at(&mut self, offset: u64, length: u64) -> Result<Vec<u8>, String> {
        if length > LIMIT || offset.checked_add(length).is_none_or(|end| end > self.len) {
            return Err("TIFF metadata exceeds file bounds or annotation size limit".into());
        }
        let mut bytes = vec![0; length as usize];
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
    fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut t = Self {
            file,
            len,
            little: true,
            big: false,
            entries: vec![],
            next: vec![],
        };
        let h = t.at(0, 8)?;
        t.little = match &h[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err("Not a TIFF file".into()),
        };
        let first = match t.number(&h[2..4]) {
            42 => t.number(&h[4..8]),
            43 => {
                t.big = true;
                if t.number(&h[4..6]) != 8 || t.number(&h[6..8]) != 0 {
                    return Err("Unsupported BigTIFF header".into());
                }
                let p = t.at(8, 8)?;
                t.number(&p)
            }
            _ => return Err("Unsupported TIFF header".into()),
        };
        let size = if t.big { 8 } else { 2 };
        if first < if t.big { 16 } else { 8 } {
            return Err("Invalid first TIFF directory".into());
        }
        let n = t.at(first, size)?;
        let n = t.number(&n);
        // TIFF has uint16 directory counts; apply that ceiling to BigTIFF too.
        if n > 65535 {
            return Err("Too many TIFF directory entries".into());
        }
        let width = if t.big { 20 } else { 12 };
        let offset_size = if t.big { 8 } else { 4 };
        let data = t.at(first + size, n * width + offset_size)?;
        t.entries = data[..(n * width) as usize]
            .chunks_exact(width as usize)
            .map(|b| b.to_vec())
            .collect();
        t.next = data[(n * width) as usize..].to_vec();
        Ok(t)
    }
    fn tag(&self, entry: &[u8]) -> u64 {
        self.number(&entry[..2])
    }
    fn value(&mut self, tag: u64, kind: u64, unit: u64) -> Result<Option<Vec<u8>>, String> {
        let found: Vec<_> = self
            .entries
            .iter()
            .filter(|e| self.tag(e) == tag)
            .cloned()
            .collect();
        if found.len() > 1 {
            return Err("Duplicate ImageJ TIFF metadata tag".into());
        }
        let Some(e) = found.first() else {
            return Ok(None);
        };
        if self.number(&e[2..4]) != kind {
            return Err("Unsupported ImageJ TIFF metadata field type".into());
        }
        let pos = if self.big { 12 } else { 8 };
        let length = self
            .number(&e[4..pos])
            .checked_mul(unit)
            .ok_or("Invalid TIFF metadata count")?;
        if length > LIMIT {
            return Err("ImageJ TIFF metadata exceeds annotation size limit".into());
        }
        if length <= if self.big { 8 } else { 4 } {
            Ok(Some(e[pos..pos + length as usize].to_vec()))
        } else {
            let offset = self.number(&e[pos..]);
            self.at(offset, length).map(Some)
        }
    }
    fn blocks(&mut self) -> Result<MetadataBlocks, String> {
        let counts = self.value(COUNTS, 4, 4)?;
        let data = self.value(METADATA, 1, 1)?;
        let (counts, data) = match (counts, data) {
            (None, None) => return Ok(vec![]),
            (Some(c), Some(d)) => (c, d),
            _ => return Err("Incomplete ImageJ TIFF metadata".into()),
        };
        let counts: Vec<_> = counts
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| self.number(b) as usize)
            .collect();
        let hdr = *counts
            .first()
            .ok_or("Missing ImageJ metadata header count")?;
        if hdr < 4
            || hdr > data.len()
            || (hdr - 4) % 8 != 0
            || self.number(&data[..4]) != 0x494a494a
        {
            return Err("Invalid ImageJ metadata header".into());
        }
        let mut count_index = 1;
        let mut offset = hdr;
        let mut blocks = vec![];
        for e in data[4..hdr].as_chunks::<8>().0 {
            let kind = self.number(&e[..4]) as u32;
            let n = self.number(&e[4..]) as usize;
            if n > counts.len().saturating_sub(count_index) {
                return Err("Invalid ImageJ metadata block count".into());
            }
            let mut items = vec![];
            for &length in &counts[count_index..count_index + n] {
                let end = offset
                    .checked_add(length)
                    .ok_or("ImageJ metadata length overflow")?;
                if end > data.len() {
                    return Err("Truncated ImageJ metadata block".into());
                }
                items.push(data[offset..end].to_vec());
                offset = end;
            }
            count_index += n;
            blocks.push((kind, items));
        }
        if count_index != counts.len() || offset != data.len() {
            return Err("Inconsistent ImageJ metadata lengths".into());
        }
        Ok(blocks)
    }
    fn entry(&self, tag: u64, kind: u64, count: u64, value: &[u8]) -> Result<Vec<u8>, String> {
        let width = if self.big { 8 } else { 4 };
        let mut e = self.bytes(tag, 2)?;
        e.extend(self.bytes(kind, 2)?);
        e.extend(self.bytes(count, width)?);
        e.extend(value);
        e.resize(if self.big { 20 } else { 12 }, 0);
        Ok(e)
    }
}

pub fn read(path: &Path) -> Result<TiffAnnotations, String> {
    let mut t = Tiff::open(path)?;
    let mut annotations = TiffAnnotations::default();
    for (kind, items) in t.blocks()? {
        if kind == ROI {
            if items.len() != 1 || annotations.active_roi.is_some() {
                return Err("Invalid active ImageJ ROI count".into());
            }
            annotations.active_roi = items.into_iter().next();
        } else if kind == OVERLAY {
            annotations.overlays.extend(items);
        }
    }
    Ok(annotations)
}

/// Writes a TIFF atomically, including replacement of the source when authorized.
/// The caller supplies all overlays to retain.
pub fn save_copy(
    source: &Path,
    target: &Path,
    active: Vec<u8>,
    overlays: Vec<Vec<u8>>,
    overwrite: bool,
) -> Result<(), String> {
    if !overwrite && fs::symlink_metadata(target).is_ok() {
        return Err("The destination exists. Confirm overwrite before saving.".into());
    }
    // Resolve existing symlinks so an overwrite preserves the selected link.
    let target_path = match fs::canonicalize(target) {
        Ok(path) => path,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && !fs::symlink_metadata(target)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink()) =>
        {
            target.to_path_buf()
        }
        Err(error) => return Err(format!("Cannot resolve the destination: {error}")),
    };
    let target = target_path.as_path();
    if fs::metadata(target).is_ok_and(|metadata| metadata.permissions().readonly()) {
        return Err("The destination is read-only. Choose Save as to write another file.".into());
    }
    let mut t = Tiff::open(source)?;
    // New Save-as files use the creation mode and umask. Only an existing
    // destination carries its permissions through atomic replacement.
    let permissions = fs::metadata(target)
        .ok()
        .map(|metadata| metadata.permissions());
    if !t.big && t.len > u32::MAX as u64 {
        return Err("Source already exceeds classic TIFF's 4 GiB offset limit".into());
    }
    let mut blocks = t.blocks()?;
    blocks.retain(|(kind, _)| *kind != ROI && *kind != OVERLAY);
    if !active.is_empty() {
        blocks.push((ROI, vec![active]));
    }
    if !overlays.is_empty() {
        blocks.push((OVERLAY, overlays));
    }
    let payload_size =
        blocks
            .iter()
            .flat_map(|(_, items)| items)
            .try_fold(0u64, |total, item| {
                total
                    .checked_add(item.len() as u64)
                    .ok_or("Annotation size overflow")
            })?;
    let item_count = blocks
        .iter()
        .map(|(_, items)| items.len() as u64)
        .sum::<u64>();
    if payload_size + 4 + blocks.len() as u64 * 8 > LIMIT || (item_count + 1) * 4 > LIMIT {
        return Err("Annotations exceed TIFF metadata size limit".into());
    }
    let mut data = t.bytes(0x494a494a, 4)?;
    let mut counts = t.bytes(4 + blocks.len() as u64 * 8, 4)?;
    for (kind, items) in &blocks {
        data.extend(t.bytes(u64::from(*kind), 4)?);
        data.extend(t.bytes(items.len() as u64, 4)?);
    }
    for (_, items) in blocks {
        for item in items {
            counts.extend(t.bytes(item.len() as u64, 4)?);
            data.extend(item);
        }
    }
    if data.len() as u64 > LIMIT || counts.len() as u64 > LIMIT {
        return Err("Annotations exceed TIFF metadata size limit".into());
    }
    let alignment = if t.big { 8u64 } else { 2u64 };
    let padded = t
        .len
        .checked_add(alignment - 1)
        .ok_or("TIFF file size overflow")?
        / alignment
        * alignment;
    let metadata_end = padded
        .checked_add(counts.len() as u64)
        .and_then(|size| size.checked_add(data.len() as u64))
        .ok_or("TIFF file size overflow")?;
    let ifd_offset = metadata_end
        .checked_add(alignment - 1)
        .ok_or("TIFF file size overflow")?
        / alignment
        * alignment;
    let entry_count = t
        .entries
        .iter()
        .filter(|e| ![COUNTS, METADATA].contains(&t.tag(e)))
        .count()
        + 2;
    let final_size = ifd_offset
        .checked_add(if t.big {
            8 + entry_count as u64 * 20 + 8
        } else {
            2 + entry_count as u64 * 12 + 4
        })
        .ok_or("TIFF file size overflow")?;
    if !t.big && final_size > u32::MAX as u64 {
        return Err(
            "Annotations would exceed classic TIFF's 4 GiB offset limit; use a BigTIFF source"
                .into(),
        );
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = parent.join(format!(".aperture-save-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        t.file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut t.file, &mut out).map_err(|e| e.to_string())?;
        if copied != t.len {
            return Err("Source TIFF changed while saving".into());
        }
        let alignment = if t.big { 8u64 } else { 2u64 };
        let padded = t
            .len
            .checked_add(alignment - 1)
            .ok_or("TIFF file size overflow")?
            / alignment
            * alignment;
        out.write_all(&vec![0; (padded - t.len) as usize])
            .map_err(|e| e.to_string())?;
        let counts_offset = padded;
        out.write_all(&counts).map_err(|e| e.to_string())?;
        let data_offset = out.stream_position().map_err(|e| e.to_string())?;
        out.write_all(&data).map_err(|e| e.to_string())?;
        let position = out.stream_position().map_err(|e| e.to_string())?;
        let ifd_offset = position
            .checked_add(alignment - 1)
            .ok_or("TIFF file size overflow")?
            / alignment
            * alignment;
        out.write_all(&vec![0; (ifd_offset - position) as usize])
            .map_err(|e| e.to_string())?;
        let offset_width = if t.big { 8 } else { 4 };
        let counts_value = if counts.len() <= offset_width {
            counts.clone()
        } else {
            t.bytes(counts_offset, offset_width)?
        };
        let data_value = if data.len() <= offset_width {
            data.clone()
        } else {
            t.bytes(data_offset, offset_width)?
        };
        let mut entries: Vec<_> = t
            .entries
            .iter()
            .filter(|e| ![COUNTS, METADATA].contains(&t.tag(e)))
            .cloned()
            .collect();
        entries.push(t.entry(COUNTS, 4, counts.len() as u64 / 4, &counts_value)?);
        entries.push(t.entry(METADATA, 1, data.len() as u64, &data_value)?);
        entries.sort_by_key(|e| t.tag(e));
        out.write_all(&t.bytes(entries.len() as u64, if t.big { 8 } else { 2 })?)
            .map_err(|e| e.to_string())?;
        for e in entries {
            out.write_all(&e).map_err(|e| e.to_string())?;
        }
        out.write_all(&t.next).map_err(|e| e.to_string())?;
        if !t.big && out.stream_position().map_err(|e| e.to_string())? > u32::MAX as u64 {
            return Err("Annotated copy exceeds classic TIFF's 4 GiB offset limit".into());
        }
        out.seek(SeekFrom::Start(if t.big { 8 } else { 4 }))
            .map_err(|e| e.to_string())?;
        out.write_all(&t.bytes(ifd_offset, offset_width)?)
            .map_err(|e| e.to_string())?;
        if let Some(permissions) = permissions {
            out.set_permissions(permissions)
                .map_err(|e| e.to_string())?;
        }
        out.sync_all().map_err(|e| e.to_string())?;
        if overwrite {
            fs::rename(&temp, target).map_err(|e| e.to_string())?;
        } else {
            // Creating the destination link is atomic and fails if it appeared
            // after the dialog. A rename would silently replace that file.
            fs::hard_link(&temp, target).map_err(|e| {
                format!("Cannot create the destination without overwriting it: {e}")
            })?;
            let _ = fs::remove_file(&temp);
        }
        // The file is already committed; best effort directory fsync also
        // persists the rename/link on filesystems which support it.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(path: &Path, little: bool, big: bool) {
        let file = File::create(path).unwrap();
        let t = Tiff {
            file,
            len: 0,
            little,
            big,
            entries: vec![],
            next: vec![],
        };
        let mut b = if little {
            b"II".to_vec()
        } else {
            b"MM".to_vec()
        };
        b.extend(t.bytes(if big { 43 } else { 42 }, 2).unwrap());
        if big {
            b.extend(t.bytes(8, 2).unwrap());
            b.extend([0, 0]);
        }
        b.extend(
            t.bytes(if big { 16 } else { 8 }, if big { 8 } else { 4 })
                .unwrap(),
        );
        b.extend(t.bytes(1, if big { 8 } else { 2 }).unwrap());
        b.extend(t.entry(256, 4, 1, &t.bytes(7, 4).unwrap()).unwrap());
        b.extend(t.bytes(0, if big { 8 } else { 4 }).unwrap());
        b.extend(b"unchanged-pixel-block");
        fs::write(path, b).unwrap();
    }
    #[test]
    fn copy_roundtrip_preserves_bytes_and_unrelated_blocks_in_all_layouts() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aperture-tiff-test-{stamp}"));
        fs::create_dir(&dir).unwrap();
        for little in [false, true] {
            for big in [false, true] {
                let source = dir.join("source.tif");
                let target = dir.join("target.tif");
                fixture(&source, little, big);
                let before = fs::read(&source).unwrap();
                assert!(read(&source).unwrap().active_roi.is_none());
                save_copy(
                    &source,
                    &target,
                    vec![1, 2, 3],
                    vec![vec![4, 5], vec![6]],
                    true,
                )
                .unwrap();
                let a = read(&target).unwrap();
                assert_eq!(a.active_roi, Some(vec![1, 2, 3]));
                assert_eq!(a.overlays, vec![vec![4, 5], vec![6]]);
                let after = fs::read(&target).unwrap();
                let header = if big { 16 } else { 8 };
                assert_eq!(&after[header..before.len()], &before[header..]);
                assert_eq!(fs::read(&source).unwrap(), before);
                assert!(save_copy(&source, &source, vec![], vec![], false).is_err());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
                }
                save_copy(&source, &source, vec![7], vec![], true).unwrap();
                let replaced = fs::read(&source).unwrap();
                assert_eq!(&replaced[header..before.len()], &before[header..]);
                assert_eq!(read(&source).unwrap().active_roi, Some(vec![7]));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(&source).unwrap().permissions().mode() & 0o777,
                        0o640
                    );
                }
                assert!(
                    save_copy(&source, &source, vec![0; LIMIT as usize], vec![], true).is_err()
                );
                assert_eq!(fs::read(&source).unwrap(), replaced);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).unwrap();
                    assert!(save_copy(&source, &source, vec![8], vec![], true)
                        .unwrap_err()
                        .contains("read-only"));
                    assert_eq!(fs::read(&source).unwrap(), replaced);
                    let copy = dir.join("readonly-copy.tif");
                    fs::remove_file(&copy).ok();
                    save_copy(&source, &copy, vec![8], vec![], false).unwrap();
                    assert!(!fs::metadata(&copy).unwrap().permissions().readonly());
                    save_copy(&copy, &copy, vec![9], vec![], true).unwrap();
                    assert_eq!(read(&copy).unwrap().active_roi, Some(vec![9]));
                    fs::remove_file(copy).unwrap();
                    let alias = dir.join("linked-source.tif");
                    std::os::unix::fs::symlink(&source, &alias).unwrap();
                    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
                    save_copy(&source, &alias, vec![10], vec![], true).unwrap();
                    assert!(fs::symlink_metadata(&alias)
                        .unwrap()
                        .file_type()
                        .is_symlink());
                    assert_eq!(read(&source).unwrap().active_roi, Some(vec![10]));
                    fs::remove_file(alias).unwrap();
                    let dangling = dir.join("dangling.tif");
                    std::os::unix::fs::symlink(dir.join("missing.tif"), &dangling).unwrap();
                    assert!(save_copy(&source, &dangling, vec![11], vec![], true).is_err());
                    assert!(fs::symlink_metadata(&dangling)
                        .unwrap()
                        .file_type()
                        .is_symlink());
                    fs::remove_file(dangling).unwrap();
                }
                // Turn the active block into an unknown block; the next save must retain it.
                let t = Tiff::open(&target).unwrap();
                let entry = t.entries.iter().find(|e| t.tag(e) == METADATA).unwrap();
                let start = if big { 12 } else { 8 };
                let offset = t.number(&entry[start..]);
                let mut f = OpenOptions::new().write(true).open(&target).unwrap();
                f.seek(SeekFrom::Start(offset + 4)).unwrap();
                f.write_all(&t.bytes(0x12345678, 4).unwrap()).unwrap();
                drop(f);
                let second = dir.join("second.tif");
                save_copy(&target, &second, vec![9], vec![vec![8]], true).unwrap();
                let blocks = Tiff::open(&second).unwrap().blocks().unwrap();
                assert!(blocks
                    .iter()
                    .any(|(kind, items)| *kind == 0x12345678 && *items == vec![vec![1, 2, 3]]));
                // Fail during the final rename, after writing a complete temp
                // copy, and verify cleanup and the original remain intact.
                let failed_target = dir.join("directory-target.tif");
                fs::create_dir(&failed_target).unwrap();
                let source_before_failure = fs::read(&source).unwrap();
                assert!(save_copy(&source, &failed_target, vec![11], vec![], true).is_err());
                assert_eq!(fs::read(&source).unwrap(), source_before_failure);
                assert!(failed_target.is_dir());
                fs::remove_dir(failed_target).unwrap();
            }
        }
        assert!(!fs::read_dir(&dir).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aperture-save-")));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn malformed_offsets_are_rejected() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aperture-tiff-bad-{stamp}.tif"));
        fs::write(&path, [b'I', b'I', 42, 0, 255, 255, 255, 255]).unwrap();
        assert!(read(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn classic_tiff_offset_overflow_fails_before_copying_or_changing_source() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aperture-tiff-limit-{stamp}.tif"));
        fixture(&path, true, false);
        let before = fs::read(&path).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(u32::MAX as u64 - 8)
            .unwrap();
        let error = save_copy(&path, &path, vec![1, 2, 3], vec![], true).unwrap_err();
        assert!(error.contains("4 GiB offset limit"), "{error}");
        assert_eq!(fs::metadata(&path).unwrap().len(), u32::MAX as u64 - 8);
        let mut prefix = vec![0; before.len()];
        File::open(&path).unwrap().read_exact(&mut prefix).unwrap();
        assert_eq!(prefix, before);
        fs::remove_file(path).unwrap();
    }
}
