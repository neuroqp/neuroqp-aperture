#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod annotation_io;
mod backend;
mod channels;
mod decoder;
mod native;
mod raw_tiff;
mod roi;
mod tiff_annotations;
mod vips_worker;

use backend::{Backend, ImageState};
use std::sync::atomic::Ordering;
use tauri::{Emitter, Manager, State};

#[tauri::command]
fn open_image(path: String, backend: State<'_, Backend>) -> Result<ImageState, String> {
    backend.open(std::path::Path::new(&path))
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
    if job.state.lock().unwrap().id != id {
        return Err("The image has changed".into());
    }
    Ok(job)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationTarget {
    path: String,
    exists: bool,
    is_tiff: bool,
}

#[tauri::command]
async fn annotation_target(
    id: String,
    path: Option<String>,
    backend: State<'_, Backend>,
) -> Result<AnnotationTarget, String> {
    let job = annotation_job(&backend, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let is_tiff = annotation_io::is_tiff(&job.source)?;
        let target = path.map(std::path::PathBuf::from).unwrap_or_else(|| {
            if is_tiff {
                job.source.clone()
            } else {
                job.source.with_extension("tif")
            }
        });
        Ok(AnnotationTarget {
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

#[tauri::command]
async fn save_annotations(
    id: String,
    path: String,
    rois: Vec<roi::PointRoi>,
    overwrite: bool,
    backend: State<'_, Backend>,
) -> Result<(), String> {
    let job = annotation_job(&backend, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        // Serialize saves and keep sourceIndex stable even after deleting or
        // splitting groups in a previous overwrite of this image.
        let mut annotations = job.annotations.lock().unwrap();
        if annotations.is_none() {
            *annotations = Some(annotation_io::load_basis(&job.source)?);
        }
        annotation_io::save(
            &job.source,
            std::path::Path::new(&path),
            rois,
            annotations.as_ref().unwrap(),
            overwrite,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--image-worker")) {
        if let Err(error) = vips_worker::worker_main() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let root = app.path().app_cache_dir()?.join("display-v2");
            let mut backend = Backend::new(root).map_err(std::io::Error::other)?;
            let handle = app.handle().clone();
            backend.set_notifier(move |state| {
                let _ = handle.emit("image-state", state);
            });
            app.manage(backend);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_image,
            image_extensions,
            folder_images,
            image_state,
            cancel_load,
            load_annotations,
            annotation_target,
            save_annotations
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
        .build(tauri::generate_context!())
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
