use crate::backend::Backend;
use std::{sync::Mutex, time::Duration};
use tauri::{Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};

const RELEASES: &str = "https://github.com/neuroqp/neuroqp-aperture/releases/latest";

#[derive(Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Edition {
    Installed,
    Appimage,
    Portable,
    Development,
}

fn can_install(mode: Edition) -> bool {
    matches!(mode, Edition::Installed | Edition::Appimage)
}

fn classify(
    debug: bool,
    windows: bool,
    portable: bool,
    installed: bool,
    appimage: bool,
) -> Edition {
    if debug {
        Edition::Development
    } else if windows && portable {
        Edition::Portable
    } else if windows && installed {
        Edition::Installed
    } else if !windows && appimage {
        Edition::Appimage
    } else {
        Edition::Development
    }
}

fn edition() -> Edition {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(".aperture-installed")))
        .is_some_and(|marker| marker.is_file());
    // APPIMAGE alone may be inherited when another AppImage launches a plain binary.
    let appimage = std::env::var_os("APPIMAGE")
        .map(std::path::PathBuf::from)
        .is_some_and(|path| path.is_absolute() && path.is_file())
        && std::env::var_os("APPDIR")
            .map(std::path::PathBuf::from)
            .is_some_and(|dir| {
                dir.is_absolute()
                    && dir.is_dir()
                    && std::env::current_exe().is_ok_and(|exe| exe.starts_with(dir))
            });
    classify(
        cfg!(debug_assertions),
        cfg!(target_os = "windows"),
        std::env::var_os("APERTURE_PORTABLE").as_deref() == Some(std::ffi::OsStr::new("1")),
        installed,
        cfg!(target_os = "linux") && appimage,
    )
}

#[derive(Default)]
pub struct Updates {
    pending: Mutex<Option<(Update, Option<Vec<u8>>)>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    notes: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    edition: Edition,
    version: String,
}

#[tauri::command]
pub fn update_package(app: tauri::AppHandle) -> PackageInfo {
    PackageInfo {
        edition: edition(),
        version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
pub async fn check_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
) -> Result<Option<UpdateInfo>, String> {
    let mode = edition();
    if mode == Edition::Development {
        return Err(
            "Use a released installer, portable executable, or AppImage to check for updates."
                .into(),
        );
    }
    let mut builder = app.updater_builder().timeout(Duration::from_secs(15));
    if mode == Edition::Portable {
        builder = builder.target(format!("windows-{}-portable", std::env::consts::ARCH));
    }
    let updater = builder.build().map_err(|error| error.to_string())?;
    let mut update = updater.check().await.map_err(|error| error.to_string())?;
    if let Some(update) = update.as_mut() {
        if update.signature.trim().is_empty() {
            return Err("The release is missing its required update signature.".into());
        }
        // Checking should time out quickly; downloading a large bundle can take longer.
        update.timeout = Some(Duration::from_secs(30 * 60));
    }
    let info = update.as_ref().map(|update| UpdateInfo {
        version: update.version.clone(),
        notes: update
            .body
            .clone()
            .unwrap_or_else(|| "See the release page for details.".into()),
    });
    *state.pending.lock().unwrap() = update.map(|update| (update, None));
    Ok(info)
}

fn require_installable() -> Result<(), String> {
    if can_install(edition()) {
        Ok(())
    } else {
        Err("This edition cannot install updates. Download the new portable executable from Releases.".into())
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    downloaded: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
) -> Result<(), String> {
    require_installable()?;
    let update = state
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .map(|(update, _)| update.clone())
        .ok_or("Check for updates first.")?;
    // Downloads are signature-verified by the updater before these bytes are retained.
    let mut downloaded = 0;
    let mut reported = 0;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                if downloaded - reported >= 256 * 1024 || total == Some(downloaded) {
                    reported = downloaded;
                    let _ = app.emit("update-progress", DownloadProgress { downloaded, total });
                }
            },
            || {},
        )
        .await
        .map_err(|error| error.to_string())?;
    *state.pending.lock().unwrap() = Some((update, Some(bytes)));
    Ok(())
}

#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
) -> Result<(), String> {
    require_installable()?;
    let (update, bytes) = {
        let mut pending = state.pending.lock().unwrap();
        pending
            .as_ref()
            .and_then(|(_, bytes)| bytes.as_ref())
            .ok_or("Download the update first.")?;
        pending.take().unwrap()
    };
    let bytes = bytes.unwrap();
    tauri::async_runtime::spawn_blocking(move || {
        let backend = app.state::<Backend>();
        backend.shutdown();
        if let Err(error) = update.install(&bytes) {
            *app.state::<Updates>().pending.lock().unwrap() = Some((update, Some(bytes)));
            backend
                .closing
                .store(false, std::sync::atomic::Ordering::Relaxed);
            return Err(format!("{error}. Reopen your image to resume viewing."));
        }
        app.restart();
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn update_release_page(app: tauri::AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(RELEASES, None::<&str>)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn download_portable_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
) -> Result<(), String> {
    if edition() != Edition::Portable {
        return Err("This action is available only in the portable edition.".into());
    }
    let pending = state.pending.lock().unwrap();
    let (update, _) = pending.as_ref().ok_or("Check for updates first.")?;
    let url = &update.download_url;
    // A portable executable is only downloaded in the browser, never launched.
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url
            .path()
            .starts_with("/neuroqp/neuroqp-aperture/releases/download/")
        || !url.path().ends_with(".exe")
    {
        return Err(
            "The portable download must be an executable from Aperture's GitHub Releases.".into(),
        );
    }
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_installed_packages_can_update_in_place() {
        assert!(can_install(Edition::Installed));
        assert!(can_install(Edition::Appimage));
        assert!(!can_install(Edition::Portable));
        assert!(!can_install(Edition::Development));
        assert!(classify(false, true, false, true, false) == Edition::Installed);
        assert!(classify(false, true, true, true, false) == Edition::Portable);
        assert!(classify(false, false, false, false, true) == Edition::Appimage);
        assert!(classify(false, true, false, false, false) == Edition::Development);
        assert!(classify(true, true, true, true, true) == Edition::Development);
        assert!(classify(false, true, false, false, true) == Edition::Development);
    }
}
