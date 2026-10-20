//! Combine editable point ROIs with untouched ImageJ selections and image pixels.
use crate::{
    roi::{self, PointRoi},
    tiff_annotations::{self, TiffAnnotations},
};
#[cfg(test)]
use std::process::Command;
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
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

pub struct AnnotationBasis {
    pub points: Vec<PointRoi>,
    original: TiffAnnotations,
}

impl AnnotationBasis {
    fn from_annotations(original: TiffAnnotations) -> Result<Self, String> {
        Ok(Self {
            points: points(&original)?,
            original,
        })
    }
}

pub fn load_basis(source: &Path) -> Result<AnnotationBasis, String> {
    let original = if is_tiff(source)? {
        tiff_annotations::read(source)?
    } else {
        TiffAnnotations::default()
    };
    AnnotationBasis::from_annotations(original)
}

fn edited_metadata(
    basis: &AnnotationBasis,
    rois: Vec<PointRoi>,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let active_index = rois.iter().rposition(|roi| !roi.overlay);
    let mut active = vec![];
    let mut replacements = vec![Vec::new(); basis.points.len()];
    let mut added = vec![];
    for (index, mut current) in rois.into_iter().enumerate() {
        // The immutable sourceIndex basis supplies metadata and overlay slots
        // through deletions, counter splits, and repeated in-place saves.
        current.raw = match current.source_index {
            Some(source_index) => basis
                .points
                .get(source_index)
                .ok_or("Original point ROI index is out of range")?
                .raw
                .clone(),
            None => vec![],
        };
        if current.points.is_empty() {
            continue;
        }
        let encoded = roi::encode(&current)?;
        if Some(index) == active_index {
            active = encoded;
        } else if let Some(source_index) = current.source_index {
            replacements[source_index].push(encoded);
        } else {
            added.push(encoded);
        }
    }
    let mut overlays = vec![];
    let mut source_index = 0;
    for bytes in &basis.original.overlays {
        if roi::decode(bytes)?.is_some() {
            overlays.append(&mut replacements[source_index]);
            source_index += 1;
        } else {
            overlays.push(bytes.clone());
        }
    }
    if let Some(bytes) = &basis.original.active_roi {
        if roi::decode(bytes)?.is_some() {
            // An original active point selection demoted by a newer selection
            // follows all original overlays, including its split counter groups.
            overlays.append(&mut replacements[source_index]);
        } else if active_index.is_some() {
            // Even an empty point selection supersedes the original active ROI.
            // Rebuilding from the same basis prevents duplicates on later saves.
            overlays.push(bytes.clone());
        } else {
            active = bytes.clone();
        }
    }
    overlays.extend(added);
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
    let same_file = same_file::is_same_file(source, target).unwrap_or(false);
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

#[cfg(test)]
pub fn save(
    source: &Path,
    target: &Path,
    rois: Vec<PointRoi>,
    basis: &AnnotationBasis,
    overwrite: bool,
) -> Result<(), String> {
    stage_save(source, target, rois, basis, overwrite)?.commit()
}

pub fn stage_save(
    source: &Path,
    target: &Path,
    rois: Vec<PointRoi>,
    basis: &AnnotationBasis,
    overwrite: bool,
) -> Result<tiff_annotations::StagedSave, String> {
    let source_is_tiff = is_tiff(source)?;
    check_target(source, target, source_is_tiff, overwrite)?;
    let (active, overlays) = edited_metadata(basis, rois)?;
    if source_is_tiff {
        return tiff_annotations::stage_copy(source, target, active, overlays, overwrite);
    }
    let source = fs::canonicalize(source).map_err(|e| e.to_string())?;
    let extension = source
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp") {
        return Err("Unsupported image format for annotated TIFF conversion".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temp_parent = if cfg!(windows) {
        std::env::temp_dir()
    } else {
        target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    };
    let temp = temp_parent.join(format!(".aperture-convert-{}-{stamp}", std::process::id()));
    fs::create_dir(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        let alias = format!("source.{extension}");
        // libvips interprets filename brackets as loader options. Only pass our
        // fixed relative names, even when the source or destination contains them.
        crate::platform::source_alias(&source, &temp.join(&alias)).map_err(|e| e.to_string())?;
        if cfg!(windows) || extension == "bmp" {
            fs::create_dir_all(temp.join("tmp")).map_err(|e| e.to_string())?;
            let decoder = crate::decoder::Decoder::start(&temp)?;
            decoder.call(
                &crate::vips_worker::Request::Command {
                    program: "vips".into(),
                    args: vec![
                        "tiffsave".into(),
                        temp.join(&alias).to_string_lossy().into_owned(),
                        temp.join("converted.tif").to_string_lossy().into_owned(),
                        "--compression=lzw".into(),
                    ],
                },
                &std::sync::atomic::AtomicBool::new(false),
                || false,
                |_| {},
            )?;
        } else {
            let output = crate::platform::tool("vips")
                .current_dir(&temp)
                .env("TMPDIR", ".")
                .args(["tiffsave", &alias, "converted.tif", "--compression=lzw"])
                .args(crate::vips_worker::tool_options())
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
        }
        check_target(&source, target, false, overwrite)?;
        tiff_annotations::stage_copy(
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
        super::edited_metadata(&AnnotationBasis::from_annotations(original)?, rois)
    }

    fn save(source: &Path, target: &Path, rois: Vec<PointRoi>) -> Result<(), String> {
        super::save(source, target, rois, &load_basis(source)?, true)
    }

    fn load(source: &Path) -> Result<Vec<PointRoi>, String> {
        Ok(load_basis(source)?.points)
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
        assert_eq!(overlays, vec![bytes, nonpoint]);
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
        for (actual, expected) in saved
            .iter()
            .zip([1, 0, 2, 3].map(|index| &submitted[index]))
        {
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
        for (group, source) in [(0, 0), (1, 1), (2, 1)] {
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
        let basis = load_basis(&target).unwrap();
        let target_before = fs::read(&target).unwrap();
        assert!(super::save(&target, &target, basis.points.clone(), &basis, false).is_err());
        assert_eq!(fs::read(&target).unwrap(), target_before);
        super::save(&target, &target, basis.points.clone(), &basis, true).unwrap();
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
    fn interleaved_overlays_keep_slots_through_edits_deletion_and_repeated_saves() {
        let point_one = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut other = point();
        other.raw.clear();
        other.points[0].x = 80.25;
        let point_two = roi::encode(&other).unwrap();
        let mut before = point_one.clone();
        before[6] = 1;
        let mut between = point_one.clone();
        between[6] = 2;
        let mut after = point_one.clone();
        after[6] = 3;
        let original_overlays = vec![
            before.clone(),
            point_one,
            between.clone(),
            point_two,
            after.clone(),
        ];
        let original = TiffAnnotations {
            active_roi: None,
            overlays: original_overlays.clone(),
        };
        let basis = AnnotationBasis::from_annotations(original).unwrap();
        let (active, overlays) = super::edited_metadata(&basis, basis.points.clone()).unwrap();
        assert!(active.is_empty());
        assert_eq!(overlays, original_overlays);

        // Load the immutable layout through the actual file API and save in place.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aperture-overlay-order-{stamp}.tif"));
        fs::write(&path, [b'I', b'I', 42, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        tiff_annotations::save_copy(&path, &path, active, overlays, true).unwrap();
        let basis = load_basis(&path).unwrap();
        let mut edited = basis.points[0].clone();
        edited.points[0].x += 0.5;
        let edited_bytes = roi::encode(&edited).unwrap();
        let mut deleted = basis.points[1].clone();
        deleted.points.clear();
        let mut added = point();
        added.overlay = true;
        added.points[0].x = 25.5;
        added.raw.clear();
        let added_bytes = roi::encode(&added).unwrap();
        // The submitted array order must not move existing overlays across a shape.
        let submitted = vec![deleted, added, edited];
        let expected = vec![
            before.clone(),
            edited_bytes,
            between.clone(),
            after.clone(),
            added_bytes,
        ];
        for _ in 0..2 {
            super::save(&path, &path, submitted.clone(), &basis, true).unwrap();
            let written = tiff_annotations::read(&path).unwrap();
            assert!(written.active_roi.is_none());
            assert_eq!(written.overlays, expected);
        }
        // Clear the last original group and save again, leaving only the new group.
        let mut cleared = submitted.clone();
        cleared[2].points.clear();
        super::save(&path, &path, cleared, &basis, true).unwrap();
        let written = tiff_annotations::read(&path).unwrap();
        assert_eq!(
            written.overlays,
            vec![before, between, after, expected[4].clone()]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn nonpoint_active_demotion_is_ordered_and_not_restored_after_clearing_points() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut shape = bytes.clone();
        shape[6] = 1;
        let mut selected_shape = bytes.clone();
        selected_shape[6] = 2;
        let basis = AnnotationBasis::from_annotations(TiffAnnotations {
            active_roi: Some(selected_shape.clone()),
            overlays: vec![bytes.clone(), shape.clone()],
        })
        .unwrap();
        let mut submitted = basis.points.clone();
        let mut new = point();
        new.points[0].x = 50.5;
        submitted.push(new);
        for _ in 0..2 {
            let (active, overlays) = super::edited_metadata(&basis, submitted.clone()).unwrap();
            assert_eq!(roi::decode(&active).unwrap().unwrap().points[0].x, 50.5);
            assert_eq!(
                overlays,
                vec![bytes.clone(), shape.clone(), selected_shape.clone()]
            );
        }
        submitted.last_mut().unwrap().points.clear();
        let (active, overlays) = super::edited_metadata(&basis, submitted).unwrap();
        assert!(active.is_empty());
        assert_eq!(overlays, vec![bytes, shape, selected_shape]);
    }

    #[test]
    fn demoted_active_point_and_new_overlays_follow_the_original_overlay_sequence() {
        let bytes = include_bytes!("fixtures/imagej-points.roi").to_vec();
        let mut shape = bytes.clone();
        shape[6] = 1;
        let basis = AnnotationBasis::from_annotations(TiffAnnotations {
            active_roi: Some(bytes.clone()),
            overlays: vec![shape.clone(), bytes.clone()],
        })
        .unwrap();
        let mut submitted = basis.points.clone();
        let mut added = point();
        added.overlay = true;
        added.points[0].x = 60.5;
        added.raw.clear();
        let added_bytes = roi::encode(&added).unwrap();
        submitted.push(added);
        let mut active = point();
        active.points[0].x = 70.5;
        submitted.push(active);
        let (active, overlays) = super::edited_metadata(&basis, submitted).unwrap();
        assert_eq!(roi::decode(&active).unwrap().unwrap().points[0].x, 70.5);
        assert_eq!(overlays, vec![shape, bytes.clone(), bytes, added_bytes]);
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
