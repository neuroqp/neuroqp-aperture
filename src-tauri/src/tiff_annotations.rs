//! ImageJ metadata edits without decoding or changing TIFF pixel blocks.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const COUNTS: u64 = 50838;
const METADATA: u64 = 50839;
const ROI: u32 = 0x726f6920;
const OVERLAY: u32 = 0x6f766572;
// ponytail: bound annotation metadata to 32 MiB; increase only for measured demand.
const LIMIT: u64 = 32 * 1024 * 1024;
const ITEM_SIZE: u64 = std::mem::size_of::<Vec<u8>>() as u64;
const BLOCK_SIZE: u64 = std::mem::size_of::<(u32, Vec<Vec<u8>>)>() as u64;

// Bound both the encoded tags together and the payload/container allocations.
// A large ROI remains one item; this ceiling targets tiny/empty item amplification.
fn metadata_sizes(payload: u64, items: u64, blocks: u64) -> Result<(u64, u64), String> {
    let sizes = (|| {
        let data = payload
            .checked_add(4)?
            .checked_add(blocks.checked_mul(8)?)?;
        let counts = items.checked_add(1)?.checked_mul(4)?;
        let materialized = payload
            .checked_add(items.checked_mul(ITEM_SIZE)?)?
            .checked_add(blocks.checked_mul(BLOCK_SIZE)?)?;
        (data.checked_add(counts)? <= LIMIT && materialized <= LIMIT).then_some((data, counts))
    })();
    sizes.ok_or_else(|| "Annotations exceed TIFF metadata size limit".into())
}

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
    directory: u64,
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
            directory: 0,
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
        t.load_directory(first, LIMIT)?;
        Ok(t)
    }
    fn load_directory(&mut self, offset: u64, budget: u64) -> Result<u64, String> {
        let size = if self.big { 8 } else { 2 };
        if budget < size {
            return Err("TIFF directory metadata budget exceeded".into());
        }
        if offset < if self.big { 16 } else { 8 } {
            return Err("Invalid TIFF directory offset".into());
        }
        let n = self.at(offset, size)?;
        let n = self.number(&n);
        // TIFF has uint16 directory counts; apply that ceiling to BigTIFF too.
        if n > 65535 {
            return Err("Too many TIFF directory entries".into());
        }
        let width = if self.big { 20 } else { 12 };
        let offset_size = if self.big { 8 } else { 4 };
        let bytes = n
            .checked_mul(width)
            .and_then(|bytes| bytes.checked_add(offset_size))
            .ok_or("TIFF directory size overflow")?;
        let total = bytes
            .checked_add(size)
            .filter(|total| *total <= budget)
            .ok_or("TIFF directory metadata budget exceeded")?;
        let data = self.at(
            offset
                .checked_add(size)
                .ok_or("TIFF directory offset overflow")?,
            bytes,
        )?;
        self.entries = data[..(n * width) as usize]
            .chunks_exact(width as usize)
            .map(|b| b.to_vec())
            .collect();
        self.next = data[(n * width) as usize..].to_vec();
        self.directory = offset;
        Ok(total)
    }
    fn unsigned_values(&mut self, tag: u64, count_limit: u64) -> Result<Vec<u64>, String> {
        let Some(entry) = self.entries.iter().find(|entry| self.tag(entry) == tag) else {
            return Ok(Vec::new());
        };
        let kind = self.number(&entry[2..4]);
        let unit = match kind {
            3 => 2,
            4 | 13 => 4,
            16 | 18 if self.big => 8,
            _ => return Err("Unsupported TIFF layout field type".into()),
        };
        let bytes = self
            .value(tag, kind, unit, count_limit * unit)?
            .ok_or("Missing TIFF layout tag")?;
        Ok(bytes
            .chunks_exact(unit as usize)
            .map(|bytes| self.number(bytes))
            .collect())
    }
    fn append_pyramid_tags(&mut self, report: &mut String) -> Result<(), String> {
        if self
            .unsigned_values(254, 1)?
            .first()
            .is_some_and(|flags| flags & 1 != 0)
        {
            report.push_str("Subfile Type: reduced-resolution image\n");
        }
        let width = self.unsigned_values(322, 1)?.first().copied().unwrap_or(0);
        let height = self.unsigned_values(323, 1)?.first().copied().unwrap_or(0);
        if width > 0 && height > 0 && width <= u32::MAX as u64 && height <= u32::MAX as u64 {
            report.push_str(&format!("Tile Width: {width} Tile Length: {height}\n"));
        }
        Ok(())
    }
    fn tag(&self, entry: &[u8]) -> u64 {
        self.number(&entry[..2])
    }
    fn descriptor(&self, tag: u64, kind: u64, unit: u64) -> Result<Option<(&[u8], u64)>, String> {
        let mut found = self.entries.iter().filter(|e| self.tag(e) == tag);
        let Some(e) = found.next() else {
            return Ok(None);
        };
        if found.next().is_some() {
            return Err("Duplicate ImageJ TIFF metadata tag".into());
        }
        if self.number(&e[2..4]) != kind {
            return Err("Unsupported ImageJ TIFF metadata field type".into());
        }
        let pos = if self.big { 12 } else { 8 };
        let length = self
            .number(&e[4..pos])
            .checked_mul(unit)
            .ok_or("Invalid TIFF metadata count")?;
        Ok(Some((e, length)))
    }
    fn value(
        &mut self,
        tag: u64,
        kind: u64,
        unit: u64,
        limit: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some((e, length)) = self.descriptor(tag, kind, unit)? else {
            return Ok(None);
        };
        if length > limit {
            return Err("ImageJ TIFF metadata exceeds annotation size limit".into());
        }
        let pos = if self.big { 12 } else { 8 };
        if length <= if self.big { 8 } else { 4 } {
            Ok(Some(e[pos..pos + length as usize].to_vec()))
        } else {
            let offset = self.number(&e[pos..]);
            self.at(offset, length).map(Some)
        }
    }
    fn blocks(&mut self) -> Result<MetadataBlocks, String> {
        // Even zero-byte items need a Vec header. Reject excessive counts before
        // reading their tag, then leave only the remaining shared byte budget.
        let counts_size = self.descriptor(COUNTS, 4, 4)?.map_or(0, |(_, size)| size);
        let data_size = self.descriptor(METADATA, 1, 1)?.map_or(0, |(_, size)| size);
        if counts_size
            .checked_add(data_size)
            .is_none_or(|size| size > LIMIT)
            || counts_size > (LIMIT / ITEM_SIZE + 1) * 4
        {
            return Err("ImageJ TIFF metadata exceeds annotation size limit".into());
        }
        let counts = self.value(COUNTS, 4, 4, LIMIT)?;
        let data = self.value(METADATA, 1, 1, LIMIT - counts_size)?;
        let (counts, data) = match (counts, data) {
            (None, None) => return Ok(vec![]),
            (Some(c), Some(d)) => (c, d),
            _ => return Err("Incomplete ImageJ TIFF metadata".into()),
        };
        let counts = counts.as_chunks::<4>().0;
        let hdr = self.number(
            counts
                .first()
                .ok_or("Missing ImageJ metadata header count")?,
        ) as usize;
        if hdr < 4
            || hdr > data.len()
            || !(hdr - 4).is_multiple_of(8)
            || self.number(&data[..4]) != 0x494a494a
        {
            return Err("Invalid ImageJ metadata header".into());
        }
        let block_count = (hdr - 4) / 8;
        metadata_sizes(
            (data.len() - hdr) as u64,
            (counts.len() - 1) as u64,
            block_count as u64,
        )?;
        let mut count_index = 1;
        let mut offset = hdr;
        let mut blocks = Vec::with_capacity(block_count);
        for e in data[4..hdr].as_chunks::<8>().0 {
            let kind = self.number(&e[..4]) as u32;
            let n = self.number(&e[4..]) as usize;
            if n > counts.len().saturating_sub(count_index) {
                return Err("Invalid ImageJ metadata block count".into());
            }
            let mut items = Vec::with_capacity(n);
            for length in &counts[count_index..count_index + n] {
                let length = self.number(length) as usize;
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
            if annotations.overlays.is_empty() {
                annotations.overlays = items;
            } else {
                annotations.overlays.reserve_exact(items.len());
                annotations.overlays.extend(items);
            }
        }
    }
    Ok(annotations)
}

