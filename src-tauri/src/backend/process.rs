//! Decoder processes: direct libvips tools, the worker and their memory admission.
use super::{
    cache::{tree_size, CACHE_LIMIT},
    Job, Phase,
};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    process::Stdio,
    sync::{atomic::Ordering, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub(super) struct DecoderMemory {
    bytes: u64,
    // Additional admission after metadata discovery keeps the initial lease alive.
    reservations: Vec<crate::memory::Reservation>,
}

pub(super) fn input(job: &Job, options: &str) -> OsString {
    OsString::from(format!("{}{}", job.input.display(), options))
}

// Before first pixels, count only this bounded preview workspace; prune and
// count the persisted cache once the preview is visible. Short read-only metadata
// commands then avoid repeatedly traversing thousands of cached display tiles.
pub(super) fn budget_exceeded(job: &Job, finished: bool) -> bool {
    if !job.housekeeping.load(Ordering::Relaxed) {
        if finished || job.budget_check.lock().unwrap().elapsed() >= Duration::from_millis(500) {
            *job.budget_check.lock().unwrap() = Instant::now();
            return tree_size(&job.work) > CACHE_LIMIT / 2;
        }
        return false;
    }
    let mut last = job.budget_check.lock().unwrap();
    if !finished && last.elapsed() < Duration::from_millis(500) {
        return false;
    }
    *last = Instant::now();
    tree_size(&job.root) > CACHE_LIMIT
}

// Each decode lives in a killable process. libvips' operation cache is bounded;
// codec memory is not a hard RSS limit and should be measured on representative files.
pub(super) fn run(job: &Job, program: &str, args: &[OsString]) -> Result<String, String> {
    run_with_progress(job, program, args, None)
}

pub(super) fn run_with_progress(
    job: &Job,
    program: &str,
    args: &[OsString],
    progress: Option<(f64, f64)>,
) -> Result<String, String> {
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    if uses_worker(job) && matches!(program, "vips" | "vipsheader") {
        let args = args
            .iter()
            .map(|arg| {
                arg.to_str()
                    .map(String::from)
                    .ok_or("Image decoder path is not UTF-8")
            })
            .collect::<Result<Vec<_>, _>>()?;
        return worker_call(
            job,
            &crate::vips_worker::Request::Command {
                program: program.into(),
                args,
            },
            progress,
        );
    }
    // Direct tools have the same admission rules as the worker. Keep this lease
    // local until the child has exited or has been killed and reaped, including
    // when Job::cancel drains the process table from another thread.
    let bytes = if matches!(program, "vips" | "vipsheader") {
        decoder_workspace(job)?
    } else {
        // Metadata tools do not need a second image workspace beside a live worker.
        8 * 1024 * 1024
    };
    let _memory = reserve_workspace(job, bytes)?;
    let dir = job.work.clone();
    let number = job.process_number.fetch_add(1, Ordering::Relaxed);
    let stdout_path = dir.join(format!("stdout-{number}"));
    let stderr_path = dir.join(format!("stderr-{number}"));
    let stdout = File::create(&stdout_path).map_err(|e| e.to_string())?;
    let stderr = File::create(&stderr_path).map_err(|e| e.to_string())?;
    let mut command = crate::platform::tool(program);
    let threads = crate::vips_worker::VIPS_THREADS.to_string();
    command
        .args(args)
        .env("TMPDIR", dir.join("tmp"))
        .env("VIPS_CONCURRENCY", threads);
    if program == "vips" || program == "vipsheader" {
        command.args(crate::vips_worker::tool_options());
    }
    let process = {
        // Serialize spawning with cancellation so replacement cannot leave a
        // decoder process starting after its job has been cancelled.
        let mut processes = job.processes.lock().unwrap();
        if job.cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let child = command.stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr)).spawn().map_err(|e| format!("Cannot run {program}: {e}. Install libvips tools (and libtiff tools for existing page pyramids)."))?;
        let process = Arc::new(Mutex::new(child));
        processes.insert(number, process.clone());
        process
    };
    let status = loop {
        if job.cancel.load(Ordering::Relaxed) {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            job.processes.lock().unwrap().remove(&number);
            let _ = fs::remove_file(stdout_path);
            let _ = fs::remove_file(stderr_path);
            return Err("Loading cancelled.".into());
        }
        let status = process.lock().unwrap().try_wait();
        match status {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(error) => {
                let mut child = process.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
                drop(child);
                job.processes.lock().unwrap().remove(&number);
                return Err(error.to_string());
            }
        }
        if budget_exceeded(job, false) {
            let mut child = process.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            job.processes.lock().unwrap().remove(&number);
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err("The 2 GiB display cache limit was reached. This image needs more temporary disk space than this proof of concept allows.".into());
        }
        thread::sleep(Duration::from_millis(2));
    };
    job.processes.lock().unwrap().remove(&number);
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let mut output = String::new();
    File::open(&stdout_path)
        .map_err(|e| e.to_string())?
        .take(65536)
        .read_to_string(&mut output)
        .map_err(|e| e.to_string())?;
    if !status.success() {
        eprintln!("{program} exited with {status}:");
        if let Ok(mut errors) = File::open(&stderr_path) {
            let _ = std::io::copy(&mut errors, &mut std::io::stderr().lock());
        }
    }
    let _ = fs::remove_file(stdout_path);
    let _ = fs::remove_file(stderr_path);
    if budget_exceeded(job, program == "vips") {
        return Err("Display cache limit reached.".into());
    }
    if status.success() {
        Ok(output)
    } else {
        Err("Could not read this image. It may be damaged or use an unsupported format.".into())
    }
}

