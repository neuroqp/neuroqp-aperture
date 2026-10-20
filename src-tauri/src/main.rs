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
mod save;
mod tiff_annotations;
mod tiff_ifd;
mod updates;
mod vips_worker;

use backend::{Backend, ImageState};
use instances::{fingerprint, path_key, Fingerprint, Instances};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    let cache_root = platform::app_cache_root(&context.config().identifier)
        .expect("Cannot locate Aperture cache");
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
            save::load_annotations,
            save::annotation_target,
            save::save_annotations,
            updates::update_package,
            updates::check_update,
            updates::download_update,
            updates::install_update,
            updates::update_release_page,
            updates::download_portable_update
        ])
        .register_asynchronous_uri_scheme_protocol("aperture", |context, request, responder| {
            backend::assets::respond(context.app_handle().clone(), request, responder)
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

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::sync::atomic::Ordering;

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
}
