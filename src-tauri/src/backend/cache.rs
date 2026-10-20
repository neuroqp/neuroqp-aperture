//! Persistent display caches: manifests, reader identity, validation and pruning.
use super::Job;
use crate::{
    channels::{self, Channel, Plane},
    native::{self, NativeLevel},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::UNIX_EPOCH,
};

pub(crate) const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) struct CacheManifest {
    pub(super) version: u32,
    pub(super) source_key: String,
    pub(super) tools: u64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) orientation: u8,
    pub(super) channels: Vec<Channel>,
    pub(super) channel_mode: bool,
    pub(super) planes: Vec<Plane>,
    pub(super) native: Vec<NativeLevel>,
    #[serde(default)]
    pub(super) dimensions: Option<crate::frames::Dimensions>,
    #[serde(default)]
    pub(super) calibration: Option<crate::frames::Calibration>,
    #[serde(default)]
    pub(super) display_ranges: Vec<[u16; 2]>,
    #[serde(default)]
    pub(super) projection: crate::frames::Projection,
}

// AppImage readers live under a different mount path after each launch. The
// containing package identifies that complete reader payload without scanning it.
#[cfg(target_os = "linux")]
fn appimage_tool_identity(executable: &Path, appdir: &Path, appimage: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    if !appdir.is_absolute()
        || !appdir.is_dir()
        || !executable.starts_with(appdir)
        || !appimage.is_absolute()
    {
        return None;
    }
    let image = fs::canonicalize(appimage).ok()?;
    let metadata = fs::metadata(&image).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    "aperture-appimage-readers-v1".hash(&mut hasher);
    image.hash(&mut hasher);
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
        .hash(&mut hasher);
    Some(hasher.finish())
}

// Metadata-only identity keeps a cache made with different decoder tools from
// assuming that the installed reader still supports its native TIFF layout.
fn tool_identity() -> u64 {
    let executable = std::env::current_exe().ok();
    #[cfg(target_os = "linux")]
    if let (Some(executable), Some(appdir), Some(appimage)) = (
        executable.as_deref(),
        std::env::var_os("APPDIR"),
        std::env::var_os("APPIMAGE"),
    ) {
        if let Some(identity) =
            appimage_tool_identity(executable, Path::new(&appdir), Path::new(&appimage))
        {
            return identity;
        }
    }
    let mut hasher = DefaultHasher::new();
    // The application contains the Rust BMP reader and native worker. Bundled
    // DLLs can also change while CLI filenames remain unchanged across updates.
    if let Some(executable) = executable {
        let mut readers = vec![executable.clone()];
        if cfg!(windows) {
            readers.extend(
                [
                    "libvips-42.dll",
                    "libtiff-6.dll",
                    "libglib-2.0-0.dll",
                    "libgobject-2.0-0.dll",
                ]
                .map(|name| executable.with_file_name(name)),
            );
        }
        for reader in readers {
            reader.hash(&mut hasher);
            if let Ok(metadata) = fs::metadata(reader) {
                metadata.len().hash(&mut hasher);
                metadata.modified().ok().hash(&mut hasher);
            }
        }
    }
    for program in ["vips", "vipsheader", "tiffinfo"] {
        program.hash(&mut hasher);
        let bundled = PathBuf::from(crate::platform::tool(program).get_program());
        #[cfg(windows)]
        let filename = format!("{program}.exe");
        #[cfg(not(windows))]
        let filename = program.to_owned();
        let search_path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(bundled)
            .chain(std::env::split_paths(&search_path).map(|directory| directory.join(&filename)));
        for path in paths {
            if let Ok(metadata) = fs::metadata(&path) {
                if metadata.is_file() {
                    path.hash(&mut hasher);
                    metadata.len().hash(&mut hasher);
                    metadata.modified().ok().hash(&mut hasher);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        metadata.ctime().hash(&mut hasher);
                        metadata.ctime_nsec().hash(&mut hasher);
                    }
                    break;
                }
            }
        }
    }
    hasher.finish()
}

