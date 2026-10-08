#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod annotation_io;
mod backend;
mod native;
mod roi;
mod tiff_annotations;

use backend::{Backend, ImageState};
use std::sync::atomic::Ordering;
use tauri::{Manager, State};

#[tauri::command]
fn open_image(path: String, backend: State<'_, Backend>) -> Result<ImageState, String> {
    backend.open(std::path::Path::new(&path))
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
        let mut annotations = job.annotations.lock().unwrap();
        if annotations.is_none() {
            *annotations = Some(annotation_io::load_basis(&job.source)?);
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
            app.manage(Backend::new(root).map_err(std::io::Error::other)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_image,
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
            let Some(asset) = backend::asset(&path, &state.id, state.width, state.height) else {
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
                let permit = if matches!(&asset, backend::Asset::Preview) {
                    None
                } else {
                    Some(slots.acquire_owned().await)
                };
                let result = if permit.as_ref().map(Result::is_ok).unwrap_or(true) {
                    tauri::async_runtime::spawn_blocking(move || backend::read_asset(&job, asset))
                        .await
                        .unwrap_or_else(|e| Err((500, e.to_string())))
                } else {
                    Err((503, "Viewer shutting down.".into()))
                };
                drop(permit);
                app.state::<Backend>()
                    .requests
                    .fetch_sub(1, Ordering::Relaxed);
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
            if status == 200 {
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
