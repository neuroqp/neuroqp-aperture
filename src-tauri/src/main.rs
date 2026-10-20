#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod annotation_io;
mod backend;
mod channels;
mod czi;
mod decoder;
mod frame_cache;
mod frames;
mod instances;
mod memory;
mod native;
mod nd2;
mod nd2_metadata;
mod platform;
mod preload;
mod raw_tiff;
mod roi;
mod tiff_annotations;
mod tiff_ifd;
mod updates;
mod vips_worker;

use backend::{Backend, ImageState};
use instances::{fingerprint, path_key, Fingerprint, Instances};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
};
use tauri::{Emitter, Manager, State};

struct Session {
    instances: Arc<Instances>,
    loaded: Mutex<Option<LoadedImage>>,
    opening: Mutex<()>,
    startup: Mutex<Option<PathBuf>>,
}

struct LoadedImage {
    id: String,
    source: PathBuf,
    fingerprints: HashMap<PathBuf, Option<Fingerprint>>,
}

#[tauri::command]
async fn open_image(path: String, app: tauri::AppHandle) -> Result<ImageState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        open_image_path(
            Path::new(&path),
            &app.state::<Backend>(),
            &app.state::<Arc<Session>>(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn open_image_path(
    path: &Path,
    backend: &Backend,
    session: &Session,
) -> Result<ImageState, String> {
    let _opening = session.opening.lock().unwrap();
    // The frontend has discarded the previous image before requesting replacement.
    // Release its presence even when path validation or opening the new image fails.
    backend.cancel();
    *session.loaded.lock().unwrap() = None;
    session
        .instances
        .set_image(None)
        .map_err(|e| e.to_string())?;
    let source = path_key(path).map_err(|e| e.to_string())?;
    let initial = {
        // Publish presence under the write lock so it cannot race a save's
        // final warning check. Release the lock before starting image work.
        let _writing = session
            .instances
            .save_lock(&source)
            .map_err(|e| e.to_string())?;
        let initial = fingerprint(&source).map_err(|e| e.to_string())?;
        session
            .instances
            .set_image(Some(&source))
            .map_err(|e| e.to_string())?;
        initial
    };
    let state = match backend.open_resolved(path, &source) {
        Ok(state) => state,
        Err(error) => {
            session
                .instances
                .set_image(None)
                .map_err(|e| e.to_string())?;
            return Err(error);
        }
    };
    *session.loaded.lock().unwrap() = Some(LoadedImage {
        id: state.id.clone(),
        fingerprints: HashMap::from([(source.clone(), initial)]),
        source,
    });
    Ok(state)
}

#[tauri::command]
fn startup_image(session: State<'_, Arc<Session>>) -> Result<Option<String>, String> {
    session
        .startup
        .lock()
        .unwrap()
        .take()
        .map(|path| {
            path.into_os_string()
                .into_string()
                .map_err(|_| "The image filename is not valid Unicode.".into())
        })
        .transpose()
}

#[tauri::command]
fn new_window() -> Result<(), String> {
    let mut child = std::process::Command::new(updates::launch_executable()?)
        .spawn()
        .map_err(|error| format!("Cannot open another window: {error}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn startup_path(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    let Some(argument) = args.into_iter().skip(1).find(|argument| argument != "--") else {
        return Ok(None);
    };
    if let Some(uri) = argument
        .to_str()
        .filter(|argument| argument.starts_with("file://"))
    {
        let path = tauri::Url::parse(uri)
            .map_err(|error| error.to_string())?
            .to_file_path()
            .map_err(|_| "Cannot open this file URL.".to_owned())?;
        return Ok(Some(path));
    }
    Ok(Some(PathBuf::from(argument)))
}

#[tauri::command]
fn image_extensions() -> &'static [&'static str] {
    backend::IMAGE_EXTENSIONS
}
#[tauri::command]
async fn folder_images(path: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        backend::folder_images(std::path::Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn image_state(backend: State<'_, Backend>) -> Option<ImageState> {
    backend.snapshot()
}
#[tauri::command]
async fn memory_status(app: tauri::AppHandle) -> Result<memory::MemoryStatus, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<Backend>().memory.status())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn set_memory_budget(
    limit_bytes: u64,
    app: tauri::AppHandle,
) -> Result<memory::MemoryStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Backend>().set_memory_budget(limit_bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn nd2_preload_status(backend: State<'_, Backend>) -> preload::PreloadStatus {
    backend.preload_status()
}
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Named IPC fields keep navigation hints optional.
async fn select_plane(
    id: String,
    position: u32,
    z: u32,
    time: u32,
    projection: Option<frames::Projection>,
    navigation_axis: Option<String>,
    navigation_direction: Option<i8>,
    app: tauri::AppHandle,
) -> Result<ImageState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let backend = app.state::<Backend>();
        let session = app.state::<Arc<Session>>();
        let _opening = session.opening.lock().unwrap();
        backend.navigation_hint(navigation_axis.as_deref(), navigation_direction);
        let state =
            backend.select_view(&id, frames::Selection { position, z, time }, projection)?;
        if let Some(loaded) = session.loaded.lock().unwrap().as_mut() {
            loaded.id = state.id.clone();
        }
        Ok(state)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn cancel_load(backend: State<'_, Backend>) {
    backend.cancel();
}

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
struct AnnotationTarget {
    path: String,
    exists: bool,
    is_tiff: bool,
    warning: Option<String>,
    acknowledgment: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SaveResult {
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
async fn annotation_target(
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
async fn load_annotations(
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
async fn save_annotations(
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

/// Resolve the same native cache directory used by Tauri before creating a WebView.
fn app_cache_root(identifier: &str) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".cache")))
        .ok_or_else(|| std::io::Error::other("Cannot locate the user cache directory."))?;
    #[cfg(windows)]
    let root = windows_cache_root()?;
    Ok(root.join(identifier))
}

#[cfg(windows)]
fn windows_cache_root() -> std::io::Result<PathBuf> {
    use std::{
        ffi::{c_void, OsString},
        os::windows::ffi::OsStringExt,
    };
    use windows_sys::core::GUID;
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(
            folder: *const GUID,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(memory: *mut c_void);
    }
    const LOCAL_APP_DATA: GUID = GUID {
        data1: 0xf1b32785,
        data2: 0x6fba,
        data3: 0x4fcf,
        data4: [0x9d, 0x55, 0x7b, 0x8e, 0x7f, 0x15, 0x70, 0x91],
    };
    let mut directory = std::ptr::null_mut();
    // The OS returns an allocated, null-terminated UTF-16 directory path.
    unsafe {
        let result = SHGetKnownFolderPath(&LOCAL_APP_DATA, 0, std::ptr::null_mut(), &mut directory);
        if result < 0 || directory.is_null() {
            CoTaskMemFree(directory.cast());
            return Err(std::io::Error::other(format!(
                "Cannot locate LocalAppData: HRESULT {result:#x}"
            )));
        }
        let mut length = 0;
        while *directory.add(length) != 0 {
            length += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
            directory, length,
        )));
        CoTaskMemFree(directory.cast());
        Ok(path)
    }
}

#[cfg(windows)]
fn configure_webview_runtime() {
    if let Ok(executable) = std::env::current_exe() {
        let runtime = executable.with_file_name("webview2");
        if runtime.join("msedgewebview2.exe").is_file() {
            // This runs before Tauri starts threads or creates the WebView.
            std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", runtime);
        }
    }
}

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--image-worker")) {
        if let Err(error) = vips_worker::worker_main() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let context = tauri::generate_context!();
    let cache_root =
        app_cache_root(&context.config().identifier).expect("Cannot locate Aperture cache");
    let instances =
        Arc::new(Instances::new(cache_root.clone()).expect("Cannot coordinate Aperture startup"));
    #[cfg(windows)]
    configure_webview_runtime();
    let startup = startup_path(std::env::args_os());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .manage(updates::Updates::default())
        .setup(move |app| {
            if app.path().app_cache_dir()? != cache_root {
                return Err(std::io::Error::other("The configured Aperture cache directory differs from the startup coordinator directory.").into());
            }
            let memory = memory::Budget::new(cache_root.clone()).map_err(std::io::Error::other)?;
            let mut backend = Backend::with_memory(instances.cache_root().to_path_buf(), memory)
                .map_err(std::io::Error::other)?;
            let handle = app.handle().clone();
            backend.set_notifier(move |state| {
                let _ = handle.emit("image-state", state);
            });
            app.manage(backend);
            app.manage(Arc::new(Session {
                instances,
                loaded: Mutex::new(None),
                opening: Mutex::new(()),
                startup: Mutex::new(startup.map_err(std::io::Error::other)?),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_image,
            select_plane,
            startup_image,
            new_window,
            image_extensions,
            folder_images,
            image_state,
            memory_status,
            set_memory_budget,
            nd2_preload_status,
            cancel_load,
            load_annotations,
            annotation_target,
            save_annotations,
            updates::update_package,
            updates::check_update,
            updates::download_update,
            updates::install_update,
            updates::update_release_page,
            updates::download_portable_update
        ])
        .register_asynchronous_uri_scheme_protocol("aperture", |context, request, responder| {
            let app = context.app_handle().clone();
            let backend = app.state::<Backend>();
            if backend.closing.load(Ordering::Relaxed) {
                responder.respond(response(503, b"Viewer shutting down.".to_vec()));
                return;
            }
            let path = request.uri().path().to_string();
            let job = backend.current.lock().unwrap().clone();
            let Some(job) = job else {
                responder.respond(response(404, b"No image loaded.".to_vec()));
                return;
            };
            let state = job.state.lock().unwrap().clone();
            let asset = backend::asset(&path, &state.id, state.width, state.height).or_else(|| {
                state
                    .channel_mode
                    .then(|| {
                        backend::channel_asset(
                            &path,
                            &state.id,
                            state.width,
                            state.height,
                            state.channels.len(),
                        )
                    })
                    .flatten()
            });
            let Some(asset) = asset else {
                responder.respond(response(404, b"Invalid image asset.".to_vec()));
                return;
            };
            if backend.requests.fetch_add(1, Ordering::Relaxed) >= 16 {
                backend.requests.fetch_sub(1, Ordering::Relaxed);
                responder.respond(response(503, b"Too many image requests.".to_vec()));
                return;
            }
            let slots = backend.tile_slots.clone();
            tauri::async_runtime::spawn(async move {
                // Preview delivery must not queue behind unfinished pyramid tiles.
                let permit = if matches!(
                    &asset,
                    backend::Asset::Preview | backend::Asset::ChannelPreview { .. }
                ) {
                    Ok(None)
                } else {
                    job.tile_permit(slots).await.map(Some)
                };
                let result = match permit.as_ref() {
                    _ if job.cancel.load(Ordering::Relaxed) => {
                        Err((410, "Image replaced or cancelled.".into()))
                    }
                    Ok(_) => {
                        let active = job.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            backend::read_asset(&active, asset)
                        })
                        .await
                        .unwrap_or_else(|e| Err((500, e.to_string())))
                    }
                    Err(error) => Err(error.clone()),
                };
                drop(permit);
                app.state::<Backend>()
                    .requests
                    .fetch_sub(1, Ordering::Relaxed);
                let result = if job.cancel.load(Ordering::Relaxed) {
                    Err((410, "Image replaced or cancelled.".into()))
                } else {
                    result
                };
                match result {
                    Ok(bytes) => responder.respond(response(200, bytes)),
                    Err((status, error)) => responder.respond(response(status, error.into_bytes())),
                }
            });
        })
        .build(context)
        .expect("Cannot start Aperture")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                app.state::<Backend>().shutdown();
            }
        });
}

