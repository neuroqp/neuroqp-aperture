//! Cross-process coordination. Lock files stay outside caches and are never
//! removed: unlinking a locked file would let another process lock a new inode.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

pub struct Instances {
    coordination: PathBuf,
    cache_root: PathBuf,
    slot: u64,
    running: Mutex<File>,
    images: Mutex<BTreeMap<PathBuf, File>>,
    _slot_lock: File,
}

impl Instances {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        let coordination = root.join("instance-locks-v1");
        fs::create_dir_all(&coordination)?;
        // Taking both shared locks in this order closes the startup/update race.
        let gate = lock_file(&coordination.join("update.lock"))?;
        gate.lock_shared()?;
        let running = lock_file(&coordination.join("running.lock"))?;
        running.lock_shared()?;
        let (slot, slot_lock) = (0..)
            .find_map(|slot| {
                let lock = match lock_file(&coordination.join(format!("slot-{slot}.lock"))) {
                    Ok(lock) => lock,
                    Err(error) => return Some(Err(error)),
                };
                match try_exclusive(&lock) {
                    Ok(true) => Some(Ok((slot, lock))),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .unwrap()?;
        let cache_root = root.join("display-v3").join(format!("slot-{slot}"));
        fs::create_dir_all(&cache_root)?;
        Ok(Self {
            coordination,
            cache_root,
            slot,
            _slot_lock: slot_lock,
            running: Mutex::new(running),
            images: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn set_image(&self, path: Option<&Path>) -> io::Result<bool> {
        self.set_images(&path.into_iter().map(Path::to_path_buf).collect::<Vec<_>>())?;
        path.map(|path| self.others_open(path)).unwrap_or(Ok(false))
    }

    /// Hold independent leases for the loaded source and its latest Save As path.
    /// These only report presence; they never prohibit editing or saving.
    pub fn set_images(&self, paths: &[PathBuf]) -> io::Result<()> {
        let paths = paths
            .iter()
            .map(|path| self.image_directory(path))
            .collect::<io::Result<Vec<_>>>()?;
        let mut images = self.images.lock().unwrap();
        let mut added = BTreeMap::new();
        for path in &paths {
            if images.contains_key(path) || added.contains_key(path) {
                continue;
            }
            fs::create_dir_all(path)?;
            let lock = lock_file(&path.join(format!("slot-{}.lock", self.slot)))?;
            lock.lock()?;
            added.insert(path.clone(), lock);
        }
        // Keep existing leases if acquiring a new lease fails.
        images.extend(added);
        images.retain(|path, _| paths.contains(path));
        Ok(())
    }

    pub fn others_open(&self, path: &Path) -> io::Result<bool> {
        let directory = self.image_directory(path)?;
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let own_name = format!("slot-{}.lock", self.slot);
        for entry in entries {
            let entry = entry?;
            if entry.file_name() == own_name.as_str() {
                continue;
            }
            let lock = lock_file(&entry.path())?;
            if !try_exclusive(&lock)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Serialize writes to the destination. Hold through the final changed-file
    /// check and atomic replacement; image-presence leases do not affect this.
    pub fn save_lock(&self, path: &Path) -> io::Result<SaveGuard> {
        Ok(self.save_locks(&[path.to_path_buf()])?.pop().unwrap())
    }

    /// Sort and deduplicate coordination keys, rather than I/O paths, to avoid
    /// locking an alias twice or taking source/destination locks in reverse order.
    pub fn save_locks(&self, paths: &[PathBuf]) -> io::Result<Vec<SaveGuard>> {
        let directory = self.coordination.join("saves");
        fs::create_dir_all(&directory)?;
        let paths = paths
            .iter()
            .map(|path| {
                coordination_key(path)
                    .map(|key| directory.join(format!("{}.lock", path_hash(&key))))
            })
            .collect::<io::Result<std::collections::BTreeSet<_>>>()?;
        paths
            .into_iter()
            .map(|path| {
                let lock = lock_file(&path)?;
                lock.lock()?;
                Ok(SaveGuard { _lock: lock })
            })
            .collect()
    }

    pub fn begin_update(&self) -> io::Result<UpdateGuard<'_>> {
        let gate = lock_file(&self.coordination.join("update.lock"))?;
        if !try_exclusive(&gate)? {
            return Err(io::Error::other(
                "Another Aperture instance is starting or installing an update. Try again shortly.",
            ));
        }
        let running = self.running.lock().unwrap();
        running.unlock()?;
        match try_exclusive(&running) {
            Ok(true) => Ok(UpdateGuard {
                running,
                _gate: gate,
            }),
            result => {
                // Restore our lifetime lock before allowing another startup.
                running.lock_shared()?;
                match result {
                    Ok(false) => Err(io::Error::other("Close the other Aperture windows before installing this update. Unsaved annotations in those windows must be saved or discarded there.")),
                    Err(error) => Err(error),
                    Ok(true) => unreachable!(),
                }
            }
        }
    }

    fn image_directory(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(self
            .coordination
            .join("images")
            .join(path_hash(&coordination_key(path)?)))
    }
}

pub struct SaveGuard {
    _lock: File,
}

pub struct UpdateGuard<'a> {
    running: MutexGuard<'a, File>,
    _gate: File,
}

impl Drop for UpdateGuard<'_> {
    fn drop(&mut self) {
        // Startup remains gated while converting back to a shared lifetime lock.
        if let Err(error) = self
            .running
            .unlock()
            .and_then(|()| self.running.lock_shared())
        {
            eprintln!("Could not restore Aperture's running lock: {error}");
        }
    }
}

fn lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn try_exclusive(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Normalize aliases, including destinations that do not exist yet.
pub fn path_key(path: &Path) -> io::Result<PathBuf> {
    let normalized = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::canonicalize(parent)?.join(
                path.file_name()
                    .ok_or_else(|| io::Error::other("Image path has no filename."))?,
            )
        }
        Err(error) => return Err(error),
    };
    Ok(normalized)
}

fn coordination_key(path: &Path) -> io::Result<PathBuf> {
    let path = path_key(path)?;
    #[cfg(windows)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Image path has no parent."))?;
        if !case_sensitive_directory(parent) {
            // Fold only coordination keys. Unicode case conversion can expand or
            // change filesystem names, and must never feed image I/O paths.
            let filename = path
                .file_name()
                .ok_or_else(|| io::Error::other("Image path has no filename."))?;
            return Ok(parent.join(filename.to_string_lossy().to_uppercase()));
        }
    }
    Ok(path)
}

#[cfg(windows)]
fn case_sensitive_directory(path: &Path) -> bool {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileCaseSensitiveInfo, GetFileInformationByHandleEx, FILE_CASE_SENSITIVE_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    };
    // ponytail: unknown directory flags can merge unrelated coordination keys.
    // This only adds warnings or serialization; actual I/O paths stay distinct.
    let directory = match OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
    {
        Ok(directory) => directory,
        Err(_) => return false,
    };
    let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
    let result = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&mut info as *mut FILE_CASE_SENSITIVE_INFO).cast(),
            std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    };
    result != 0 && info.Flags & 1 != 0
}

