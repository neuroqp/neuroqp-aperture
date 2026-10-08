//! Combine editable point ROIs with untouched ImageJ selections and image pixels.
use crate::{
    roi::{self, PointRoi},
    tiff_annotations::{self, TiffAnnotations},
};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn tiff_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("tif") || s.eq_ignore_ascii_case("tiff"))
}

pub fn is_tiff(path: &Path) -> Result<bool, String> {
    let mut header = Vec::with_capacity(4);
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(4)
        .read_to_end(&mut header)
        .map_err(|e| e.to_string())?;
    if header.starts_with(b"II") || header.starts_with(b"MM") {
        // Let the TIFF parser reject unsupported or malformed TIFF headers.
        Ok(true)
    } else if tiff_extension(path) {
        Err("The source has a TIFF extension but an invalid TIFF header".into())
    } else {
        Ok(false)
    }
}

fn points(annotations: &TiffAnnotations) -> Result<Vec<PointRoi>, String> {
    let mut rois: Vec<PointRoi> = annotations
        .overlays
        .iter()
        .map(|bytes| (bytes, true))
        .chain(annotations.active_roi.iter().map(|bytes| (bytes, false)))
        .filter_map(|(bytes, overlay)| {
            roi::decode(bytes)
                .map(|decoded| {
                    decoded.map(|mut roi| {
                        roi.overlay = overlay;
                        roi
                    })
                })
                .transpose()
        })
        .collect::<Result<_, _>>()?;
    for (index, roi) in rois.iter_mut().enumerate() {
        roi.source_index = Some(index);
    }
    Ok(rois)
}

pub fn load(source: &Path) -> Result<Vec<PointRoi>, String> {
    if is_tiff(source)? {
        points(&tiff_annotations::read(source)?)
    } else {
        Ok(vec![])
    }
}

fn edited_metadata(
    original: TiffAnnotations,
    rois: Vec<PointRoi>,
    basis: Option<&[PointRoi]>,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let live = points(&original)?;
    let existing = basis.unwrap_or(&live);
    let mut overlays = vec![];
    for bytes in original.overlays {
        if roi::decode(&bytes)?.is_none() {
            overlays.push(bytes);
        }
    }
    let mut active = original
        .active_roi
        .filter(|bytes| matches!(roi::decode(bytes), Ok(None)))
        .unwrap_or_default();
    let active_index = rois.iter().rposition(|roi| !roi.overlay);
    let mut encoded = vec![];
    for (index, mut current) in rois.into_iter().enumerate() {
        // Restore source metadata without accepting arbitrary ROI bytes through IPC.
        current.raw = match current.source_index {
            Some(index) => existing
                .get(index)
                .ok_or("Original point ROI index is out of range")?
                .raw
                .clone(),
            None => vec![],
        };
        if !current.points.is_empty() {
            encoded.push((Some(index) != active_index, roi::encode(&current)?));
        }
    }
    if let Some(index) = encoded.iter().rposition(|(overlay, _)| !overlay) {
        let (_, last) = encoded.remove(index);
        if !active.is_empty() {
            overlays.push(active);
        }
        active = last;
    }
    overlays.extend(encoded.into_iter().map(|(_, bytes)| bytes));
    Ok((active, overlays))
}

fn check_target(
    source: &Path,
    target: &Path,
    source_is_tiff: bool,
    overwrite: bool,
) -> Result<(), String> {
    let source_path = fs::canonicalize(source).map_err(|e| e.to_string())?;
    let same_path = fs::canonicalize(target).ok().as_ref() == Some(&source_path);
    if !tiff_extension(target) && !(source_is_tiff && same_path) {
        return Err("Save annotations with a .tif or .tiff extension".into());
    }
    #[cfg(unix)]
    let same_file = {
        use std::os::unix::fs::MetadataExt;
        let original = fs::metadata(source).map_err(|e| e.to_string())?;
        fs::metadata(target)
            .is_ok_and(|target| original.dev() == target.dev() && original.ino() == target.ino())
    };
    #[cfg(not(unix))]
    let same_file = false;
    if !source_is_tiff && (same_path || same_file) {
        return Err(
            "Save annotations to a TIFF; the original non-TIFF image cannot be overwritten".into(),
        );
    }
    if !overwrite && fs::symlink_metadata(target).is_ok() {
        return Err("The destination exists. Confirm overwrite before saving.".into());
    }
    Ok(())
}

