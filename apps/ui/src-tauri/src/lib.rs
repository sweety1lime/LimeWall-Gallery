mod daemon_client;

use std::path::PathBuf;

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