pub(super) fn uses_worker(job: &Job) -> bool {
    #[cfg(test)]
    if std::env::var_os("APERTURE_WORKER_EXECUTABLE").is_none() {
        return false;
    }
    cfg!(windows)
        || matches!(
            job.input.extension().and_then(|s| s.to_str()),
            Some("tif" | "tiff" | "bmp")
        )
}

pub(super) fn decoded_workspace(
    width: u32,
    height: u32,
    bands: u64,
    sample_bytes: u64,
) -> Result<u64, String> {
    // Full-image codecs can retain source pixels alongside decoder/output copies.
    // Four copies also cover BMP's two retained loader variants and copy peak.
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|bytes| bytes.checked_mul(bands))
        .and_then(|bytes| bytes.checked_mul(sample_bytes))
        .and_then(|bytes| bytes.checked_mul(4))
        .ok_or_else(|| "Image decoder workspace is too large.".into())
}

fn decoder_workspace(job: &Job) -> Result<u64, String> {
    let state = job.state.lock().unwrap();
    let preview = state.channels.len().max(1) as u64 * 1024 * 1024 * 4;
    let (overhead, frame) = if job.selection.is_some() {
        // Frame display TIFFs have one whole-image strip; preserve its full decode.
        (
            128 * 1024 * 1024,
            decoded_workspace(state.width, state.height, state.channels.len() as u64, 1)?,
        )
    } else {
        // A bounded metadata/streaming allowance, plus geometry for codecs that
        // can materialize a complete frame. Small images fit the minimum budget.
        (
            64 * 1024 * 1024,
            job.frame_memory
                .load(Ordering::Relaxed)
                .max(64 * 1024 * 1024),
        )
    };
    frame
        .checked_add(overhead)
        .and_then(|bytes| bytes.checked_add(preview))
        .ok_or_else(|| "Image decoder workspace is too large.".into())
}

fn reserve_workspace(job: &Job, bytes: u64) -> Result<crate::memory::Reservation, String> {
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let reservation = if let Some(acquisition) = &job.acquisition {
        acquisition.reserve_workspace(bytes, job.background)?
    } else {
        job.memory
            .reserve(bytes, job.background, Some(&job.cancel))?
    };
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    reservation.ok_or_else(|| "Image decoding does not fit the memory limit. Increase it in Settings or close another image window.".into())
}

pub(super) fn worker_call(
    job: &Job,
    request: &crate::vips_worker::Request,
    progress: Option<(f64, f64)>,
) -> Result<String, String> {
    let exceeded = std::cell::Cell::new(false);
    let call = |decoder: &crate::decoder::Decoder| {
        decoder.call(
            request,
            &job.cancel,
            || {
                exceeded.set(budget_exceeded(job, false));
                exceeded.get()
            },
            |fraction| {
                if let Some((start, end)) = progress {
                    // Rough work percentage: preview 20%, pyramid 79%, final writes 1%.
                    job.report_progress(start + (end - start) * fraction);
                }
            },
        )
    };
    let decoder = worker(job)?;
    let mut output = call(&decoder);
    // A crashed decoder fails its in-flight call; retry it once with a fresh
    // decoder. Cancellation, cache-limit stops and failed loads are not retried.
    if output.is_err()
        && decoder.stopped()
        && !exceeded.get()
        && !job.cancel.load(Ordering::Relaxed)
        && job.state.lock().unwrap().phase != Phase::Error
    {
        output = call(&*worker(job)?);
    }
    let output = output?;
    if budget_exceeded(job, true) {
        return Err("Display cache limit reached.".into());
    }
    Ok(output)
}

