//! Finds or starts the renderer daemon and forwards IPC requests to it.
//!
//! The daemon owns the wallpapers and must outlive this UI process, so it is
//! spawned fully detached (docs/research/phase2-architecture.md).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Endpoint the UI talks to; overridable for tests and parallel setups.
pub fn endpoint() -> String {
    std::env::var("LIMEWALL_ENDPOINT").unwrap_or_else(|_| ipc::default_endpoint())
}

/// Sends one request and unwraps the protocol envelope.
pub fn request(endpoint: &str, command: ipc::Command) -> Result<ipc::ResponseData, String> {
    let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let response = ipc::send_request(endpoint, &ipc::Request::new(id, command))
        .map_err(|error| error.to_string())?;
    match response.body {
        ipc::ResponseBody::Success { result } => Ok(result),
        ipc::ResponseBody::Error { error } => Err(format!("{:?}: {}", error.code, error.message)),
    }
}

/// Pings the daemon; when it is not running, starts one detached and waits
/// for it to come up. Returns the daemon version.
pub fn ensure_daemon(endpoint: &str) -> Result<String, String> {
    if let Ok(ipc::ResponseData::Pong { daemon_version }) = request(endpoint, ipc::Command::Ping) {
        return Ok(daemon_version);
    }
    let renderer = renderer_path().ok_or_else(|| {
        "renderer executable not found: set LIMEWALL_RENDERER or build the workspace".to_owned()
    })?;
    spawn_detached(&renderer, endpoint)
        .map_err(|error| format!("failed to start {}: {error}", renderer.display()))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match request(endpoint, ipc::Command::Ping) {
            Ok(ipc::ResponseData::Pong { daemon_version }) => return Ok(daemon_version),
            _ if Instant::now() >= deadline => {
                return Err("renderer daemon did not answer within 10 seconds".into());
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Renderer executable lookup: explicit override, next to the UI executable
/// (bundled install), then the development workspace target directory.
fn renderer_path() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "renderer.exe"
    } else {
        "renderer"
    };
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("LIMEWALL_RENDERER") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Ok(ui_exe) = std::env::current_exe()
        && let Some(dir) = ui_exe.parent()
    {
        candidates.push(dir.join(exe_name));
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for profile in ["debug", "release"] {
        candidates.push(workspace.join("target").join(profile).join(exe_name));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Starts `renderer serve` with no console and no parent-child lifetime tie.
/// Daemon output lands in the user data directory so problems can be
/// diagnosed after the fact; a fresh file per daemon start.
fn spawn_detached(renderer: &std::path::Path, endpoint: &str) -> std::io::Result<()> {
    let mut command = std::process::Command::new(renderer);
    command
        .arg("serve")
        .arg("--endpoint")
        .arg(endpoint)
        .stdin(std::process::Stdio::null());
    // Parallel setups and tests must not touch the real wallpaper state.
    if let Ok(state) = std::env::var("LIMEWALL_STATE") {
        command.arg("--state").arg(state);
    }
    match daemon_log_file() {
        Some(log) => {
            let errors = log.try_clone()?;
            command.stdout(log).stderr(errors);
        }
        None => {
            command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(drop)
}

fn daemon_log_file() -> Option<std::fs::File> {
    let dir = dirs::data_dir()?.join("LimeWall");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::File::create(dir.join("daemon.log")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end: no daemon on a fresh endpoint -> ensure_daemon starts the
    /// real renderer, answers ping, and shuts down cleanly.
    #[test]
    fn ensure_daemon_starts_and_answers() {
        let Some(_) = renderer_path() else {
            eprintln!("skipped: renderer executable is not built");
            return;
        };
        // Keep the spawned daemon away from the user's wallpaper state.
        let state = std::env::temp_dir().join(format!(
            "limewall-ui-test-state-{}.json",
            std::process::id()
        ));
        // SAFETY: no other thread in this test binary touches this variable.
        unsafe {
            std::env::set_var("LIMEWALL_STATE", &state);
        }
        let endpoint = format!("limewall-ui-test-{}.sock", std::process::id());

        let version = ensure_daemon(&endpoint).expect("daemon should start and answer");
        assert!(!version.is_empty());

        // Second call must reuse the running daemon, not spawn another one.
        let again = ensure_daemon(&endpoint).expect("daemon should still answer");
        assert_eq!(version, again);

        let result = request(&endpoint, ipc::Command::Shutdown);
        assert!(result.is_ok(), "shutdown failed: {result:?}");
    }
}
