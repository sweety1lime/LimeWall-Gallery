mod assoc;
mod daemon_client;
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

#[tauri::command]
fn daemon_status() -> Result<Vec<ipc::SessionStatus>, String> {
    match daemon_client::request(&daemon_client::endpoint(), ipc::Command::Status)? {
        ipc::ResponseData::Status { sessions } => Ok(sessions),
        other => Err(format!("unexpected daemon response: {other:?}")),
    }
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

async fn blocking<T: Send + 'static>(
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        ])
        .setup(|_app| {
            // Double-clicking a .wpk should land here (per-user, no admin).
            if let Err(error) = assoc::register() {
                eprintln!("wpk association not registered: {error}");
            }
            // Packages passed on the command line (double-click) import now;
            // the panel picks them up with its next library refresh.
            let packages: Vec<std::path::PathBuf> = std::env::args_os()
                .skip(1)
                .map(std::path::PathBuf::from)
                .filter(|path| {
                    path.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("wpk"))
                        && path.is_file()
                })
                .collect();
            if !packages.is_empty() {
                tauri::async_runtime::spawn_blocking(move || {
                    let library = match library::Library::default_location() {
                        Ok(library) => library,
                        Err(error) => {
                            eprintln!("library unavailable: {error}");
                            return;
                        }
                    };
                    for package in packages {
                        match library.import(&package) {
                            Ok(item) => {
                                println!("imported {} from {}", item.name, package.display())
                            }
                            Err(error) => {
                                eprintln!("failed to import {}: {error}", package.display());
                            }
                        }
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
