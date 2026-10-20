//! Shared admission limit for explicitly accounted image allocations, not process RSS.
//! Keep each reservation alive for the entire allocation/cache lifetime. Decoder and
//! native-library workspaces need their own conservative reservations. Lock files
//! are never unlinked: kernel-owned leases survive manager drops and release on crash.
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_LIMIT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MIN_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_LIMIT_BYTES: u64 = 256 * 1024 * 1024 * 1024;
/// How long foreground admission waits for other processes to reclaim caches.
const RECLAIM_WAIT: Duration = Duration::from_secs(2);
const RECLAIM_POLL: Duration = Duration::from_millis(25);

pub struct Budget {
    directory: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatus {
    pub limit_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub pressure: bool,
    pub preloading_paused: bool,
}

pub struct Reservation {
    budget: Arc<Budget>,
    slot: u64,
    bytes: u64,
    _lease: File,
}

struct Snapshot {
    status: MemoryStatus,
    free: Option<(u64, File)>,
    next: u64,
    /// Available physical memory, when the platform reports it.
    physical: Option<u64>,
}

impl Budget {
    /// `root` must be the global application cache root, not an instance's cache.
    pub fn new(root: PathBuf) -> Result<Arc<Self>, String> {
        let budget = Arc::new(Self {
            directory: root.join("image-memory-v1"),
        });
        fs::create_dir_all(&budget.directory).map_err(message)?;
        let _gate = budget.gate().map_err(message)?;
        let settings = budget.directory.join("limit");
        if !settings.exists() {
            write_amount(&settings, DEFAULT_LIMIT_BYTES).map_err(message)?;
            open_lock(&settings)
                .and_then(|file| file.sync_all())
                .map_err(message)?;
        }
        budget.limit().map_err(message)?;
        Ok(budget)
    }

    pub fn status(&self) -> Result<MemoryStatus, String> {
        let _gate = self.gate().map_err(message)?;
        self.snapshot()
            .map(|snapshot| snapshot.status)
            .map_err(message)
    }

    /// Ask all application processes to drop optional image caches. Maintenance
    /// compares epochs and also evicts on `status().pressure`, even when idle.
    /// A foreground caller requests once, then retries admission for a bounded time.
    pub fn request_reclaim(&self) -> Result<(), String> {
        let _gate = self.gate().map_err(message)?;
        let epoch = self
            .read_reclaim_epoch()
            .map_err(message)?
            .checked_add(1)
            .ok_or_else(|| "Image memory reclaim epoch overflow.".to_string())?;
        write_amount(&self.directory.join("reclaim"), epoch).map_err(message)
    }

    pub fn reclaim_epoch(&self) -> Result<u64, String> {
        let _gate = self.gate().map_err(message)?;
        self.read_reclaim_epoch().map_err(message)
    }

    fn read_reclaim_epoch(&self) -> io::Result<u64> {
        match read_amount(&self.directory.join("reclaim")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            result => result,
        }
    }

    /// Existing allocations remain valid. Callers evict optional caches when
    /// used_bytes exceeds the new limit; no further admission can exceed it.
    pub fn set_limit(&self, bytes: u64) -> Result<MemoryStatus, String> {
        if !(MIN_LIMIT_BYTES..=MAX_LIMIT_BYTES).contains(&bytes) {
            return Err("Image memory limit must be between 256 MiB and 256 GiB.".into());
        }
        let _gate = self.gate().map_err(message)?;
        let settings = self.directory.join("limit");
        write_amount(&settings, bytes).map_err(message)?;
        open_lock(&settings)
            .and_then(|file| file.sync_all())
            .map_err(message)?;
        self.snapshot()
            .map(|snapshot| snapshot.status)
            .map_err(message)
    }

    /// None means capacity or system pressure currently prevents admission.
    /// Foreground work may proceed during pressure within the configured limit.
    pub fn try_reserve(
        self: &Arc<Self>,
        bytes: u64,
        background: bool,
    ) -> Result<Option<Reservation>, String> {
        let _gate = self.gate().map_err(message)?;
        let Snapshot {
            status,
            free,
            next,
            physical,
        } = self.snapshot().map_err(message)?;
        if status.used_bytes > status.limit_bytes
            || bytes > status.available_bytes
            || (background && status.preloading_paused)
            || physical.is_some_and(|available| !physical_headroom_allows(bytes, available))
        {
            return Ok(None);
        }
        let (slot, lease) = match free {
            Some(free) => free,
            None => {
                let lease = open_lock(&self.directory.join(format!("lease-{next}.lock")))
                    .map_err(message)?;
                lease.lock().map_err(message)?;
                (next, lease)
            }
        };
        write_amount(&self.amount_path(slot), bytes).map_err(message)?;
        Ok(Some(Reservation {
            budget: Arc::clone(self),
            slot,
            bytes,
            _lease: lease,
        }))
    }