pub(super) fn write_manifest(job: &Job, dir: &Path) -> Result<(), String> {
    let state = job.state.lock().unwrap().clone();
    let manifest = CacheManifest {
        version: 1,
        source_key: job.cache_key.clone(),
        tools: tool_identity(),
        width: state.width,
        height: state.height,
        orientation: state.orientation,
        channels: state.channels,
        channel_mode: state.channel_mode,
        planes: job.planes.lock().unwrap().clone(),
        native: job.native.lock().unwrap().clone(),
        dimensions: state.dimensions,
        calibration: state.calibration,
        display_ranges: state.display_ranges,
        projection: state.projection,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
    fs::write(dir.join("manifest.json"), bytes).map_err(|error| error.to_string())
}

pub(super) fn cached_manifest(job: &Job, dir: &Path) -> Option<CacheManifest> {
    let complete = dir.join("complete").is_file();
    if !complete && (job.selection.is_none() || !dir.join("previews").is_file()) {
        return None;
    }
    let file = File::open(dir.join("manifest.json")).ok()?;
    if file.metadata().ok()?.len() > 65536 {
        return None;
    }
    let manifest: CacheManifest = serde_json::from_reader(file.take(65536)).ok()?;
    if manifest.version != 1
        || manifest.source_key != job.cache_key
        || manifest.tools != tool_identity()
        || manifest.projection != job.projection
        || !(1..=i32::MAX as u32).contains(&manifest.width)
        || !(1..=i32::MAX as u32).contains(&manifest.height)
        || !(1..=8).contains(&manifest.orientation)
        || manifest.channels.is_empty()
        || manifest.channels.len() > channels::MAX_CHANNELS as usize
        || manifest.planes.len() != manifest.channels.len()
        || manifest
            .planes
            .iter()
            .any(|plane| plane.page >= 256 || plane.band >= channels::MAX_CHANNELS)
        || manifest.native.len() > 29
        || (job.selection.is_some() != manifest.dimensions.is_some())
        || manifest.dimensions.as_ref().is_some_and(|dimensions| {
            dimensions.positions.is_empty()
                || dimensions.position as usize >= dimensions.positions.len()
                || dimensions.z_index >= dimensions.z
                || dimensions.time_index >= dimensions.time
                || job.selection.is_none_or(|selected| {
                    selected.position != dimensions.position
                        || selected.z != dimensions.z_index
                        || selected.time != dimensions.time_index
                })
        })
        || (!manifest.native.is_empty()
            && !native::valid_cached_levels(&manifest.native, manifest.width, manifest.height))
        || (manifest.channel_mode
            && manifest.planes.iter().any(|plane| plane.page != 0)
            && !manifest.native.is_empty())
    {
        return None;
    }
    let valid = if complete {
        valid_cache_assets(
            dir,
            manifest.channels.len(),
            manifest.channel_mode,
            !manifest.native.is_empty(),
        )
    } else {
        manifest.channel_mode
            && manifest.native.is_empty()
            && (0..manifest.channels.len()).all(|i| {
                valid_preview(
                    &dir.join("channels").join(i.to_string()).join("preview.png"),
                    true,
                )
            })
    };
    if !valid {
        return None;
    }
    Some(manifest)
}

pub(super) fn valid_cache_assets(
    dir: &Path,
    channels: usize,
    channel_mode: bool,
    native: bool,
) -> bool {
    dir.join("complete").is_file()
        && if channel_mode {
            (0..channels).all(|index| {
                let channel = dir.join("channels").join(index.to_string());
                valid_preview(&channel.join("preview.png"), true)
                    && (native || channel.join("tiles_files").is_dir())
            })
        } else {
            valid_preview(&dir.join("preview.jpg"), false)
                && (native || dir.join("tiles_files").is_dir())
        }
}

pub(super) fn valid_preview(path: &Path, png: bool) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !(16..=16 * 1024 * 1024).contains(&metadata.len()) {
        return false;
    }
    let mut first = [0; 8];
    let mut last = [0; 12];
    if file.read_exact(&mut first).is_err()
        || file.seek(SeekFrom::End(-12)).is_err()
        || file.read_exact(&mut last).is_err()
    {
        return false;
    }
    if png {
        first == *b"\x89PNG\r\n\x1a\n" && last == *b"\0\0\0\0IEND\xaeB`\x82"
    } else {
        first.starts_with(&[0xff, 0xd8]) && last.ends_with(&[0xff, 0xd9])
    }
}