fn response(status: u16, bytes: Vec<u8>) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header(
            "Content-Type",
            if status == 200 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "image/png"
            } else if status == 200 {
                "image/jpeg"
            } else {
                "text/plain; charset=utf-8"
            },
        )
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .body(bytes)
        .unwrap()
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn filename_arguments_preserve_spaces_unicode_and_separator() {
        let filename = std::ffi::OsString::from("/tmp/cells étude 01.tif");
        assert_eq!(
            startup_path(["aperture".into(), filename.clone()]).unwrap(),
            Some(PathBuf::from(&filename))
        );
        assert_eq!(
            startup_path(["aperture".into(), "--".into(), filename.clone()]).unwrap(),
            Some(PathBuf::from(filename))
        );
        assert_eq!(startup_path(["aperture".into()]).unwrap(), None);
        #[cfg(windows)]
        let (uri, expected) = (
            "file:///C:/tmp/cells%20%C3%A9tude%2001.tif",
            PathBuf::from(r"C:\tmp\cells étude 01.tif"),
        );
        #[cfg(not(windows))]
        let (uri, expected) = (
            "file:///tmp/cells%20%C3%A9tude%2001.tif",
            PathBuf::from("/tmp/cells étude 01.tif"),
        );
        assert_eq!(
            startup_path(["aperture".into(), uri.into()]).unwrap(),
            Some(expected)
        );
    }

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
    fn failed_image_replacement_releases_previous_presence_and_cancels_work() {
        let root = std::env::temp_dir().join(format!(
            "aperture-failed-replacement-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.png");
        std::fs::write(&source, []).unwrap();
        let unsupported = root.join("corrupt.unsupported");
        std::fs::write(&unsupported, b"invalid image").unwrap();
        let session = Session {
            instances: Arc::new(Instances::new(root.join("cache")).unwrap()),
            loaded: Mutex::new(None),
            opening: Mutex::new(()),
            startup: Mutex::new(None),
        };
        let observer = Instances::new(root.join("cache")).unwrap();
        let backend = Backend::new(session.instances.cache_root().to_path_buf()).unwrap();
        for failed in [
            root.join("missing.tif"),
            unsupported,
            root.join("missing-parent").join("missing.tif"),
        ] {
            open_image_path(&source, &backend, &session).unwrap();
            let previous = backend.current.lock().unwrap().clone().unwrap();
            assert!(observer.others_open(&source).unwrap());
            assert!(open_image_path(&failed, &backend, &session).is_err());
            assert!(previous.cancel.load(Ordering::Relaxed));
            assert!(session.loaded.lock().unwrap().is_none());
            assert!(!observer.others_open(&source).unwrap());
            if path_key(&failed).is_ok() {
                assert!(!observer.others_open(&failed).unwrap());
            }
        }
        backend.shutdown();
        drop(session);
        drop(observer);
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