fn path_hash(path: &Path) -> String {
    // Stable across builds. A collision can only add a warning or serialize
    // unrelated writes; it cannot grant access or overwrite another file.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fingerprint {
    length: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(windows)]
    identity: Option<(u64, [u8; 16], i64)>,
}

/// An advisory changed-on-disk check, without reading or changing image bytes.
pub fn fingerprint(path: &Path) -> io::Result<Option<Fingerprint>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Some(Fingerprint {
        length: metadata.len(),
        modified: metadata.modified()?,
        created: metadata.created().ok(),
        #[cfg(unix)]
        identity: (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
        #[cfg(windows)]
        identity: crate::platform::file_identity(&file),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("aperture-instances-{}-{stamp}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Actor {
        child: Child,
        ready: PathBuf,
    }
    impl Actor {
        fn start(root: &Path, mode: &str) -> Self {
            let ready = root.join(format!("ready-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
            let test = format!(
                "{}::process_actor",
                module_path!().split_once("::").unwrap().1
            );
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--nocapture"])
                .env("APERTURE_TEST_LOCK_ROOT", root)
                .env("APERTURE_TEST_LOCK_READY", &ready)
                .env("APERTURE_TEST_LOCK_MODE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            Self { child, ready }
        }

        fn wait_ready(&mut self) -> PathBuf {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.ready.exists() {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "lock actor exited before readiness"
                );
                assert!(Instant::now() < deadline, "lock actor did not become ready");
                thread::sleep(Duration::from_millis(10));
            }
            PathBuf::from(fs::read_to_string(&self.ready).unwrap())
        }
    }
    impl Drop for Actor {
        fn drop(&mut self) {
            // Abrupt termination exercises kernel lock recovery, not graceful cleanup.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn process_actor() {
        let Some(root) = std::env::var_os("APERTURE_TEST_LOCK_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let instance = Instances::new(root.clone()).unwrap();
        let mode = std::env::var("APERTURE_TEST_LOCK_MODE").unwrap();
        let _save = if mode == "save" {
            Some(instance.save_lock(&root.join("image.tif")).unwrap())
        } else {
            instance.set_image(Some(&root.join("image.tif"))).unwrap();
            None
        };
        fs::write(instance.cache_root().join("cache-marker"), "keep").unwrap();
        fs::write(
            std::env::var_os("APERTURE_TEST_LOCK_READY").unwrap(),
            instance.cache_root().to_str().unwrap(),
        )
        .unwrap();
        // The parent owns stdin and either closes it or kills this process.
        let _ = std::io::stdin().read(&mut [0]);
    }

    #[test]
    fn separate_processes_keep_caches_presence_writes_and_updates_independent() {
        let root = Directory::new();
        let image = root.0.join("image.tif");
        let target = root.0.join("saved.tif");
        fs::write(&image, "synthetic image").unwrap();
        let instance = Instances::new(root.0.clone()).unwrap();
        fs::write(instance.cache_root().join("own-marker"), "own").unwrap();
        instance
            .set_images(&[image.clone(), target.clone()])
            .unwrap();

        let mut first = Actor::start(&root.0, "image");
        let first_cache = first.wait_ready();
        let mut second = Actor::start(&root.0, "image");
        let second_cache = second.wait_ready();
        assert_ne!(instance.cache_root(), first_cache);
        assert_ne!(first_cache, second_cache);
        assert!(instance.others_open(&image).unwrap());
        assert!(!instance.others_open(&target).unwrap());
        assert!(instance.begin_update().is_err());
        // Repeated failures must restore this instance's lifetime shared lock.
        assert!(instance.begin_update().is_err());
        drop(first);
        assert!(
            instance.others_open(&image).unwrap(),
            "remaining image owner was lost"
        );
        let reused = Instances::new(root.0.clone()).unwrap();
        assert_eq!(reused.cache_root(), first_cache);
        assert_eq!(
            fs::read_to_string(reused.cache_root().join("cache-marker")).unwrap(),
            "keep"
        );
        fs::remove_dir_all(reused.cache_root()).unwrap();
        assert_eq!(
            fs::read_to_string(instance.cache_root().join("own-marker")).unwrap(),
            "own"
        );
        assert!(second_cache.join("cache-marker").exists());
        assert!(
            instance.begin_update().is_err(),
            "cache cleanup removed a live slot lock"
        );
        drop(reused);
        drop(second);
        assert!(!instance.others_open(&image).unwrap());

        let update = instance.begin_update().unwrap();
        let mut startup = Actor::start(&root.0, "image");
        thread::sleep(Duration::from_millis(150));
        assert!(!startup.ready.exists(), "startup escaped the update gate");
        assert!(startup.child.try_wait().unwrap().is_none());
        drop(update);
        startup.wait_ready();
        assert!(instance.begin_update().is_err());
        drop(startup);
        drop(instance.begin_update().unwrap());

        let mut writer = Actor::start(&root.0, "save");
        writer.wait_ready();
        thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let image = &image;
            let instance = &instance;
            let waiting = scope.spawn(move || {
                let _guard = instance.save_lock(image).unwrap();
                send.send(()).unwrap();
            });
            assert!(
                receive.recv_timeout(Duration::from_millis(150)).is_err(),
                "concurrent destination write was admitted"
            );
            drop(writer);
            receive.recv_timeout(Duration::from_secs(10)).unwrap();
            waiting.join().unwrap();
        });
        drop(instance);
    }

    #[test]
    fn aliases_and_changed_destination_fingerprints() {
        let root = Directory::new();
        let image = root.0.join("image.tif");
        assert_eq!(fingerprint(&image).unwrap(), None);
        assert_eq!(
            path_key(&image).unwrap(),
            path_key(&root.0.join(".").join("image.tif")).unwrap()
        );
        fs::write(&image, "original").unwrap();
        let original = fingerprint(&image).unwrap();
        fs::write(&image, "changed length").unwrap();
        assert_ne!(original, fingerprint(&image).unwrap());
        #[cfg(unix)]
        {
            let alias = root.0.join("alias.tif");
            std::os::unix::fs::symlink(&image, &alias).unwrap();
            assert_eq!(path_key(&alias).unwrap(), path_key(&image).unwrap());
            let instance = Instances::new(root.0.clone()).unwrap();
            instance.set_images(&[image, alias]).unwrap();
            assert_eq!(instance.images.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn unicode_io_paths_are_preserved_and_save_keys_are_deduplicated() {
        let root = Directory::new();
        let image = root.0.join("İ_cells.tif");
        fs::write(&image, "original").unwrap();
        let actual = path_key(&image).unwrap();
        assert_eq!(actual, fs::canonicalize(&image).unwrap());
        assert_eq!(actual.file_name().unwrap(), "İ_cells.tif");
        assert_eq!(fingerprint(&actual).unwrap(), fingerprint(&image).unwrap());
        let before = fingerprint(&actual).unwrap();
        fs::write(&image, "changed length").unwrap();
        assert_ne!(before, fingerprint(&actual).unwrap());
        let instance = Instances::new(root.0.join("cache")).unwrap();
        assert_eq!(
            instance.save_locks(&[actual.clone(), image]).unwrap().len(),
            1
        );
        #[cfg(windows)]
        if !case_sensitive_directory(&root.0) {
            let upper = root.0.join("MISSING.tif");
            let lower = root.0.join("missing.tif");
            assert_eq!(
                coordination_key(&upper).unwrap(),
                coordination_key(&lower).unwrap()
            );
            assert_ne!(path_key(&upper).unwrap(), path_key(&lower).unwrap());
            assert_eq!(instance.save_locks(&[upper, lower]).unwrap().len(), 1);
            let sigma = root.0.join("σ.tif");
            let final_sigma = root.0.join("ς.tif");
            assert_eq!(
                coordination_key(&sigma).unwrap(),
                coordination_key(&final_sigma).unwrap()
            );
            assert_eq!(instance.save_locks(&[sigma, final_sigma]).unwrap().len(), 1);
        }
    }

    #[cfg(windows)]
    #[test]
    fn case_sensitive_windows_directories_keep_distinct_paths_and_locks() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FileCaseSensitiveInfo, SetFileInformationByHandle, FILE_CASE_SENSITIVE_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
        };
        let root = Directory::new();
        let directory = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&root.0)
            .unwrap();
        let info = FILE_CASE_SENSITIVE_INFO { Flags: 1 };
        let enabled = unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle(),
                FileCaseSensitiveInfo,
                (&info as *const FILE_CASE_SENSITIVE_INFO).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        if enabled == 0 {
            let error = io::Error::last_os_error();
            drop(directory);
            eprintln!("Per-directory case-sensitive regression unavailable on this Windows filesystem: {error}");
            return;
        }
        drop(directory);
        assert!(case_sensitive_directory(&root.0));
        let upper = root.0.join("A.tif");
        let lower = root.0.join("a.tif");
        assert_ne!(
            coordination_key(&upper).unwrap(),
            coordination_key(&lower).unwrap()
        );
        fs::write(&upper, "uppercase").unwrap();
        fs::write(&lower, "lowercase longer").unwrap();
        assert_ne!(path_key(&upper).unwrap(), path_key(&lower).unwrap());
        assert_ne!(fingerprint(&upper).unwrap(), fingerprint(&lower).unwrap());
        let instance = Instances::new(root.0.join("cache")).unwrap();
        assert_eq!(instance.save_locks(&[upper, lower]).unwrap().len(), 2);
    }
}