pub(super) fn housekeeping(job: &Job) -> Result<(), String> {
    let _access = job.cache_access.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let active = job.dir.lock().unwrap().clone();
    if !job.background {
        prune_except(&job.root, CACHE_LIMIT / 2, Some(&active))?;
    }
    job.housekeeping.store(true, Ordering::Relaxed);
    if tree_size(&job.root) > CACHE_LIMIT {
        return Err("Display cache limit reached.".into());
    }
    Ok(())
}

pub(crate) fn tree_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_symlink() => 0,
            Ok(kind) if kind.is_dir() => tree_size(&entry.path()),
            Ok(_) => entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
fn prune(root: &Path, limit: u64) -> Result<(), String> {
    prune_except(root, limit, None)
}

fn prune_except(root: &Path, limit: u64, protected: Option<&Path>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|entry| {
            protected != Some(entry.path().as_path())
                && (entry.path().join("complete").is_file()
                    || entry.path().join("previews").is_file())
                && !entry.file_name().to_string_lossy().starts_with("work-")
        })
        .collect();
    entries.sort_by_key(|entry| {
        fs::metadata(entry.path().join("access"))
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let mut size = tree_size(root);
    for entry in entries {
        if size <= limit {
            break;
        }
        let bytes = tree_size(&entry.path());
        fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
        size = size.saturating_sub(bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        process::budget_exceeded,
        process::uses_worker,
        tests::{channel_fixture, wait_ready},
        Backend, ImageState, NEXT_ID,
    };
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    #[cfg(windows)]
    #[test]
    fn windows_directory_promotion_waits_for_open_descendant_to_close() {
        let root = std::env::temp_dir().join(format!(
            "aperture-directory-promotion-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let tiles = root.join("tiles_files");
        fs::create_dir_all(&tiles).unwrap();
        let tile = tiles.join("tile.jpg");
        fs::write(&tile, b"tile").unwrap();
        let reader = File::open(&tile).unwrap();
        let published = root.join("published_tiles");
        assert_eq!(
            fs::rename(&tiles, &published).unwrap_err().raw_os_error(),
            Some(5)
        );
        drop(reader);
        fs::rename(&tiles, &published).unwrap();
        assert_eq!(fs::read(published.join("tile.jpg")).unwrap(), b"tile");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_reader_identity_survives_mount_changes_and_detects_package_changes() {
        let root = std::env::temp_dir().join(format!(
            "aperture-package-identity-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let image = root.join("Aperture.AppImage");
        fs::write(&image, b"aperture vips-v1").unwrap();
        let mounts = [root.join(".mount_first"), root.join(".mount_second")];
        let identity = |mount: &Path| {
            appimage_tool_identity(&mount.join("usr/bin/aperture"), mount, &image).unwrap()
        };
        for mount in &mounts {
            fs::create_dir_all(mount.join("usr/bin")).unwrap();
            fs::write(mount.join("usr/bin/aperture"), b"same application").unwrap();
            fs::write(mount.join("usr/bin/vips"), b"same bundled reader").unwrap();
        }
        let original = identity(&mounts[0]);
        assert_eq!(
            original,
            identity(&mounts[1]),
            "Mount locations and reader ctimes must not invalidate a cache"
        );
        assert!(appimage_tool_identity(&root.join("development/aperture"), &mounts[0], &image).is_none(), "Inherited AppImage environment must leave ordinary binaries on their normal reader identity");
        let metadata = fs::metadata(&image).unwrap();
        let modified = metadata.modified().unwrap();
        let replacement = root.join("replacement.AppImage");
        fs::write(&replacement, b"aperture vips-v2").unwrap();
        fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        fs::rename(replacement, &image).unwrap();
        assert_eq!(fs::metadata(&image).unwrap().len(), metadata.len());
        assert_eq!(fs::metadata(&image).unwrap().modified().unwrap(), modified);
        let changed = identity(&mounts[0]);
        assert_ne!(
            original, changed,
            "Changed bundled readers must invalidate old caches even with preserved size and mtime"
        );
        assert_eq!(changed, identity(&mounts[1]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_assets_require_only_the_previews_used_by_each_mode() {
        let root = std::env::temp_dir().join(format!(
            "aperture-cache-previews-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("tiles_files")).unwrap();
        fs::write(root.join("complete"), []).unwrap();
        let jpeg = [0xff, 0xd8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xd9];
        assert!(!valid_cache_assets(&root, 1, false, false));
        assert!(!valid_cache_assets(&root, 1, false, true));
        fs::write(root.join("preview.jpg"), jpeg).unwrap();
        assert!(valid_cache_assets(&root, 1, false, false));
        assert!(valid_cache_assets(&root, 1, false, true));
        fs::remove_file(root.join("preview.jpg")).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82";
        for channel in 0..2 {
            let dir = root.join("channels").join(channel.to_string());
            fs::create_dir_all(dir.join("tiles_files")).unwrap();
            fs::write(dir.join("preview.png"), png).unwrap();
        }
        assert!(valid_cache_assets(&root, 2, true, false));
        assert!(valid_cache_assets(&root, 2, true, true));
        // Older channel caches can retain an unused JPEG, even a corrupt one.
        fs::write(root.join("preview.jpg"), b"unused legacy preview").unwrap();
        assert!(valid_cache_assets(&root, 2, true, false));
        fs::remove_dir(root.join("channels/1/tiles_files")).unwrap();
        assert!(!valid_cache_assets(&root, 2, true, false));
        assert!(valid_cache_assets(&root, 2, true, true));
        fs::write(root.join("channels/1/preview.png"), b"broken preview").unwrap();
        assert!(!valid_cache_assets(&root, 2, true, true));
        fs::remove_file(root.join("channels/1/preview.png")).unwrap();
        assert!(!valid_cache_assets(&root, 2, true, true));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn early_cache_requires_valid_manifest_preview_and_source_fingerprint() {
        let root = std::env::temp_dir().join(format!(
            "aperture-early-cache-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.tif");
        channel_fixture(&source, 1, 1, "");
        let mut backend = Backend::new(root.join("cache")).unwrap();
        let events = Arc::new(Mutex::new(Vec::<ImageState>::new()));
        let recorded = events.clone();
        let callback_job = Arc::new(Mutex::new(None::<std::sync::Weak<Job>>));
        let target = callback_job.clone();
        backend.set_notifier(move |state| {
            if let Some(job) = target
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
            {
                let _snapshot = job.state.lock().unwrap().clone();
            }
            recorded.lock().unwrap().push(state.clone());
        });
        backend.open(&source).unwrap();
        let first = wait_ready(&backend);
        let dir = backend
            .current
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .dir
            .lock()
            .unwrap()
            .clone();
        let manifest = fs::read(dir.join("manifest.json")).unwrap();
        let preview_metadata = fs::metadata(dir.join("preview.jpg")).unwrap();
        fs::remove_file(dir.join("manifest.json")).unwrap();
        backend.open(&source).unwrap();
        assert!(wait_ready(&backend).detail.contains("Reused"));
        assert_eq!(
            fs::metadata(dir.join("preview.jpg"))
                .unwrap()
                .modified()
                .unwrap(),
            preview_metadata.modified().unwrap()
        );
        assert!(dir.join("manifest.json").is_file());
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(
            job.process_number.load(Ordering::Relaxed),
            if cfg!(windows) {
                0
            } else if uses_worker(&job) {
                1
            } else {
                2
            },
            "legacy cache migration starts only the CLI metadata readers (Windows omits tiffinfo)"
        );
        for corrupt in [b"{invalid".to_vec(), vec![b'x'; 65537]] {
            fs::write(dir.join("manifest.json"), corrupt).unwrap();
            backend.open(&source).unwrap();
            assert!(!wait_ready(&backend).detail.contains("Reused"));
        }
        fs::write(dir.join("manifest.json"), &manifest).unwrap();
        let reopened = backend.open(&source).unwrap();
        let ready = wait_ready(&backend);
        assert!(ready.detail.contains("Reused"));
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert_eq!(
            job.process_number.load(Ordering::Relaxed),
            0,
            "cache hit must not spawn readers"
        );
        assert!(
            job.decoder.lock().unwrap().is_none(),
            "cache hit must not start the native worker"
        );
        // Reenter this same job's state mutex from its callback.
        *callback_job.lock().unwrap() = Some(Arc::downgrade(&job));
        job.update(|state| state.detail = "Reused cache notification check".into());
        fs::write(dir.join("preview.jpg"), b"broken preview").unwrap();
        backend.open(&source).unwrap();
        assert!(!wait_ready(&backend).detail.contains("Reused"));
        // Same path with a changed source cannot reuse its previous descriptor.
        channel_fixture(&source, 1, 1, "changed metadata");
        backend.open(&source).unwrap();
        assert!(!wait_ready(&backend).detail.contains("Reused"));
        backend.cancel();
        let events = events.lock().unwrap();
        let initial_events: Vec<_> = events.iter().filter(|state| state.id == first.id).collect();
        assert!(initial_events
            .windows(2)
            .all(|pair| pair[0].revision < pair[1].revision));
        assert!(
            initial_events
                .iter()
                .position(|state| state.preview_ready)
                .unwrap()
                < initial_events
                    .iter()
                    .position(|state| state.phase == "ready")
                    .unwrap()
        );
        assert!(events
            .iter()
            .any(|state| state.id == reopened.id && state.phase == "ready"));
        assert_eq!(events.last().unwrap().phase, "cancelled");
        drop(events);
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deferred_housekeeping_pins_live_cache_and_preview_work_remains_bounded() {
        let root = std::env::temp_dir().join(format!(
            "aperture-budget-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let active = root.join("active");
        let stale = root.join("stale");
        for dir in [&active, &stale] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("complete"), b"ok").unwrap();
            fs::write(dir.join("preview.jpg"), b"preview").unwrap();
        }
        prune_except(&root, 0, Some(&active)).unwrap();
        assert!(active.join("preview.jpg").is_file());
        assert!(!stale.exists());
        let backend = Backend::new(root.join("cache")).unwrap();
        let source = root.join("source.tif");
        channel_fixture(&source, 1, 1, "");
        backend.open(&source).unwrap();
        wait_ready(&backend);
        let job = backend.current.lock().unwrap().clone().unwrap();
        // A replaced worker waiting on cache housekeeping must recheck cancel
        // while holding the same guard used by the next warm-cache selection.
        let guard = backend.cache_access.lock().unwrap();
        let replaced = job.clone();
        let old_housekeeping = thread::spawn(move || housekeeping(&replaced));
        backend.open(&source).unwrap();
        assert_eq!(backend.snapshot().unwrap().phase, "loading");
        drop(guard);
        assert!(old_housekeeping
            .join()
            .unwrap()
            .unwrap_err()
            .contains("cancelled"));
        assert!(wait_ready(&backend).detail.contains("Reused"));
        let oversized = File::create(job.work.join("oversized")).unwrap();
        oversized.set_len(CACHE_LIMIT / 2 + 1).unwrap();
        job.housekeeping.store(false, Ordering::Relaxed);
        assert!(budget_exceeded(&job, true));
        fs::remove_file(job.work.join("oversized")).unwrap();
        let oversized = File::create(job.dir.lock().unwrap().join("oversized")).unwrap();
        oversized.set_len(CACHE_LIMIT + 1).unwrap();
        job.housekeeping.store(true, Ordering::Relaxed);
        assert!(budget_exceeded(&job, true));
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_pruning_never_follows_source_alias() {
        let root = std::env::temp_dir().join(format!(
            "aperture-prune-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("entry")).unwrap();
        fs::write(root.join("entry/complete"), b"ok").unwrap();
        fs::write(root.join("entry/pixels"), vec![0; 1024]).unwrap();
        prune(&root, 0).unwrap();
        assert!(!root.join("entry").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