    /// Like `try_reserve`, but foreground admission that fits the limit asks
    /// other processes to reclaim optional caches and retries for `RECLAIM_WAIT`.
    /// Setting `cancel` stops waiting early.
    pub fn reserve(
        self: &Arc<Self>,
        bytes: u64,
        background: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<Option<Reservation>, String> {
        if let Some(reservation) = self.try_reserve(bytes, background)? {
            return Ok(Some(reservation));
        }
        if background || bytes > self.status()?.limit_bytes {
            return Ok(None);
        }
        self.request_reclaim()?;
        let deadline = Instant::now() + RECLAIM_WAIT;
        while Instant::now() < deadline && !cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            thread::sleep(RECLAIM_POLL);
            if let Some(reservation) = self.try_reserve(bytes, false)? {
                return Ok(Some(reservation));
            }
        }
        Ok(None)
    }

    /// Foreground `reserve` that reports `error` when admission fails.
    pub fn require(self: &Arc<Self>, bytes: u64, error: &str) -> Result<Reservation, String> {
        self.reserve(bytes, false, None)?
            .ok_or_else(|| error.into())
    }

    fn gate(&self) -> io::Result<File> {
        let gate = open_lock(&self.directory.join("gate.lock"))?;
        gate.lock()?;
        Ok(gate)
    }

    fn limit(&self) -> io::Result<u64> {
        let limit = read_amount(&self.directory.join("limit"))?;
        if !(MIN_LIMIT_BYTES..=MAX_LIMIT_BYTES).contains(&limit) {
            return Err(io::Error::other("Invalid stored image memory limit."));
        }
        Ok(limit)
    }

    fn amount_path(&self, slot: u64) -> PathBuf {
        self.directory.join(format!("lease-{slot}.bytes"))
    }

    // ponytail: scan reusable lease slots under one gate; replace with a shared
    // ledger only if measured admission overhead requires it. Unlocked slots are
    // stale regardless of PID reuse, including after an abrupt process exit.
    fn snapshot(&self) -> io::Result<Snapshot> {
        let mut used: u64 = 0;
        let mut free = None;
        let mut next = 0;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(slot) = name
                .to_str()
                .and_then(|name| name.strip_prefix("lease-"))
                .and_then(|name| name.strip_suffix(".lock"))
                .and_then(|slot| slot.parse::<u64>().ok())
            else {
                continue;
            };
            next = next.max(
                slot.checked_add(1)
                    .ok_or_else(|| io::Error::other("Too many memory leases."))?,
            );
            let lease = open_lock(&entry.path())?;
            match lease.try_lock() {
                Ok(()) => {
                    if free.is_none() {
                        free = Some((slot, lease));
                    }
                }
                Err(TryLockError::WouldBlock) => {
                    used = used
                        .checked_add(read_amount(&self.amount_path(slot))?)
                        .ok_or_else(|| io::Error::other("Image memory accounting overflow."))?;
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        let limit = self.limit()?;
        let pressure_path = self.directory.join("pressure");
        let was_paused = match fs::read(&pressure_path) {
            Ok(bytes) => bytes == b"1",
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let sample = pressure_sample();
        let pressure = sample
            .map(|(total, available)| pressure_paused(was_paused, total, available))
            .unwrap_or(was_paused);
        if pressure != was_paused {
            fs::write(pressure_path, if pressure { b"1" } else { b"0" })?;
        }
        Ok(Snapshot {
            status: MemoryStatus {
                limit_bytes: limit,
                used_bytes: used,
                available_bytes: limit.saturating_sub(used),
                pressure,
                preloading_paused: pressure || used >= limit,
            },
            free,
            next,
            physical: sample.map(|(_, available)| available),
        })
    }
}

impl Reservation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Reduce a decode peak to the retained buffer without releasing admission
    /// between the two. Free the corresponding allocation before shrinking.
    pub fn shrink(&mut self, bytes: u64) -> Result<(), String> {
        if bytes > self.bytes {
            return Err("A memory reservation can only shrink.".into());
        }
        let _gate = self.budget.gate().map_err(message)?;
        write_amount(&self.budget.amount_path(self.slot), bytes).map_err(message)?;
        self.bytes = bytes;
        Ok(())
    }
}

fn message(error: io::Error) -> String {
    format!("Image memory budget: {error}")
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

// Store duplicate fixed-width values and reject interrupted/corrupt updates.
// Lease records are sidecars because Windows exclusive locks prohibit reading
// their locked range through another handle.
fn write_amount(path: &Path, bytes: u64) -> io::Result<()> {
    let mut file = open_lock(path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes.to_le_bytes())?;
    file.write_all(&bytes.to_le_bytes())?;
    file.set_len(16)
}

fn read_amount(path: &Path) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut record = [0; 16];
    file.read_exact(&mut record)?;
    if record[..8] != record[8..] || file.metadata()?.len() != 16 {
        return Err(io::Error::other("Corrupt image memory record."));
    }
    Ok(u64::from_le_bytes(record[..8].try_into().unwrap()))
}

fn pressure_paused(was_paused: bool, total: u64, available: u64) -> bool {
    // Resume only after twice the free-memory margin, avoiding rapid toggles.
    let pause_at = (total / 20).max(MIN_LIMIT_BYTES).min(total / 4);
    let resume_at = (total / 10).max(2 * MIN_LIMIT_BYTES).min(total / 2);
    available < if was_paused { resume_at } else { pause_at }
}

fn physical_headroom_allows(bytes: u64, available: u64) -> bool {
    // During pressure, leave a small margin for the window and other processes.
    bytes <= available.saturating_sub(64 * 1024 * 1024)
}

#[cfg(target_os = "linux")]
fn pressure_sample() -> Option<(u64, u64)> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let kib = |key: &str| -> Option<u64> {
        info.lines().find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        })
    };
    Some(cgroup_pressure(
        kib("MemTotal:")?,
        kib("MemAvailable:")?,
        Path::new("/sys/fs/cgroup"),
        &fs::read_to_string("/proc/self/cgroup").unwrap_or_default(),
    ))
}