pub fn save(
    source: &Path,
    target: &Path,
    rois: Vec<PointRoi>,
    basis: &[PointRoi],
    overwrite: bool,
) -> Result<(), String> {
    let source_is_tiff = is_tiff(source)?;
    check_target(source, target, source_is_tiff, overwrite)?;
    if source_is_tiff {
        let (active, overlays) =
            edited_metadata(tiff_annotations::read(source)?, rois, Some(basis))?;
        return tiff_annotations::save_copy(source, target, active, overlays, overwrite);
    }
    let (active, overlays) = edited_metadata(TiffAnnotations::default(), rois, Some(basis))?;
    let source = fs::canonicalize(source).map_err(|e| e.to_string())?;
    let extension = source
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp") {
        return Err("Unsupported image format for annotated TIFF conversion".into());
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temp = parent.join(format!(".aperture-convert-{}-{stamp}", std::process::id()));
    fs::create_dir(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        let alias = format!("source.{extension}");
        // libvips interprets filename brackets as loader options. Only pass our
        // fixed relative names, even when the source or destination contains them.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, temp.join(&alias)).map_err(|e| e.to_string())?;
        #[cfg(not(unix))]
        fs::copy(&source, temp.join(&alias)).map_err(|e| e.to_string())?;
        let output = Command::new("vips")
            .current_dir(&temp)
            .env("TMPDIR", ".")
            .args([
                "tiffsave",
                &alias,
                "converted.tif",
                "--compression=lzw",
                "--vips-concurrency=2",
                "--vips-cache-max=0",
                "--vips-cache-max-memory=67108864",
                "--vips-disc-threshold=32m",
            ])
            .output()
            .map_err(|e| {
                format!("Cannot convert the image to TIFF: {e}. Install libvips tools.")
            })?;
        if !output.status.success() {
            return Err(format!(
                "Cannot convert the image to TIFF: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        check_target(&source, target, false, overwrite)?;
        tiff_annotations::save_copy(
            &temp.join("converted.tif"),
            target,
            active,
            overlays,
            overwrite,
        )
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edited_metadata(
        original: TiffAnnotations,
        rois: Vec<PointRoi>,
    ) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
        super::edited_metadata(original, rois, None)
    }

    fn save(source: &Path, target: &Path, rois: Vec<PointRoi>) -> Result<(), String> {
        super::save(source, target, rois, &load(source)?, true)
    }

    fn point() -> PointRoi {
        roi::decode(include_bytes!("fixtures/imagej-points.roi"))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn overlay_only_points_remain_overlays_without_creating_an_active_selection() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let original = TiffAnnotations {
            active_roi: None,
            overlays: vec![bytes.clone()],
        };
        let loaded = points(&original).unwrap();
        assert!(loaded[0].overlay);
        let (active, overlays) = edited_metadata(original, loaded).unwrap();
        assert!(active.is_empty());
        assert_eq!(overlays, vec![bytes]);
    }

    #[test]
    fn point_overlays_preserve_nonpoint_active_selection_until_a_new_point_selection_is_added() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut nonpoint = bytes.clone();
        nonpoint[6] = 1;
        let original = || TiffAnnotations {
            active_roi: Some(nonpoint.clone()),
            overlays: vec![bytes.clone()],
        };
        let loaded = points(&original()).unwrap();
        assert!(loaded[0].overlay);
        let (active, overlays) = edited_metadata(original(), loaded.clone()).unwrap();
        assert_eq!(active, nonpoint);
        assert_eq!(overlays, vec![bytes.clone()]);

        let mut submitted = loaded;
        let mut new_selection = point();
        new_selection.points[0].x += 1.0;
        assert!(!new_selection.overlay);
        submitted.push(new_selection.clone());
        let (active, overlays) = edited_metadata(original(), submitted).unwrap();
        assert_eq!(
            roi::decode(&active).unwrap().unwrap().points,
            new_selection.points
        );
        assert_eq!(overlays, vec![nonpoint, bytes]);
    }

    #[test]
    fn edits_restore_raw_metadata_and_preserve_nonpoint_selections() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut nonpoint = bytes.clone();
        nonpoint[6] = 1;
        let original = TiffAnnotations {
            active_roi: Some(nonpoint.clone()),
            overlays: vec![bytes.clone(), nonpoint.clone()],
        };
        let mut submitted = point();
        submitted.source_index = Some(0);
        submitted.raw.clear();
        let (active, overlays) = edited_metadata(original, vec![submitted]).unwrap();
        assert_eq!(active, bytes);
        assert_eq!(overlays, vec![nonpoint.clone(), nonpoint]);
    }

    #[test]
    fn deleting_all_points_clears_active_and_point_overlays_but_preserves_other_rois() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut rectangle = bytes.clone();
        rectangle[6] = 1;
        let original = TiffAnnotations {
            active_roi: Some(bytes.clone()),
            overlays: vec![bytes, rectangle.clone()],
        };
        let mut cleared = points(&original).unwrap();
        for roi in &mut cleared {
            roi.points.clear();
        }
        let (active, overlays) = edited_metadata(original, cleared).unwrap();
        assert!(active.is_empty());
        assert_eq!(overlays, vec![rectangle]);
    }

    #[test]
    fn deleting_the_last_active_selection_does_not_promote_an_earlier_selection() {
        let original = TiffAnnotations {
            active_roi: Some(roi::encode(&point()).unwrap()),
            overlays: vec![],
        };
        let previous = points(&original).unwrap().remove(0);
        let mut cleared = point();
        cleared.points.clear();
        let (active, overlays) =
            edited_metadata(original, vec![previous.clone(), cleared]).unwrap();
        assert!(active.is_empty());
        assert_eq!(overlays.len(), 1);
        assert_eq!(
            roi::decode(&overlays[0]).unwrap().unwrap().points,
            previous.points
        );
    }

    #[test]
    fn cleared_groups_do_not_shift_metadata_and_last_group_is_active() {
        let original_bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut first = point();
        first.raw.clear();
        let original = TiffAnnotations {
            active_roi: Some(original_bytes.clone()),
            overlays: vec![roi::encode(&first).unwrap()],
        };
        let mut cleared = point();
        cleared.source_index = Some(0);
        cleared.points.clear();
        let mut existing = point();
        existing.source_index = Some(1);
        existing.raw.clear();
        let mut appended = point();
        appended.raw.clear();
        appended.points[0].x = 41.5;
        let (active, overlays) =
            edited_metadata(original, vec![cleared, existing.clone(), appended.clone()]).unwrap();
        assert_eq!(
            roi::decode(&active).unwrap().unwrap().points,
            appended.points
        );
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0], original_bytes);
        assert_eq!(
            roi::decode(&overlays[0]).unwrap().unwrap().points,
            existing.points
        );
        assert_eq!(
            roi::decode(&overlays[0]).unwrap().unwrap().frame,
            existing.frame
        );
    }

    #[test]
    fn split_counter_groups_keep_source_metadata_after_reordering_and_reopening() {
        let mut mixed = point();
        mixed.points[0].counter = 1;
        mixed.points[1].counter = 2;
        let mixed_bytes = roi::encode(&mixed).unwrap();
        let mut other_bytes = mixed_bytes.clone();
        let h = u32::from_be_bytes(other_bytes[60..64].try_into().unwrap()) as usize;
        let name = u32::from_be_bytes(other_bytes[h + 16..h + 20].try_into().unwrap()) as usize;
        for (i, letter) in "overlay".encode_utf16().enumerate() {
            other_bytes[name + i * 2..name + i * 2 + 2].copy_from_slice(&letter.to_be_bytes());
        }
        let original = TiffAnnotations {
            active_roi: Some(mixed_bytes),
            overlays: vec![other_bytes],
        };
        let loaded = points(&original).unwrap();
        assert_eq!(loaded[0].source_index, Some(0));
        assert_eq!(loaded[1].source_index, Some(1));
        let mut counter_one = loaded[1].clone();
        counter_one.points.retain(|p| p.counter == 1);
        counter_one.point_type = 1;
        counter_one.size = 5;
        counter_one.color = Some(0xff00ff00);
        counter_one.show_labels = false;
        let mut counter_two = loaded[1].clone();
        counter_two.points.retain(|p| p.counter == 2);
        let mut added = point();
        added.points.truncate(1);
        added.points[0].x = 41.5;
        // Raw bytes from a new frontend group must never supply source metadata.
        assert_eq!(added.source_index, None);
        let submitted = vec![counter_one, loaded[0].clone(), counter_two, added];
        let (active, overlays) = edited_metadata(original, submitted.clone()).unwrap();
        let reopened = TiffAnnotations {
            active_roi: Some(active),
            overlays,
        };
        let saved = points(&reopened).unwrap();
        assert_eq!(saved.len(), 4);
        for (actual, expected) in saved.iter().zip(&submitted) {
            assert_eq!(actual.points, expected.points);
            assert_eq!(actual.point_type, expected.point_type);
            assert_eq!(actual.size, expected.size);
            assert_eq!(actual.color, expected.color);
            assert_eq!(actual.show_labels, expected.show_labels);
            assert_eq!(actual.frame, expected.frame);
        }
        let text = |raw: &[u8], offset: usize| {
            let h = u32::from_be_bytes(raw[60..64].try_into().unwrap()) as usize;
            let start =
                u32::from_be_bytes(raw[h + offset..h + offset + 4].try_into().unwrap()) as usize;
            let len = u32::from_be_bytes(raw[h + offset + 4..h + offset + 8].try_into().unwrap())
                as usize;
            raw[start..start + len * 2].to_vec()
        };
        for (group, source) in [(0, 1), (1, 0), (2, 1)] {
            assert_eq!(text(&saved[group].raw, 16), text(&loaded[source].raw, 16));
            assert_eq!(text(&saved[group].raw, 40), text(&loaded[source].raw, 40));
        }
        assert!(text(&saved[3].raw, 16).is_empty());
        assert!(text(&saved[3].raw, 40).is_empty());
        let (active, overlays) = edited_metadata(
            TiffAnnotations {
                active_roi: reopened.active_roi.clone(),
                overlays: reopened.overlays.clone(),
            },
            saved,
        )
        .unwrap();
        assert_eq!(Some(active), reopened.active_roi);
        assert_eq!(overlays, reopened.overlays);

        let mut invalid = submitted[0].clone();
        invalid.source_index = Some(99);
        assert!(edited_metadata(reopened, vec![invalid]).is_err());
    }

    #[test]
    fn converts_png_without_changing_source_and_rejects_source_aliases() {
        if Command::new("vips").arg("--version").output().is_err() {
            return;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aperture-annotation-test-{}-{stamp}-[image]",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("vips")
                .current_dir(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["black", "image.v", "4", "3", "--bands=3"]);
        run(&["linear", "image.v", "source.png", "1", "73"]);
        run(&["rawsave", "source.png", "original.raw"]);
        let source = dir.join("source.png");
        let before = fs::read(&source).unwrap();
        let target = dir.join("copy.tiff");
        save(&source, &target, vec![point()]).unwrap();
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(load(&target).unwrap()[0].points, point().points);
        run(&["rawsave", "copy.tiff", "saved.raw"]);
        assert_eq!(
            fs::read(dir.join("original.raw")).unwrap(),
            fs::read(dir.join("saved.raw")).unwrap()
        );
        let basis = load(&target).unwrap();
        let target_before = fs::read(&target).unwrap();
        assert!(super::save(&target, &target, basis.clone(), &basis, false).is_err());
        assert_eq!(fs::read(&target).unwrap(), target_before);
        super::save(&target, &target, basis.clone(), &basis, true).unwrap();
        run(&["rawsave", "copy.tiff", "saved.raw"]);
        assert_eq!(
            fs::read(dir.join("original.raw")).unwrap(),
            fs::read(dir.join("saved.raw")).unwrap()
        );
        #[cfg(unix)]
        {
            let alias = dir.join("alias.tif");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            assert!(save(&source, &alias, vec![point()]).is_err());
            fs::remove_file(&alias).unwrap();
            fs::hard_link(&source, &alias).unwrap();
            assert!(save(&source, &alias, vec![point()]).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repeated_overwrites_use_initial_metadata_after_deletion_and_reordering() {
        let first_bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut second = roi::decode(&first_bytes).unwrap().unwrap();
        second.raw.clear();
        second.points[0].x = 80.25;
        let second_bytes = roi::encode(&second).unwrap();
        let original = TiffAnnotations {
            active_roi: Some(second_bytes),
            overlays: vec![first_bytes.clone()],
        };
        let basis = points(&original).unwrap();
        let mut reordered = basis.clone();
        reordered.swap(0, 1);
        let (active, overlays) =
            super::edited_metadata(original, reordered.clone(), Some(&basis)).unwrap();
        let live = TiffAnnotations {
            active_roi: Some(active),
            overlays,
        };
        let (active, overlays) = super::edited_metadata(live, reordered, Some(&basis)).unwrap();
        let live = TiffAnnotations {
            active_roi: Some(active),
            overlays,
        };
        // Clearing the earlier source group leaves only sourceIndex=1 on disk.
        let mut deleted = basis[0].clone();
        deleted.points.clear();
        let survivor = basis[1].clone();
        let submitted = vec![deleted, survivor.clone()];
        let (active, overlays) =
            super::edited_metadata(live, submitted.clone(), Some(&basis)).unwrap();
        assert!(overlays.is_empty());
        let live = TiffAnnotations {
            active_roi: Some(active),
            overlays,
        };
        assert_eq!(points(&live).unwrap()[0].source_index, Some(0));
        let (active, overlays) = super::edited_metadata(live, submitted, Some(&basis)).unwrap();
        assert!(overlays.is_empty());
        assert_eq!(
            roi::decode(&active).unwrap().unwrap().points,
            survivor.points
        );
        assert_eq!(active, basis[1].raw);
        assert_ne!(active, first_bytes);
    }

    #[test]
    fn tiff_header_detection_does_not_convert_malformed_tiffs() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aperture-header-{stamp}.tif"));
        fs::write(&path, b"not a TIFF").unwrap();
        assert!(load(&path).unwrap_err().contains("invalid TIFF header"));
        let disguised = path.with_extension("png");
        fs::write(&disguised, b"II\x2a\0\xff\xff\xff\xff").unwrap();
        assert!(is_tiff(&disguised).unwrap());
        assert!(load(&disguised).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_file(disguised).unwrap();
    }
}