/// Channel tags plus the bounded directory layout used by native pyramid probes.
/// Rust file I/O keeps Windows filenames literal; the worker makes reads cancellable.
pub fn channel_info(path: &Path) -> Result<String, String> {
    let mut t = Tiff::open(path)?;
    let mut report = String::from("=== TIFF directory 0 ===\n");
    if let Some(extras) = t.value(338, 3, 2, 64)? {
        for bytes in extras.as_chunks::<2>().0 {
            let kind = match t.number(bytes) {
                0 => "unspecified",
                1 => "associated-alpha",
                2 => "unassociated-alpha",
                _ => return Err("Unsupported TIFF ExtraSamples value".into()),
            };
            report.push_str(&format!("Extra Samples: {kind}\n"));
        }
    }
    if let Some(planar) = t.value(284, 3, 2, 2)? {
        match t.number(&planar) {
            1 => {}
            2 => report.push_str("Planar Configuration: separate image planes\n"),
            _ => return Err("Unsupported TIFF PlanarConfiguration value".into()),
        }
    }
    let mut pyramid = String::new();
    let layout = (|| {
        // Match native discovery: <=256 ordinary pages and the first descriptor
        // of <=28 direct SubIFD chains on page zero. Never recurse into scenes.
        let first_bytes =
            t.entries.len() as u64 * if t.big { 20 } else { 12 } + if t.big { 16 } else { 6 };
        let mut budget = 4 * 1024 * 1024_u64 - first_bytes;
        let mut seen = std::collections::HashSet::new();
        let first = t.directory;
        for page in 0..256 {
            let offset = if page == 0 { first } else { t.number(&t.next) };
            if offset == 0 {
                return Ok::<_, String>(());
            }
            if !seen.insert(offset) {
                return Err("Cyclic TIFF directory chain".into());
            }
            if page != 0 {
                budget -= t.load_directory(offset, budget)?;
                pyramid.push_str(&format!("=== TIFF directory {page} ===\n"));
            }
            t.append_pyramid_tags(&mut pyramid)?;
            if page == 0 {
                let next = t.number(&t.next);
                for (chain, subifd) in t.unsigned_values(330, 28)?.into_iter().enumerate() {
                    if !seen.insert(subifd) {
                        return Err("Cyclic TIFF SubIFD reference".into());
                    }
                    budget -= t.load_directory(subifd, budget)?;
                    pyramid.push_str(&format!(
                        "--- SubIFD 0 of chain {chain} at offset {subifd}:\n"
                    ));
                    t.append_pyramid_tags(&mut pyramid)?;
                }
                // SubIFD loads change the parser's current directory; restore the
                // ordinary next-page link without rereading the base directory.
                t.next = t.bytes(next, if t.big { 8 } else { 4 })?;
            }
            if pyramid.len() > 48 * 1024 {
                return Err("TIFF directory report budget exceeded".into());
            }
        }
        (t.number(&t.next) == 0)
            .then_some(())
            .ok_or_else(|| "Too many TIFF directories to inspect".into())
    })();
    if layout.is_ok() {
        // Channel tags stay before directory one; child alpha/planar tags never
        // contaminate first-page channel interpretation.
        let complete = format!("{report}{pyramid}");
        // Include JSON escaping and reserve space for the worker result envelope.
        if serde_json::to_vec(&complete)
            .map_err(|e| e.to_string())?
            .len()
            <= crate::vips_worker::MAX_MESSAGE - 128
        {
            return Ok(complete);
        }
    }
    // An incomplete graph cannot authorize reuse of any partial pyramid.
    report.push_str("aperture-native-pyramid: unavailable\n");
    Ok(report)
}