fn worker(job: &Job) -> Result<Arc<crate::decoder::Decoder>, String> {
    let mut slot = job.decoder.lock().unwrap();
    if job.cancel.load(Ordering::Relaxed) {
        return Err("Loading cancelled.".into());
    }
    let amount = decoder_workspace(job)?;
    let mut memory = job.decoder_memory.lock().unwrap();
    let previous = memory.as_ref().map_or(0, |memory| memory.bytes);
    if amount > previous {
        let reservation = reserve_workspace(job, amount - previous)?;
        let memory = memory.get_or_insert_with(|| DecoderMemory {
            bytes: 0,
            reservations: Vec::new(),
        });
        memory.bytes = amount;
        memory.reservations.push(reservation);
    }
    // Replace a decoder that exited while idle or stopped after a failed call.
    if slot.as_ref().is_none_or(|decoder| decoder.stopped()) {
        *slot = match crate::decoder::Decoder::start(&job.work) {
            Ok(decoder) => Some(Arc::new(decoder)),
            Err(error) => {
                memory.take();
                return Err(error);
            }
        };
    }
    Ok(slot.as_ref().unwrap().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{tests::workspace_fixture, LoadContext, LoadMode};

    #[test]
    fn decoder_workspace_accounts_for_rgba_ushort_and_eager_bmp() {
        assert_eq!(decoded_workspace(8000, 8000, 4, 2).unwrap(), 2_048_000_000);
        assert_eq!(decoded_workspace(8000, 8000, 3, 1).unwrap(), 768_000_000);
        assert!(decoded_workspace(u32::MAX, u32::MAX, 4, 2).is_err());
        let (root, backend, job) = workspace_fixture();
        // The browser allowance plus a small image workspace fits 256 MiB.
        assert!(
            96 * 1024 * 1024 + decoder_workspace(&job).unwrap() <= crate::memory::MIN_LIMIT_BYTES
        );
        let bmp = root.join("source.bmp");
        image::RgbaImage::new(16, 12).save(&bmp).unwrap();
        let context = LoadContext {
            root: backend.root.clone(),
            memory: backend.memory.clone(),
            cache_access: backend.cache_access.clone(),
        };
        let bmp_job = context
            .create_job(&bmp, bmp.clone(), None, None, LoadMode::Foreground, None)
            .unwrap();
        assert_eq!(
            bmp_job.frame_memory.load(Ordering::Relaxed),
            16 * 12 * 4 * 4
        );
        backend.shutdown();
        drop((job, bmp_job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_foreground_workspace_reclaims_another_windows_optional_memory() {
        let (root, backend, job) = workspace_fixture();
        let peer = crate::memory::Budget::new(root.join("cache")).unwrap();
        let optional = peer.try_reserve(200 * 1024 * 1024, false).unwrap().unwrap();
        let epoch = peer.reclaim_epoch().unwrap();
        let maintenance = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while peer.reclaim_epoch().unwrap() == epoch {
                assert!(
                    Instant::now() < deadline,
                    "Foreground admission did not request reclaim"
                );
                thread::sleep(Duration::from_millis(2));
            }
            drop(optional);
        });
        let bytes = decoder_workspace(&job).unwrap();
        let reservation = reserve_workspace(&job, bytes).unwrap();
        maintenance.join().unwrap();
        assert_eq!(backend.memory.status().unwrap().used_bytes, bytes);
        drop(reservation);
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_decoder_refuses_oversized_geometry_before_spawning() {
        let (root, backend, job) = workspace_fixture();
        job.frame_memory.store(
            decoded_workspace(8000, 8000, 4, 2).unwrap(),
            Ordering::Relaxed,
        );
        let error = run(&job, "vipsheader", &["-a".into(), input(&job, "")]).unwrap_err();
        assert!(error.contains("does not fit the memory limit"), "{error}");
        assert!(job.processes.lock().unwrap().is_empty());
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn direct_admission_cancels_before_spawn_and_releases_after_reaping() {
        let (root, backend, job) = workspace_fixture();
        let full = backend
            .memory
            .try_reserve(crate::memory::MIN_LIMIT_BYTES, false)
            .unwrap()
            .unwrap();
        let epoch = backend.memory.reclaim_epoch().unwrap();
        let pending = {
            let job = job.clone();
            thread::spawn(move || run(&job, "sleep", &["0".into()]))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while backend.memory.reclaim_epoch().unwrap() == epoch {
            assert!(
                Instant::now() < deadline,
                "Direct command did not wait for admission"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(job.processes.lock().unwrap().is_empty());
        let cancelled = Instant::now();
        job.cancel();
        assert!(pending.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(cancelled.elapsed() < Duration::from_secs(1));
        drop(full);
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);

        job.cancel.store(false, Ordering::Relaxed);
        run(&job, "sleep", &["0".into()]).unwrap();
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        let running = {
            let job = job.clone();
            thread::spawn(move || run(&job, "sleep", &["30".into()]))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.processes.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "Admitted child did not spawn");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(backend.memory.status().unwrap().used_bytes, 8 * 1024 * 1024);
        job.cancel();
        assert!(running.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(job.processes.lock().unwrap().is_empty());
        assert_eq!(backend.memory.status().unwrap().used_bytes, 0);
        backend.shutdown();
        drop((job, backend));
        fs::remove_dir_all(root).unwrap();
    }
}
