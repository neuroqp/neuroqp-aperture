//! C decoding lives in a child process, so cancellation can interrupt native I/O.
use crate::vips_worker::{Request, Response, MAX_MESSAGE};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::Duration,
};

pub struct Decoder {
    child: Mutex<Child>,
    io: Mutex<(ChildStdin, mpsc::Receiver<Result<Response, String>>)>,
}

impl Decoder {
    pub fn start(work: &Path) -> Result<Self, String> {
        #[cfg(not(test))]
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        #[cfg(test)]
        let executable = std::env::var_os("APERTURE_WORKER_EXECUTABLE")
            .ok_or("Worker test executable is not configured")?;
        Self::spawn(executable.as_ref(), work)
    }

    fn spawn(executable: &std::ffi::OsStr, work: &Path) -> Result<Self, String> {
        let mut child = Command::new(executable)
            .arg("--image-worker")
            .env("TMPDIR", work.join("tmp"))
            .env("VIPS_CONCURRENCY", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(work.join("decoder.stderr")).map_err(|e| e.to_string())?)
            .spawn()
            .map_err(|e| format!("Cannot start image decoder: {e}"))?;
        let stdin = child.stdin.take().ok_or("Missing decoder input")?;
        let stdout = child.stdout.take().ok_or("Missing decoder output")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let result = reader
                    .by_ref()
                    .take((MAX_MESSAGE + 1) as u64)
                    .read_until(b'\n', &mut bytes);
                let response = match result {
                    Ok(0) => Err("Image decoder stopped unexpectedly.".into()),
                    Ok(_) if bytes.len() > MAX_MESSAGE || bytes.last() != Some(&b'\n') => {
                        Err("Invalid image decoder response.".into())
                    }
                    Ok(_) => serde_json::from_slice(&bytes)
                        .map_err(|e| format!("Invalid image decoder response: {e}")),
                    Err(e) => Err(e.to_string()),
                };
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child: Mutex::new(child),
            io: Mutex::new((stdin, receiver)),
        })
    }

    pub fn call(
        &self,
        request: &Request,
        cancel: &AtomicBool,
        exceeded: impl Fn() -> bool,
    ) -> Result<String, String> {
        let mut io = self.io.lock().unwrap();
        if cancel.load(Ordering::Relaxed) {
            return Err("Loading cancelled.".into());
        }
        let mut bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_MESSAGE {
            return Err("Image decoder request is too large.".into());
        }
        if let Err(e) = io.0.write_all(&bytes).and_then(|()| io.0.flush()) {
            self.stop();
            return Err(format!("Cannot contact image decoder: {e}"));
        }
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.stop();
                return Err("Loading cancelled.".into());
            }
            if exceeded() {
                self.stop();
                return Err("Display cache limit reached.".into());
            }
            match io.1.recv_timeout(Duration::from_millis(2)) {
                Ok(Ok(response)) => {
                    return response.result.map_err(|error| {
                        eprintln!("Image decoder: {error}");
                        "Could not read this image. It may be damaged or use an unsupported format."
                            .into()
                    })
                }
                Ok(Err(e)) => {
                    self.stop();
                    return Err(e);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return Err("Image decoder stopped unexpectedly.".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
        }
    }

    pub fn stop(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::Arc,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    fn helper(body: &str) -> (Decoder, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "aperture-decoder-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let script = root.join("worker.py");
        fs::write(
            &script,
            format!("#!/usr/bin/python3\nimport sys,time\nsys.stdin.readline()\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        (Decoder::spawn(script.as_os_str(), &root).unwrap(), root)
    }

    fn request() -> Request {
        Request::Command {
            program: "vipsheader".into(),
            args: vec!["-a".into(), "source.tif".into()],
        }
    }

    #[test]
    fn broken_protocol_reaps_worker_and_cancel_interrupts_wait() {
        for body in [
            "pass",
            "print('not json',flush=True)",
            "print('x'*65537,flush=True)",
        ] {
            let (decoder, root) = helper(body);
            let pid = decoder.child.lock().unwrap().id();
            assert!(decoder
                .call(&request(), &AtomicBool::new(false), || false)
                .is_err());
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
            drop(decoder);
            fs::remove_dir_all(root).unwrap();
        }
        let (decoder, root) = helper("time.sleep(30)");
        let decoder = Arc::new(decoder);
        let pid = decoder.child.lock().unwrap().id();
        let cancel = Arc::new(AtomicBool::new(false));
        let call = {
            let decoder = decoder.clone();
            let cancel = cancel.clone();
            thread::spawn(move || decoder.call(&request(), &cancel, || false))
        };
        thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        decoder.stop();
        assert!(call.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        drop(decoder);
        fs::remove_dir_all(root).unwrap();
    }
}