#[cfg(target_os = "linux")]
fn cgroup_pressure(
    mut total: u64,
    mut available: u64,
    root: &Path,
    membership: &str,
) -> (u64, u64) {
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .unwrap_or("");
    let path = if Path::new(relative)
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        root.join(relative)
    } else {
        root.to_path_buf()
    };
    // Every ancestor can impose the tightest limit, including a namespace root.
    for directory in path.ancestors().take_while(|path| path.starts_with(root)) {
        let number = |name| {
            fs::read_to_string(directory.join(name))
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        if let (Some(max), Some(current)) = (number("memory.max"), number("memory.current")) {
            total = total.min(max);
            available = available.min(max.saturating_sub(current));
        }
    }
    (total, available)
}

#[cfg(windows)]
fn pressure_sample() -> Option<(u64, u64)> {
    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatusEx {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    // SAFETY: the initialized structure has the exact documented C layout/size.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        None
    } else {
        Some((status.total_physical, status.available_physical))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn pressure_sample() -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant, SystemTime},
    };

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                    "aperture-memory-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn shared_limit_shrink_and_lowering_preserve_accounting() {
        let root = Directory::new();
        let first = Budget::new(root.0.clone()).unwrap();
        let second = Budget::new(root.0.clone()).unwrap();
        assert_eq!(first.status().unwrap().limit_bytes, DEFAULT_LIMIT_BYTES);
        first.set_limit(2 * MIN_LIMIT_BYTES).unwrap();
        assert_eq!(second.status().unwrap().limit_bytes, 2 * MIN_LIMIT_BYTES);
        let mut allocation = first
            .try_reserve(2 * MIN_LIMIT_BYTES, false)
            .unwrap()
            .unwrap();
        assert!(second.try_reserve(1, false).unwrap().is_none());
        second.set_limit(MIN_LIMIT_BYTES).unwrap();
        assert_eq!(second.status().unwrap().used_bytes, 2 * MIN_LIMIT_BYTES);
        assert!(second.try_reserve(1, false).unwrap().is_none());
        allocation.shrink(MIN_LIMIT_BYTES / 2).unwrap();
        assert_eq!(second.status().unwrap().used_bytes, MIN_LIMIT_BYTES / 2);
        let another = second
            .try_reserve(MIN_LIMIT_BYTES / 2, false)
            .unwrap()
            .unwrap();
        assert!(allocation.shrink(MIN_LIMIT_BYTES).is_err());
        drop(allocation);
        drop(another);
        assert_eq!(second.status().unwrap().used_bytes, 0);
        assert!(second.set_limit(1).is_err());
        assert_eq!(
            Budget::new(root.0.clone())
                .unwrap()
                .status()
                .unwrap()
                .limit_bytes,
            MIN_LIMIT_BYTES
        );
    }

    #[test]
    fn pressure_can_resume_inside_a_small_cgroup() {
        let total = 512 * 1024 * 1024;
        assert!(pressure_paused(false, total, 100 * 1024 * 1024));
        assert!(pressure_paused(true, total, 200 * 1024 * 1024));
        assert!(!pressure_paused(true, total, 300 * 1024 * 1024));
    }

    #[test]
    fn pressure_hysteresis() {
        let gib = 1024 * 1024 * 1024;
        assert!(pressure_paused(false, 16 * gib, gib / 2));
        assert!(pressure_paused(true, 16 * gib, gib));
        assert!(!pressure_paused(false, 16 * gib, gib));
        assert!(!pressure_paused(true, 16 * gib, 2 * gib));
        let mib = 1024 * 1024;
        assert!(physical_headroom_allows(36 * mib, 100 * mib));
        assert!(!physical_headroom_allows(36 * mib + 1, 100 * mib));
        assert!(!physical_headroom_allows(gib, 100 * mib));
        assert!(!physical_headroom_allows(1, 63 * mib));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nested_cgroup_limits_include_ancestors_and_ignore_bad_records() {
        let directory = Directory::new();
        let root = directory.0.join("cgroup");
        let leaf = root.join("user.slice/app.scope");
        fs::create_dir_all(&leaf).unwrap();
        for (path, max, current) in [
            (root.clone(), "max", "0"),
            (root.join("user.slice"), "800", "750"),
            (leaf.clone(), "600", "100"),
        ] {
            fs::write(path.join("memory.max"), max).unwrap();
            fs::write(path.join("memory.current"), current).unwrap();
        }
        let member = "3:cpu:/elsewhere\n0::/user.slice/app.scope\n";
        assert_eq!(cgroup_pressure(1000, 900, &root, member), (600, 50));
        fs::write(leaf.join("memory.current"), "malformed").unwrap();
        assert_eq!(cgroup_pressure(1000, 900, &root, member), (800, 50));
        // Malformed membership cannot walk outside the mounted hierarchy.
        fs::write(directory.0.join("memory.max"), "1").unwrap();
        fs::write(directory.0.join("memory.current"), "1").unwrap();
        assert_eq!(cgroup_pressure(1000, 900, &root, "0::/../"), (1000, 900));
        assert_eq!(cgroup_pressure(1000, 900, &root, "malformed"), (1000, 900));
        fs::remove_file(root.join("user.slice/memory.max")).unwrap();
        assert_eq!(cgroup_pressure(1000, 900, &root, member), (1000, 900));
    }

    #[test]
    fn reclaim_epoch_is_shared_and_persistent() {
        let root = Directory::new();
        let first = Budget::new(root.0.clone()).unwrap();
        let second = Budget::new(root.0.clone()).unwrap();
        assert_eq!(first.reclaim_epoch().unwrap(), 0);
        first.request_reclaim().unwrap();
        assert_eq!(second.reclaim_epoch().unwrap(), 1);
        second.request_reclaim().unwrap();
        assert_eq!(first.reclaim_epoch().unwrap(), 2);
        assert_eq!(
            Budget::new(root.0.clone())
                .unwrap()
                .reclaim_epoch()
                .unwrap(),
            2
        );
    }

    #[test]
    fn lease_process_actor() {
        let Some(root) = std::env::var_os("APERTURE_TEST_MEMORY_ROOT") else {
            return;
        };
        let budget = Budget::new(PathBuf::from(&root)).unwrap();
        let _lease = budget.try_reserve(MIN_LIMIT_BYTES, false).unwrap().unwrap();
        fs::write(PathBuf::from(root).join("ready"), "ready").unwrap();
        let _ = std::io::stdin().read(&mut [0]);
    }

    #[test]
    fn abrupt_process_exit_reclaims_a_lease() {
        let root = Directory::new();
        let budget = Budget::new(root.0.clone()).unwrap();
        budget.set_limit(MIN_LIMIT_BYTES).unwrap();
        let test = format!(
            "{}::lease_process_actor",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env("APERTURE_TEST_MEMORY_ROOT", &root.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.0.join("ready").exists() {
            if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Memory lease actor did not become ready.");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(budget.status().unwrap().used_bytes, MIN_LIMIT_BYTES);
        assert!(budget.try_reserve(1, false).unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        assert!(budget
            .try_reserve(MIN_LIMIT_BYTES, false)
            .unwrap()
            .is_some());
    }
}
