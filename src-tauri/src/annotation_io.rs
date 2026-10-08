//! Combine editable point ROIs with untouched ImageJ selections and image pixels.
use crate::{
    roi::{self, PointRoi},
    tiff_annotations::{self, TiffAnnotations},
};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn is_tiff(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("tif") || s.eq_ignore_ascii_case("tiff"))
}

fn points(annotations: &TiffAnnotations) -> Result<Vec<PointRoi>, String> {
    annotations
        .overlays
        .iter()
        .chain(annotations.active_roi.iter())
        .filter_map(|bytes| roi::decode(bytes).transpose())
        .collect()
}

pub fn load(source: &Path) -> Result<Vec<PointRoi>, String> {
    if is_tiff(source) {
        points(&tiff_annotations::read(source)?)
    } else {
        Ok(vec![])
    }
}

fn edited_metadata(
    original: TiffAnnotations,
    rois: Vec<PointRoi>,
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let existing = points(&original)?;
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
    let mut encoded = vec![];
    for (index, mut current) in rois.into_iter().enumerate() {
        // The frontend retains group order and appends groups. Restore metadata
        // from the source rather than accepting arbitrary ROI bytes through IPC.
        current.raw = existing
            .get(index)
            .map(|r| r.raw.clone())
            .unwrap_or_default();
        if !current.points.is_empty() {
            encoded.push(roi::encode(&current)?);
        }
    }
    if let Some(last) = encoded.pop() {
        if !active.is_empty() {
            overlays.push(active);
        }
        active = last;
    }
    overlays.extend(encoded);
    Ok((active, overlays))
}

fn check_target(source: &Path, target: &Path) -> Result<(), String> {
    if !is_tiff(target) {
        return Err("Save the annotated copy with a .tif or .tiff extension".into());
    }
    let source_path = fs::canonicalize(source).map_err(|e| e.to_string())?;
    let same_path = fs::canonicalize(target).ok().as_ref() == Some(&source_path);
    #[cfg(unix)]
    let same_file = {
        use std::os::unix::fs::MetadataExt;
        let original = fs::metadata(source).map_err(|e| e.to_string())?;
        fs::metadata(target)
            .is_ok_and(|target| original.dev() == target.dev() && original.ino() == target.ino())
    };
    #[cfg(not(unix))]
    let same_file = false;
    if same_path || same_file {
        return Err(
            "Save annotations to a different TIFF; source images cannot be overwritten".into(),
        );
    }
    Ok(())
}

pub fn save(source: &Path, target: &Path, rois: Vec<PointRoi>) -> Result<(), String> {
    check_target(source, target)?;
    if is_tiff(source) {
        let (active, overlays) = edited_metadata(tiff_annotations::read(source)?, rois)?;
        return tiff_annotations::save_copy(source, target, active, overlays);
    }
    let (active, overlays) = edited_metadata(TiffAnnotations::default(), rois)?;
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
        check_target(&source, target)?;
        tiff_annotations::save_copy(&temp.join("converted.tif"), target, active, overlays)
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> PointRoi {
        roi::decode(include_bytes!("fixtures/imagej-points.roi"))
            .unwrap()
            .unwrap()
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
        submitted.raw.clear();
        let (active, overlays) = edited_metadata(original, vec![submitted]).unwrap();
        assert_eq!(active, bytes);
        assert_eq!(overlays, vec![nonpoint.clone(), nonpoint]);
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
        cleared.points.clear();
        let mut existing = point();
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
        let target_before = fs::read(&target).unwrap();
        assert!(save(&target, &target, vec![point()]).is_err());
        assert_eq!(fs::read(&target).unwrap(), target_before);
        #[cfg(unix)]
        {
            let alias = dir.join("alias.tif");
            std::os::unix::fs::symlink(&target, &alias).unwrap();
            assert!(save(&target, &alias, vec![point()]).is_err());
            fs::remove_file(&alias).unwrap();
            fs::hard_link(&target, &alias).unwrap();
            assert!(save(&target, &alias, vec![point()]).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