/// A complete TIFF copy awaiting a short, coordinated destination commit.
pub struct StagedSave {
    temp: PathBuf,
    target: PathBuf,
    overwrite: bool,
}

impl StagedSave {
    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn commit(self) -> Result<(), String> {
        if self.overwrite {
            fs::rename(&self.temp, &self.target).map_err(|e| e.to_string())?;
        } else {
            fs::hard_link(&self.temp, &self.target).map_err(|e| {
                format!("Cannot create the destination without overwriting it: {e}")
            })?;
        }
        if let Some(parent) = self.target.parent() {
            let _ = File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    }
}

impl Drop for StagedSave {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp);
    }
}

/// Writes a TIFF atomically, including replacement of the source when authorized.
#[cfg(test)]
pub fn save_copy(
    source: &Path,
    target: &Path,
    active: Vec<u8>,
    overlays: Vec<Vec<u8>>,
    overwrite: bool,
) -> Result<(), String> {
    stage_copy(source, target, active, overlays, overwrite)?.commit()
}

pub fn stage_copy(
    source: &Path,
    target: &Path,
    active: Vec<u8>,
    overlays: Vec<Vec<u8>>,
    overwrite: bool,
) -> Result<StagedSave, String> {
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
            crate::instances::path_key(target).map_err(|e| e.to_string())?
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
    let (payload_size, item_count) = blocks.iter().flat_map(|(_, items)| items).try_fold(
        (0u64, 0u64),
        |(size, count), item| {
            Ok::<_, String>((
                size.checked_add(item.len() as u64)
                    .ok_or("Annotation size overflow")?,
                count
                    .checked_add(1)
                    .ok_or("Annotation item count overflow")?,
            ))
        },
    )?;
    let (data_size, counts_size) = metadata_sizes(payload_size, item_count, blocks.len() as u64)?;
    let mut data = Vec::with_capacity(data_size as usize);
    data.extend(t.bytes(0x494a494a, 4)?);
    let mut counts = Vec::with_capacity(counts_size as usize);
    counts.extend(t.bytes(4 + blocks.len() as u64 * 8, 4)?);
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
    let mut entries: Vec<_> = t
        .entries
        .iter()
        .filter(|e| ![COUNTS, METADATA].contains(&t.tag(e)))
        .cloned()
        .collect();
    let entry_count = entries.len() + 2;
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
    let result: Result<(), String> = (|| {
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
        out.write_all(&vec![0; (padded - t.len) as usize])
            .map_err(|e| e.to_string())?;
        out.write_all(&counts).map_err(|e| e.to_string())?;
        out.write_all(&data).map_err(|e| e.to_string())?;
        out.write_all(&vec![0; (ifd_offset - metadata_end) as usize])
            .map_err(|e| e.to_string())?;
        let offset_width = if t.big { 8 } else { 4 };
        let counts_value = if counts.len() <= offset_width {
            counts.clone()
        } else {
            t.bytes(padded, offset_width)?
        };
        let data_value = if data.len() <= offset_width {
            data.clone()
        } else {
            t.bytes(padded + counts.len() as u64, offset_width)?
        };
        entries.push(t.entry(COUNTS, 4, counts.len() as u64 / 4, &counts_value)?);
        entries.push(t.entry(METADATA, 1, data.len() as u64, &data_value)?);
        entries.sort_by_key(|e| t.tag(e));
        out.write_all(&t.bytes(entries.len() as u64, if t.big { 8 } else { 2 })?)
            .map_err(|e| e.to_string())?;
        for e in entries {
            out.write_all(&e).map_err(|e| e.to_string())?;
        }
        out.write_all(&t.next).map_err(|e| e.to_string())?;
        out.seek(SeekFrom::Start(if t.big { 8 } else { 4 }))
            .map_err(|e| e.to_string())?;
        out.write_all(&t.bytes(ifd_offset, offset_width)?)
            .map_err(|e| e.to_string())?;
        if let Some(permissions) = permissions {
            out.set_permissions(permissions)
                .map_err(|e| e.to_string())?;
        }
        out.sync_all().map_err(|e| e.to_string())?;
        drop(out);
        // Windows cannot replace an open source handle without delete sharing.
        drop(t.file);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(StagedSave {
        temp,
        target: target_path,
        overwrite,
    })
}

#[cfg(test)]
pub(crate) fn pyramid_fixture_as_pages(source: &Path, target: &Path) -> Result<(), String> {
    // Relink a copied fixture's existing pixel/tile blocks; never edit the source.
    let mut t = Tiff::open(source)?;
    let children = t.unsigned_values(330, 28)?;
    if children.is_empty() {
        return Err("Fixture has no SubIFD pyramid".into());
    }
    let entries = t
        .entries
        .iter()
        .filter(|entry| t.tag(entry) != 330)
        .cloned()
        .collect::<Vec<_>>();
    let word = if t.big { 8 } else { 4 };
    let count_size = if t.big { 8 } else { 2 };
    let entry_size = if t.big { 20 } else { 12 };
    let mut bytes = fs::read(source).map_err(|e| e.to_string())?;
    for (index, child) in children.iter().enumerate() {
        t.load_directory(*child, LIMIT)?;
        let position = t.directory as usize + count_size + t.entries.len() * entry_size;
        bytes[position..position + word]
            .copy_from_slice(&t.bytes(children.get(index + 1).copied().unwrap_or(0), word)?);
    }
    let alignment = if t.big { 8 } else { 2 };
    while bytes.len() % alignment != 0 {
        bytes.push(0);
    }
    let directory = bytes.len() as u64;
    bytes.extend(t.bytes(entries.len() as u64, count_size)?);
    for entry in entries {
        bytes.extend(entry);
    }
    bytes.extend(t.bytes(children[0], word)?);
    let first = if t.big { 8 } else { 4 };
    bytes[first..first + word].copy_from_slice(&t.bytes(directory, word)?);
    fs::write(target, bytes).map_err(|e| e.to_string())
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
            directory: 0,
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
    fn staged_copies_leave_source_intact_until_commit_and_clean_cancelled_work() {
        let root = std::env::temp_dir().join(format!(
            "aperture-staged-save-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.tif");
        fixture(&source, true, false);
        let original = fs::read(&source).unwrap();
        let staged = stage_copy(&source, &source, vec![7], vec![], true).unwrap();
        assert_eq!(fs::read(&source).unwrap(), original);
        let temporary = staged.temp.clone();
        assert!(temporary.exists());
        drop(staged);
        assert!(!temporary.exists());
        assert_eq!(fs::read(&source).unwrap(), original);
        stage_copy(&source, &source, vec![7], vec![], true)
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(read(&source).unwrap().active_roi, Some(vec![7]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_report_selects_only_reduced_tiled_pages_and_rejects_incomplete_graphs() {
        let root = std::env::temp_dir().join(format!(
            "aperture-directory-info-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("pyramid [metadata] 日本語.tif");
        let header = "width: 2048\nheight: 1536\nbands: 2\nformat: ushort\ninterpretation: grey16\nvips-loader: tiffload\nn-pages: 3\n";
        let inspect = |options: &str| {
            assert!(
                options.contains("page=1,"),
                "ordinary smaller pages cannot become pyramid levels"
            );
            Ok(header.replace("2048", "1024").replace("1536", "768"))
        };
        for little in [false, true] {
            for big in [false, true] {
                fixture(&path, little, big);
                let t = Tiff::open(&path).unwrap();
                let header_size = if big { 16 } else { 8 };
                let count_size = if big { 8 } else { 2 };
                let word = if big { 8 } else { 4 };
                let directory_size = count_size + 5 * if big { 20 } else { 12 } + word;
                let mut bytes = fs::read(&path).unwrap()[..header_size].to_vec();
                for page in 0..3 {
                    bytes.extend(t.bytes(5, count_size).unwrap());
                    // Bit one remains reduced even with another NewSubfileType flag.
                    for (tag, kind, value) in [
                        (254, 4, if page == 1 { 3 } else { 0 }),
                        (284, 3, if page == 1 { 2 } else { 1 }),
                        (322, 3, 16),
                        (323, 4, 16),
                        (338, 3, if page == 1 { 1 } else { 0 }),
                    ] {
                        bytes.extend(
                            t.entry(
                                tag,
                                kind,
                                1,
                                &t.bytes(value, if kind == 3 { 2 } else { 4 }).unwrap(),
                            )
                            .unwrap(),
                        );
                    }
                    bytes.extend(
                        t.bytes(
                            if page == 2 {
                                0
                            } else {
                                (header_size + (page + 1) * directory_size) as u64
                            },
                            word,
                        )
                        .unwrap(),
                    );
                }
                let first_offset = t.bytes(header_size as u64, word).unwrap();
                let bad_offset = t.bytes(u64::MAX >> if big { 0 } else { 32 }, word).unwrap();
                drop(t);
                fs::write(&path, &bytes).unwrap();
                let info = channel_info(&path).unwrap();
                assert!(
                    !info.contains("associated-alpha"),
                    "later-page alpha must not affect base channels"
                );
                assert_eq!(
                    crate::channels::discover(header, &inspect, Some(&info))
                        .unwrap()
                        .channels
                        .len(),
                    2
                );
                assert_eq!(
                    crate::native::discover(header, &inspect, Some(&info)).len(),
                    2
                );
                assert_eq!(
                    crate::native::discover_preview(header, &inspect, Some(&info)).len(),
                    2
                );
                assert!(
                    serde_json::to_vec(&crate::vips_worker::Response::Complete {
                        result: Ok(info)
                    })
                    .unwrap()
                    .len()
                        < crate::vips_worker::MAX_MESSAGE
                );
                // Both a cycle and an invalid tail discard every reported level.
                for tail in [first_offset, bad_offset] {
                    let mut malformed = bytes.clone();
                    let end = malformed.len();
                    malformed[end - word..].copy_from_slice(&tail);
                    fs::write(&path, malformed).unwrap();
                    let info = channel_info(&path).unwrap();
                    assert!(info.contains("aperture-native-pyramid: unavailable"));
                    assert_eq!(
                        crate::channels::discover(header, &inspect, Some(&info))
                            .unwrap()
                            .channels
                            .len(),
                        2
                    );
                    let no_probe = |_: &str| -> Result<String, String> {
                        panic!("incomplete graph must not authorize probes")
                    };
                    assert!(crate::native::discover(header, &no_probe, Some(&info)).is_empty());
                    assert!(
                        crate::native::discover_preview(header, &no_probe, Some(&info)).is_empty()
                    );
                }
            }
        }
        // Excess page counts also cannot leave a partially trusted native graph.
        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        for page in 0..257 {
            bytes.extend(0_u16.to_le_bytes());
            bytes.extend(
                if page == 256 {
                    0_u32
                } else {
                    8 + (page + 1) * 6
                }
                .to_le_bytes(),
            );
        }
        fs::write(&path, bytes).unwrap();
        assert!(channel_info(&path)
            .unwrap()
            .contains("aperture-native-pyramid: unavailable"));
        // Eleven individually legal directory tables exceed the shared 4 MiB
        // budget, even though neither page count nor report size is excessive.
        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        let size = 2 + 32768 * 12 + 4;
        for page in 0..11 {
            bytes.extend(32768_u16.to_le_bytes());
            bytes.resize(bytes.len() + 32768 * 12, 0);
            bytes.extend(
                if page == 10 {
                    0_u32
                } else {
                    8 + (page + 1) * size as u32
                }
                .to_le_bytes(),
            );
        }
        fs::write(&path, bytes).unwrap();
        assert!(channel_info(&path)
            .unwrap()
            .contains("aperture-native-pyramid: unavailable"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_channel_tags_distinguish_channels_alpha_and_planar_layouts() {
        let root = std::env::temp_dir().join(format!(
            "aperture-channel-tags-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("metadata [source] 日本語.tif");
        for little in [false, true] {
            for big in [false, true] {
                for (bands, interpretation, extra, planar, expected) in [
                    (2, "grey16", 0, 1, Some(2)),
                    (4, "srgb", 0, 1, Some(4)),
                    (2, "grey16", 1, 1, None),
                    (4, "srgb", 1, 1, Some(3)),
                    (4, "srgb", 2, 1, Some(3)),
                    (4, "multiband", 0, 2, None),
                ] {
                    fixture(&path, little, big);
                    let t = Tiff::open(&path).unwrap();
                    let mut bytes = fs::read(&path).unwrap()[..if big { 16 } else { 8 }].to_vec();
                    bytes.extend(t.bytes(2, if big { 8 } else { 2 }).unwrap());
                    bytes.extend(t.entry(284, 3, 1, &t.bytes(planar, 2).unwrap()).unwrap());
                    bytes.extend(t.entry(338, 3, 1, &t.bytes(extra, 2).unwrap()).unwrap());
                    bytes.extend(t.bytes(0, if big { 8 } else { 4 }).unwrap());
                    let oversized_count = t.bytes(33, if big { 8 } else { 4 }).unwrap();
                    drop(t);
                    fs::write(&path, bytes).unwrap();
                    let info = channel_info(&path).unwrap();
                    let header = format!("width: 7\nheight: 9\nbands: {bands}\nformat: ushort\ninterpretation: {interpretation}\nvips-loader: tiffload\n");
                    let layout =
                        crate::channels::discover(&header, &|_| unreachable!(), Some(&info));
                    match expected {
                        Some(count) => assert_eq!(layout.unwrap().channels.len(), count),
                        None => assert!(layout.is_err()),
                    }
                    // An untrusted ExtraSamples count cannot make us read/allocate
                    // a large payload; fail before following its offset.
                    let mut bytes = fs::read(&path).unwrap();
                    let entry = if big { 16 + 8 + 20 } else { 8 + 2 + 12 };
                    bytes[entry + 4..entry + if big { 12 } else { 8 }]
                        .copy_from_slice(&oversized_count);
                    fs::write(&path, bytes).unwrap();
                    assert!(channel_info(&path).is_err());
                }
            }
        }
        fs::remove_dir_all(root).unwrap();
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
    // Sparse tags reproduce malicious declarations without allocating their
    // claimed item containers or metadata payload in the test itself.
    fn metadata_fixture(path: &Path, items: u64, data_size: u64) {
        fixture(path, true, false);
        let t = Tiff::open(path).unwrap();
        let counts_size = (items + 1) * 4;
        let counts_offset = t.len;
        let data_offset = counts_offset + counts_size;
        let ifd_offset = data_offset + data_size;
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(ifd_offset + 2 + 24 + 4).unwrap();
        file.seek(SeekFrom::Start(counts_offset)).unwrap();
        file.write_all(&t.bytes(12, 4).unwrap()).unwrap();
        file.seek(SeekFrom::Start(data_offset)).unwrap();
        for value in [0x494a494a, u64::from(OVERLAY), items] {
            file.write_all(&t.bytes(value, 4).unwrap()).unwrap();
        }
        file.seek(SeekFrom::Start(ifd_offset)).unwrap();
        file.write_all(&t.bytes(2, 2).unwrap()).unwrap();
        file.write_all(
            &t.entry(COUNTS, 4, items + 1, &t.bytes(counts_offset, 4).unwrap())
                .unwrap(),
        )
        .unwrap();
        file.write_all(
            &t.entry(METADATA, 1, data_size, &t.bytes(data_offset, 4).unwrap())
                .unwrap(),
        )
        .unwrap();
        file.write_all(&t.bytes(0, 4).unwrap()).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&t.bytes(ifd_offset, 4).unwrap()).unwrap();
    }

    #[test]
    fn combined_tag_bytes_and_zero_length_item_allocations_are_bounded() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aperture-tiff-budget-{stamp}.tif"));
        metadata_fixture(&path, 1, LIMIT - 4);
        assert!(read(&path).unwrap_err().contains("size limit"));
        // Counts/data total only about 5 MiB, but the claimed empty Vec headers
        // alone would consume the entire 32 MiB allocation budget.
        metadata_fixture(&path, LIMIT / ITEM_SIZE, 12);
        assert!(read(&path).unwrap_err().contains("size limit"));
        // A few empty unknown items still round-trip as ImageJ metadata.
        metadata_fixture(&path, 2, 12);
        assert_eq!(
            read(&path).unwrap().overlays,
            vec![Vec::<u8>::new(), Vec::<u8>::new()]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn writer_limits_match_reader_without_capping_points_in_one_roi() {
        // One large ROI remains near the byte limit after container/header costs.
        assert_eq!(
            metadata_sizes(LIMIT - ITEM_SIZE - BLOCK_SIZE, 1, 1).unwrap(),
            (LIMIT - ITEM_SIZE - BLOCK_SIZE + 12, 8)
        );
        assert!(metadata_sizes(LIMIT - 19, 1, 1).is_err());
        assert!(metadata_sizes(0, LIMIT / ITEM_SIZE, 1).is_err());
        assert!(metadata_sizes(0, 0, LIMIT / BLOCK_SIZE + 1).is_err());
        assert!(metadata_sizes(u64::MAX, 1, 1).is_err());
        assert!(metadata_sizes(0, u64::MAX, 1).is_err());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aperture-tiff-writer-budget-{stamp}"));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source.tif");
        let target = dir.join("target.tif");
        fixture(&source, true, false);
        let before = fs::read(&source).unwrap();
        let overlays = vec![vec![]; (LIMIT / ITEM_SIZE) as usize];
        assert!(save_copy(&source, &target, vec![], overlays, false)
            .unwrap_err()
            .contains("size limit"));
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(!target.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // Large selections stay one item, so no small per-ROI point ceiling is
        // imposed by the metadata container guard.
        let active = vec![7; 2 * 1024 * 1024];
        save_copy(&source, &target, active.clone(), vec![], false).unwrap();
        assert_eq!(read(&target).unwrap().active_roi, Some(active));
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
