mod assoc;
mod daemon_client;
mod diagnostics;
mod gallery;
mod library;

use std::path::{Path, PathBuf};

use base64::Engine;

fn parse_quality(quality: &str) -> Result<ipc::Quality, String> {
    match quality {
        "eco" => Ok(ipc::Quality::Eco),
        "balanced" => Ok(ipc::Quality::Balanced),
        "max" => Ok(ipc::Quality::Max),
        other => Err(format!("unknown quality profile: {other}")),
    }
}

/// Connects to the daemon, starting it when needed; returns its version.
#[tauri::command]
fn daemon_connect() -> Result<String, String> {
    daemon_client::ensure_daemon(&daemon_client::endpoint())
}

#[tauri::command]
fn list_monitors() -> Result<Vec<ipc::Monitor>, String> {
    match daemon_client::request(&daemon_client::endpoint(), ipc::Command::ListMonitors)? {
        ipc::ResponseData::Monitors { monitors } => Ok(monitors),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

/// Session list plus the wallpaper stack's live CPU% and playlists, for the panel.
#[derive(serde::Serialize)]
struct DaemonStatus {
    sessions: Vec<ipc::SessionStatus>,
    stack_cpu_percent: Option<f32>,
    playlists: Vec<ipc::PlaylistSummary>,
}

#[tauri::command]
fn daemon_status() -> Result<DaemonStatus, String> {
    match daemon_client::request(&daemon_client::endpoint(), ipc::Command::Status)? {
        ipc::ResponseData::Status {
            sessions,
            stack_cpu_percent,
            playlists,
        } => Ok(DaemonStatus {
            sessions,
            stack_cpu_percent,
            playlists,
        }),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

#[tauri::command]
fn set_playlist(
    monitor: usize,
    items: Vec<String>,
    interval_minutes: u32,
    shuffle: bool,
    quality: String,
    volume: u8,
    anime4k: bool,
) -> Result<String, String> {
    let items = items
        .iter()
        .map(|s| {
            PathBuf::from(s)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(s))
        })
        .collect();
    acknowledged(ipc::Command::SetPlaylist {
        monitor,
        items,
        interval_minutes,
        shuffle,
        quality: parse_quality(&quality)?,
        volume,
        anime4k,
    })
}

#[tauri::command]
fn clear_playlist(monitor: Option<usize>) -> Result<String, String> {
    acknowledged(ipc::Command::ClearPlaylist { monitor })
}

#[tauri::command]
fn playlist_next(monitor: Option<usize>) -> Result<String, String> {
    acknowledged(ipc::Command::PlaylistNext { monitor })
}

#[tauri::command]
fn play(
    path: String,
    monitor: usize,
    quality: String,
    volume: u8,
    anime4k: bool,
) -> Result<String, String> {
    let path = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| format!("file not found: {error}"))?;
    let command = ipc::Command::Play {
        monitor,
        path,
        quality: parse_quality(&quality)?,
        volume,
        anime4k,
    };
    acknowledged(command)
}

#[tauri::command]
fn stop(monitor: Option<usize>) -> Result<String, String> {
    acknowledged(ipc::Command::Stop { monitor })
}

#[tauri::command]
fn pause(monitor: Option<usize>) -> Result<String, String> {
    acknowledged(ipc::Command::Pause { monitor })
}

#[tauri::command]
fn resume(monitor: Option<usize>) -> Result<String, String> {
    acknowledged(ipc::Command::Resume { monitor })
}

#[tauri::command]
fn set_volume(monitor: usize, volume: u8) -> Result<String, String> {
    acknowledged(ipc::Command::SetVolume { monitor, volume })
}

#[tauri::command]
fn set_quality(monitor: usize, quality: String, anime4k: bool) -> Result<String, String> {
    let command = ipc::Command::SetQuality {
        monitor,
        quality: parse_quality(&quality)?,
        anime4k,
    };
    acknowledged(command)
}

fn acknowledged(command: ipc::Command) -> Result<String, String> {
    match daemon_client::request(&daemon_client::endpoint(), command)? {
        ipc::ResponseData::Acknowledged { status } => Ok(status),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

#[tauri::command]
fn get_autostart() -> Result<bool, String> {
    match daemon_client::request(&daemon_client::endpoint(), ipc::Command::GetAutostart)? {
        ipc::ResponseData::Autostart { enabled } => Ok(enabled),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<String, String> {
    acknowledged(ipc::Command::SetAutostart { enabled })
}

fn parse_battery_policy(policy: &str) -> Result<ipc::BatteryPolicy, String> {
    match policy {
        "pause" => Ok(ipc::BatteryPolicy::Pause),
        "eco" => Ok(ipc::BatteryPolicy::Eco),
        "keep" => Ok(ipc::BatteryPolicy::Keep),
        other => Err(format!("unknown battery policy: {other}")),
    }
}

#[tauri::command]
fn get_battery_policy() -> Result<String, String> {
    match daemon_client::request(&daemon_client::endpoint(), ipc::Command::GetBatteryPolicy)? {
        ipc::ResponseData::BatteryPolicy { policy } => Ok(match policy {
            ipc::BatteryPolicy::Pause => "pause".into(),
            ipc::BatteryPolicy::Eco => "eco".into(),
            ipc::BatteryPolicy::Keep => "keep".into(),
        }),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
}

#[tauri::command]
fn set_battery_policy(policy: String) -> Result<String, String> {
    acknowledged(ipc::Command::SetBatteryPolicy {
        policy: parse_battery_policy(&policy)?,
    })
}

// ---------------------------------------------------------------------------
// library commands (import runs ffmpeg — keep it off the UI thread)
// ---------------------------------------------------------------------------

pub(crate) async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn library_list() -> Result<Vec<library::LibraryItem>, String> {
    blocking(|| library::Library::default_location()?.list()).await
}

#[tauri::command]
async fn library_import(path: String) -> Result<library::LibraryItem, String> {
    blocking(move || library::Library::default_location()?.import(Path::new(&path))).await
}

#[tauri::command]
async fn library_remove(id: String) -> Result<(), String> {
    blocking(move || library::Library::default_location()?.remove(&id)).await
}

/// Preview as base64 jpeg; small enough to travel over invoke.
#[tauri::command]
async fn library_preview(id: String) -> Result<String, String> {
    let bytes = blocking(move || library::Library::default_location()?.preview_jpeg(&id)).await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Packs a library item into a `.wpk` file at the chosen location.
#[tauri::command]
async fn library_export(id: String, target: String) -> Result<(), String> {
    blocking(move || library::Library::default_location()?.export(&id, Path::new(&target))).await
}

#[derive(Clone, serde::Serialize)]
struct PackageInfo {
    name: String,
    kind: String,
}

fn kind_str(kind: wpk::MediaType) -> &'static str {
    match kind {
        wpk::MediaType::Video => "video",
        wpk::MediaType::Image => "image",
        wpk::MediaType::Web => "web",
        wpk::MediaType::Model3d => "model3d",
    }
}

/// web / 3D packages ship code that runs on the desktop, so they must never be
/// installed without the user's explicit consent (see docs/research/security-model.md).
fn needs_consent(kind: wpk::MediaType) -> bool {
    matches!(kind, wpk::MediaType::Web | wpk::MediaType::Model3d)
}

/// Reads a package's manifest so the UI can decide whether to warn before import.
#[tauri::command]
async fn inspect_package(path: String) -> Result<PackageInfo, String> {
    blocking(move || {
        let manifest = wpk::read_manifest(Path::new(&path)).map_err(|error| error.to_string())?;
        Ok(PackageInfo {
            name: manifest.name,
            kind: kind_str(manifest.media_type).to_owned(),
        })
    })
    .await
}

/// Locates the bundled first-run sample web wallpaper: portable/installed
/// layout first (`web/demo` next to the exe), then the dev tree. Mirrors
/// library's two-tier asset lookup.
fn bundled_sample_entry() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("web").join("demo").join("index.html"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../assets/web/demo/index.html"),
    );
    candidates.into_iter().find(|path| path.is_file())
}

/// Imports the bundled first-party sample wallpaper so a fresh library is not
/// empty. It is our own code shipped inside the install, so it skips the
/// code-wallpaper consent that foreign `.html`/`.wpk` imports require.
#[tauri::command]
async fn import_bundled_sample() -> Result<library::LibraryItem, String> {
    blocking(|| {
        let entry = bundled_sample_entry().ok_or("bundled sample wallpaper not found")?;
        library::Library::default_location()?.import_web_folder_meta(
            &entry,
            Some("Аврора — пример".to_owned()),
            Some("LimeWall".to_owned()),
        )
    })
    .await
}

/// `.wpk` files among command line arguments (double-click / "open with").
fn packages_in(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    args.skip(1)
        .map(std::path::PathBuf::from)
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("wpk"))
                && path.is_file()
        })
        .collect()
}

#[derive(Clone, serde::Serialize)]
struct ConsentRequest {
    path: String,
    name: String,
    kind: String,
}

/// Imports packages off the UI thread and tells the panel to refresh.
///
/// Plain media (video / image) is imported directly — double-clicking a video
/// `.wpk` just works. Code-bearing packages (web / 3D) and any package whose
/// manifest cannot be read are never imported silently: the UI is asked for
/// explicit consent first (fail-safe — a missed event means no import, not a
/// silent one).
fn import_packages(app: tauri::AppHandle, packages: Vec<std::path::PathBuf>) {
    use tauri::Emitter;
    if packages.is_empty() {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let library = match library::Library::default_location() {
            Ok(library) => library,
            Err(error) => {
                eprintln!("library unavailable: {error}");
                return;
            }
        };
        let mut imported = false;
        for package in packages {
            let consent = match wpk::read_manifest(&package) {
                Ok(manifest) if needs_consent(manifest.media_type) => Some(ConsentRequest {
                    path: package.display().to_string(),
                    name: manifest.name,
                    kind: kind_str(manifest.media_type).to_owned(),
                }),
                Ok(_) => None,
                Err(error) => {
                    eprintln!("cannot classify {}: {error}", package.display());
                    Some(ConsentRequest {
                        path: package.display().to_string(),
                        name: package
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("пакет")
                            .to_owned(),
                        kind: "unknown".to_owned(),
                    })
                }
            };
            if let Some(request) = consent {
                let _ = app.emit("wpk-consent", request);
                continue;
            }
            match library.import(&package) {
                Ok(item) => {
                    println!("imported {} from {}", item.name, package.display());
                    imported = true;
                }
                Err(error) => eprintln!("failed to import {}: {error}", package.display()),
            }
        }
        if imported {
            let _ = app.emit("library-changed", ());
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must come first: a second launch (e.g. double-clicking a .wpk)
        // hands its arguments to the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            import_packages(
                app.clone(),
                packages_in(argv.into_iter().map(std::ffi::OsString::from)),
            );
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            daemon_connect,
            list_monitors,
            daemon_status,
            play,
            stop,
            pause,
            resume,
            set_volume,
            set_quality,
            get_autostart,
            set_autostart,
            get_battery_policy,
            set_battery_policy,
            library_list,
            library_import,
            library_remove,
            library_preview,
            library_export,
            inspect_package,
            import_bundled_sample,
            diagnostics::run_diagnostics,
            set_playlist,
            clear_playlist,
            playlist_next,
            gallery::gallery_fetch_catalog,
            gallery::gallery_download,
            gallery::gallery_apply_revocations,
        ])
        .setup(|app| {
            // Double-clicking a .wpk should land here (per-user, no admin).
            if let Err(error) = assoc::register() {
                eprintln!("wpk association not registered: {error}");
            }
            import_packages(app.handle().clone(), packages_in(std::env::args_os()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_code_bearing_packages_require_consent() {
        // web / 3D run code on the desktop -> must ask the user first.
        assert!(needs_consent(wpk::MediaType::Web));
        assert!(needs_consent(wpk::MediaType::Model3d));
        // plain media is inert -> import silently.
        assert!(!needs_consent(wpk::MediaType::Video));
        assert!(!needs_consent(wpk::MediaType::Image));
    }
}
