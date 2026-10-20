//! Annotation loading and saving, including overwrite warnings for shared files.
use crate::{
    annotation_io, backend,
    backend::Backend,
    instances::{fingerprint, path_key},
    roi, Session,
};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};
use tauri::State;

fn annotation_job(backend: &Backend, id: &str) -> Result<std::sync::Arc<backend::Job>, String> {
    let job = backend
        .current
        .lock()
        .unwrap()
        .clone()
        .ok_or("No image open")?;
    let state = job.state.lock().unwrap();
    if state.id != id {
        return Err("The image has changed".into());
    }
    if state.read_only {
        return Err(
            "This image is browsing-only (read-only). Annotation editing and export are not supported yet."
                .into(),
        );
    }
    drop(state);
    Ok(job)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnnotationTarget {
    path: String,
    exists: bool,
    is_tiff: bool,
    warning: Option<String>,
    acknowledgment: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveResult {
    saved: bool,
    warning: Option<String>,
    acknowledgment: String,
}

fn save_warning(session: &Session, id: &str, target: &Path) -> Result<SaveResult, String> {
    let loaded = session.loaded.lock().unwrap();
    let loaded = loaded
        .as_ref()
        .filter(|loaded| loaded.id == id)
        .ok_or("The image has changed")?;
    let target = path_key(target).map_err(|e| e.to_string())?;
    let source_fingerprint = fingerprint(&loaded.source).map_err(|e| e.to_string())?;
    let target_fingerprint = fingerprint(&target).map_err(|e| e.to_string())?;
    let others_open = session
        .instances
        .others_open(&target)
        .map_err(|e| e.to_string())?;
    let source_changed = loaded.fingerprints.get(&loaded.source) != Some(&source_fingerprint);
    let target_changed = match loaded.fingerprints.get(&target) {
        Some(previous) => previous != &target_fingerprint,
        None => target_fingerprint.is_some(),
    };
    let mut warnings = Vec::new();
    if others_open {
        warnings.push("This file is open in another Aperture window. Saving may overwrite that window's annotations.");
    }
    if source_changed {
        warnings.push("The source image changed on disk since it was opened or last saved.");
    }
    if target_changed && target != loaded.source {
        warnings.push("The destination already exists or changed on disk since the last save.");
    }
    Ok(SaveResult {
        saved: false,
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
        acknowledgment: format!(
            "{target:?}|{source_fingerprint:?}|{target_fingerprint:?}|{others_open}"
        ),
    })
}

// Called only after the destination commit has succeeded. Tracking failures
// cannot turn that completed write into a failed save or trust old baselines.
fn finish_save(session: &Session, id: &str, target: &Path, mut status: SaveResult) -> SaveResult {
    let refresh = (|| -> Result<(), String> {
        let mut loaded = session.loaded.lock().unwrap();
        let loaded = loaded
            .as_mut()
            .filter(|loaded| loaded.id == id)
            .ok_or("The image has changed")?;
        loaded.fingerprints.clear();
        let target = path_key(target).map_err(|e| e.to_string())?;
        let source_fingerprint = fingerprint(&loaded.source)
            .map_err(|e| e.to_string())?
            .ok_or("The source image is no longer present.")?;
        let target_fingerprint = fingerprint(&target)
            .map_err(|e| e.to_string())?
            .ok_or("The saved file is no longer present.")?;
        session
            .instances
            .set_images(&[loaded.source.clone(), target.clone()])
            .map_err(|e| e.to_string())?;
        loaded
            .fingerprints
            .insert(loaded.source.clone(), Some(source_fingerprint));
        loaded.fingerprints.insert(target, Some(target_fingerprint));
        Ok(())
    })();
    status.saved = true;
    status.warning = refresh.err().map(|error| format!(
        "Saved, but file and window tracking could not be refreshed: {error}. Later saves may show additional overwrite warnings."
    ));
    status
}

#[tauri::command]
pub(crate) async fn annotation_target(
    id: String,
    path: Option<String>,
    backend: State<'_, Backend>,
    session: State<'_, Arc<Session>>,
) -> Result<AnnotationTarget, String> {
    let job = annotation_job(&backend, &id)?;
    let session = session.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let is_tiff = annotation_io::is_tiff(&job.source)?;
        let target = path.map(std::path::PathBuf::from).unwrap_or_else(|| {
            if is_tiff {
                job.source.clone()
            } else {
                job.source.with_extension("tif")
            }
        });
        let _writing = session
            .instances
            .save_lock(&target)
            .map_err(|e| e.to_string())?;
        let status = save_warning(&session, &id, &target)?;
        Ok(AnnotationTarget {
            warning: status.warning,
            acknowledgment: status.acknowledgment,
            exists: std::fs::symlink_metadata(&target).is_ok(),
            path: target.to_string_lossy().into_owned(),
            is_tiff,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn load_annotations(
    id: String,
    backend: State<'_, Backend>,
) -> Result<Vec<roi::PointRoi>, String> {
    let job = annotation_job(&backend, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Image replaced or cancelled.".into());
        }
        let mut annotations = job.annotations.lock().unwrap();
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Image replaced or cancelled.".into());
        }
        if annotations.is_none() {
            let basis = annotation_io::load_basis(&job.source)?;
            if job.cancel.load(Ordering::Relaxed) {
                return Err("Image replaced or cancelled.".into());
            }
            *annotations = Some(basis);
        }
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Image replaced or cancelled.".into());
        }
        Ok(annotations.as_ref().unwrap().points.clone())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn staged_target_changed(selected: &Path, expected: &Path, staged: &Path) -> bool {
    expected != staged || !path_key(selected).is_ok_and(|current| current == staged)
}

#[tauri::command]
pub(crate) async fn save_annotations(
    id: String,
    path: String,
    rois: Vec<roi::PointRoi>,
    overwrite: bool,
    acknowledgment: Option<String>,
    backend: State<'_, Backend>,
    session: State<'_, Arc<Session>>,
) -> Result<SaveResult, String> {
    let job = annotation_job(&backend, &id)?;
    let session = session.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let target = Path::new(&path);
        let expected_target = {
            let _writing = session
                .instances
                .save_lock(target)
                .map_err(|e| e.to_string())?;
            let status = save_warning(&session, &id, target)?;
            if status.warning.is_some() && acknowledgment.as_ref() != Some(&status.acknowledgment) {
                return Ok(status);
            }
            path_key(target).map_err(|e| e.to_string())?
        };
        // Serialize this window's saves while copying outside the destination lock.
        let mut annotations = job.annotations.lock().unwrap();
        if annotations.is_none() {
            *annotations = Some(annotation_io::load_basis(&job.source)?);
        }
        let staged_source = fingerprint(&job.source).map_err(|e| e.to_string())?;
        let staged = match annotation_io::stage_save(
            &job.source,
            target,
            rois,
            annotations.as_ref().unwrap(),
            overwrite,
        ) {
            Ok(staged) => staged,
            Err(error) => {
                let _writing = session
                    .instances
                    .save_lock(target)
                    .map_err(|e| e.to_string())?;
                let status = save_warning(&session, &id, target)?;
                if status.warning.is_some()
                    && acknowledgment.as_ref() != Some(&status.acknowledgment)
                {
                    return Ok(status);
                }
                return Err(error);
            }
        };
        let staged_target = staged.target().to_path_buf();
        // Lock both paths in stable order so another Aperture cannot replace a
        // Save As source between the final fingerprint check and destination commit.
        let _writing = session
            .instances
            .save_locks(&[job.source.clone(), staged_target.clone()])
            .map_err(|e| e.to_string())?;
        let mut status = save_warning(&session, &id, &staged_target)?;
        if staged_target_changed(target, &expected_target, &staged_target) {
            status.warning = Some(
                "The destination path changed or became unavailable while saving. Review the destination and save again.".into(),
            );
            return Ok(status);
        }
        if fingerprint(&job.source).map_err(|e| e.to_string())? != staged_source {
            // A changed source requires a fresh copy after the next acknowledgment.
            status.warning = Some(
                "The source image changed while saving. Review the warning and save again.".into(),
            );
            return Ok(status);
        }
        if status.warning.is_some() && acknowledgment.as_ref() != Some(&status.acknowledgment) {
            return Ok(status);
        }
        staged.commit()?;
        Ok(finish_save(&session, &id, &staged_target, status))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{instances::Instances, tiff_annotations, LoadedImage};
    use std::{collections::HashMap, sync::Mutex};

    #[test]
    fn overwrite_warnings_follow_live_windows_and_disk_changes() {
        let root = std::env::temp_dir().join(format!(
            "aperture-save-warning-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("cells étude.tif");
        std::fs::write(&source, b"source").unwrap();
        let source = path_key(&source).unwrap();
        let coordinator = Arc::new(Instances::new(root.join("cache")).unwrap());
        coordinator.set_image(Some(&source)).unwrap();
        let session = Session {
            instances: coordinator,
            loaded: Mutex::new(Some(LoadedImage {
                id: "image".into(),
                source: source.clone(),
                fingerprints: HashMap::from([(source.clone(), fingerprint(&source).unwrap())]),
            })),
            opening: Mutex::new(()),
            startup: Mutex::new(None),
        };
        assert!(save_warning(&session, "image", &source)
            .unwrap()
            .warning
            .is_none());
        let other = Instances::new(root.join("cache")).unwrap();
        other.set_image(Some(&source)).unwrap();
        let live = save_warning(&session, "image", &source).unwrap();
        assert!(live.warning.unwrap().contains("another Aperture window"));
        drop(other);
        let before = save_warning(&session, "image", &source).unwrap();
        assert!(before.warning.is_none());
        std::fs::write(&source, b"changed source").unwrap();
        let changed = save_warning(&session, "image", &source).unwrap();
        assert!(changed.warning.unwrap().contains("changed on disk"));
        assert_ne!(before.acknowledgment, changed.acknowledgment);
        let destination = root.join("existing.tif");
        std::fs::write(&destination, b"existing").unwrap();
        assert!(save_warning(&session, "image", &destination)
            .unwrap()
            .warning
            .unwrap()
            .contains("destination already exists"));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_save_stays_successful_when_tracking_refresh_fails() {
        let root = std::env::temp_dir().join(format!(
            "aperture-committed-tracking-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.tif");
        let target = root.join("saved.tif");
        // A minimal classic TIFF directory, created only for this regression.
        std::fs::write(&source, b"II*\0\x08\0\0\0\0\0\0\0\0\0").unwrap();
        let source = path_key(&source).unwrap();
        let instances = Arc::new(Instances::new(root.join("cache")).unwrap());
        instances.set_image(Some(&source)).unwrap();
        let session = Session {
            instances,
            loaded: Mutex::new(Some(LoadedImage {
                id: "image".into(),
                source: source.clone(),
                fingerprints: HashMap::from([(source.clone(), fingerprint(&source).unwrap())]),
            })),
            opening: Mutex::new(()),
            startup: Mutex::new(None),
        };
        let before = save_warning(&session, "image", &target).unwrap();
        tiff_annotations::stage_copy(&source, &target, vec![7], vec![], false)
            .unwrap()
            .commit()
            .unwrap();
        let committed = std::fs::read(&target).unwrap();
        let images = root.join("cache/instance-locks-v1/images");
        session.instances.set_image(None).unwrap();
        std::fs::rename(&images, root.join("image-leases-backup")).unwrap();
        std::fs::write(&images, b"unusable lease directory").unwrap();
        let result = finish_save(&session, "image", &target, before);
        assert!(result.saved);
        assert!(result.warning.unwrap().starts_with("Saved, but"));
        assert_eq!(std::fs::read(&target).unwrap(), committed);
        assert_eq!(
            tiff_annotations::read(&target).unwrap().active_roi,
            Some(vec![7])
        );
        assert!(session
            .loaded
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprints
            .is_empty());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn staged_destination_rejects_removed_or_retargeted_aliases() {
        let root = std::env::temp_dir().join(format!(
            "aperture-staged-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.tif");
        let original = root.join("original.data");
        let replacement = root.join("replacement.data");
        let selected = root.join("selected.tif");
        let original_bytes = b"II*\0\x08\0\0\0\0\0\0\0\0\0original-pixels";
        std::fs::write(&source, original_bytes).unwrap();
        std::fs::write(&original, original_bytes).unwrap();
        std::fs::write(&replacement, b"replacement must stay unchanged").unwrap();
        std::os::unix::fs::symlink(&original, &selected).unwrap();
        let expected = path_key(&selected).unwrap();
        let staged =
            tiff_annotations::stage_copy(&source, &selected, vec![7], vec![], true).unwrap();
        assert_eq!(staged.target(), expected);
        assert!(!staged_target_changed(
            &selected,
            &expected,
            staged.target()
        ));
        std::fs::remove_file(&selected).unwrap();
        assert!(staged_target_changed(&selected, &expected, staged.target()));
        std::os::unix::fs::symlink(&replacement, &selected).unwrap();
        assert!(staged_target_changed(&selected, &expected, staged.target()));
        assert_eq!(staged.target(), expected);
        drop(staged);
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        assert_eq!(
            std::fs::read(&replacement).unwrap(),
            b"replacement must stay unchanged"
        );
        // Retargeting between preflight and staging also requires a fresh review,
        // even when the selected alias now agrees with the staged target.
        let staged =
            tiff_annotations::stage_copy(&source, &selected, vec![7], vec![], true).unwrap();
        assert!(staged_target_changed(&selected, &expected, staged.target()));
        let committed_target = staged.target().to_path_buf();
        // A stage commits exactly the captured referent, never a later alias target.
        std::fs::remove_file(&selected).unwrap();
        std::os::unix::fs::symlink(&original, &selected).unwrap();
        staged.commit().unwrap();
        assert_eq!(committed_target, path_key(&replacement).unwrap());
        assert_eq!(
            tiff_annotations::read(&replacement).unwrap().active_roi,
            Some(vec![7])
        );
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        // New relative destinations normalize to the same canonical parent key.
        let relative = root.join("unused").join("..").join("new.tif");
        std::fs::create_dir(root.join("unused")).unwrap();
        let staged =
            tiff_annotations::stage_copy(&source, &relative, vec![], vec![], false).unwrap();
        assert_eq!(staged.target(), path_key(&relative).unwrap());
        drop(staged);
        std::fs::remove_dir_all(root).unwrap();
    }
}
