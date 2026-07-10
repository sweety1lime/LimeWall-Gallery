//! Long-lived renderer daemon: owns wallpaper sessions and serves the local
//! IPC endpoint.
//!
//! Threading model (docs/research/phase2-architecture.md): the accept loop and
//! one thread per connection only read/write frames; decoded requests are
//! forwarded over `mpsc` to the main daemon thread, which exclusively owns the
//! platform host and the mpv players. A stalled client therefore blocks only
//! its own connection thread — required on Windows, where named pipes have no
//! I/O timeouts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::playback;

/// Upper bound on connection threads; above it clients get a busy error.
const MAX_CONNECTIONS: usize = 16;

const STATE_VERSION: u32 = 1;

pub fn run(endpoint: Option<&str>, state_path: Option<&Path>) -> anyhow::Result<()> {
    let endpoint = endpoint
        .map(str::to_owned)
        .unwrap_or_else(ipc::default_endpoint);
    let state_path = state_path
        .map(Path::to_path_buf)
        .or_else(default_state_path);
    let host = platform::create_host().context("failed to initialize wallpaper host")?;
    let server = ipc::LocalServer::bind(&endpoint)
        .with_context(|| format!("failed to bind renderer endpoint {endpoint:?}"))?;
    println!("renderer daemon listening at {endpoint}");

    let (request_tx, request_rx) = mpsc::channel::<Envelope>();
    std::thread::Builder::new()
        .name("ipc-accept".into())
        .spawn(move || accept_loop(&server, &request_tx))
        .context("failed to spawn IPC accept thread")?;

    let mut state = DaemonState {
        host,
        api: None,
        sessions: HashMap::new(),
        state_path,
    };
    // Wallpapers applied before the last shutdown come back on their own;
    // clients connecting meanwhile just queue in the request channel.
    state.restore_state();
    // Ends when every sender is gone (accept loop died) or on shutdown.
    for envelope in request_rx {
        let shutdown = matches!(envelope.request.command, ipc::Command::Shutdown);
        let response = state.handle(envelope.request);
        let ok = matches!(response.body, ipc::ResponseBody::Success { .. });
        let _ = envelope.reply.send(response);
        if shutdown && ok {
            break;
        }
    }
    // Shutdown intentionally leaves the state file alone: these wallpapers
    // are meant to come back on the next start.
    state.stop_all();
    println!("renderer daemon stopped");
    Ok(())
}

/// %APPDATA%/LiveWall/wallpapers.json (shared convention with the UI library
/// living next to it).
fn default_state_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("LiveWall").join("wallpapers.json"))
}

/// One decoded request plus the channel its response must go back through.
struct Envelope {
    request: ipc::Request,
    reply: mpsc::Sender<ipc::Response>,
}

fn accept_loop(server: &ipc::LocalServer, requests: &mpsc::Sender<Envelope>) {
    let active = Arc::new(AtomicUsize::new(0));
    let mut consecutive_errors = 0u32;
    loop {
        let mut stream = match server.accept() {
            Ok(stream) => {
                consecutive_errors = 0;
                stream
            }
            Err(error) => {
                eprintln!("failed to accept IPC client: {error}");
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    // Listener is broken; dropping the sender ends the daemon
                    // loop instead of spinning forever.
                    eprintln!("giving up on the IPC listener");
                    return;
                }
                continue;
            }
        };
        if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            let response = ipc::Response::error(
                0,
                ipc::ErrorCode::Internal,
                "too many concurrent IPC connections",
            );
            let _ = ipc::write_frame(&mut stream, &response);
            continue;
        }
        active.fetch_add(1, Ordering::AcqRel);
        let requests = requests.clone();
        let connection_active = Arc::clone(&active);
        let spawned = std::thread::Builder::new()
            .name("ipc-connection".into())
            .spawn(move || {
                handle_connection(stream, &requests);
                connection_active.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(error) = spawned {
            eprintln!("failed to spawn IPC connection thread: {error}");
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Reads one request, routes it to the daemon thread and writes the response.
fn handle_connection(mut stream: ipc::LocalStream, requests: &mpsc::Sender<Envelope>) {
    let request: ipc::Request = match ipc::read_frame(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let response = ipc::Response::error(
                0,
                ipc::ErrorCode::InvalidRequest,
                format!("invalid IPC frame: {error}"),
            );
            let _ = ipc::write_frame(&mut stream, &response);
            eprintln!("rejected invalid IPC frame: {error}");
            return;
        }
    };
    let id = request.id;
    let (reply_tx, reply_rx) = mpsc::channel();
    let envelope = Envelope {
        request,
        reply: reply_tx,
    };
    let response = if requests.send(envelope).is_ok() {
        reply_rx.recv().unwrap_or_else(|_| {
            ipc::Response::error(id, ipc::ErrorCode::Internal, "daemon dropped the request")
        })
    } else {
        ipc::Response::error(id, ipc::ErrorCode::Internal, "daemon is shutting down")
    };
    if let Err(error) = ipc::write_frame(&mut stream, &response) {
        eprintln!("failed to write IPC response: {error}");
    }
}

// ---------------------------------------------------------------------------
// State and command handling (daemon thread only)
// ---------------------------------------------------------------------------

/// Command outcome: response data or an error code with a message.
type CmdResult = Result<ipc::ResponseData, (ipc::ErrorCode, String)>;

struct Session {
    surface: platform::SurfaceHandle,
    player: mpv::Player,
    path: PathBuf,
    quality: ipc::Quality,
    volume: u8,
    anime4k: bool,
    paused: bool,
    /// Source size probed at load, for shader decisions on quality switches.
    width: i64,
    height: i64,
    monitor: platform::MonitorInfo,
}

struct DaemonState {
    host: Box<dyn platform::WallpaperHost>,
    /// Loaded lazily on the first play request.
    api: Option<Arc<mpv::Api>>,
    sessions: HashMap<ipc::MonitorId, Session>,
    /// Where applied wallpapers persist across daemon restarts.
    state_path: Option<PathBuf>,
}

/// One entry of the persisted wallpaper state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSession {
    monitor: ipc::MonitorId,
    /// Device name; preferred over the index when the topology changed.
    monitor_name: String,
    path: PathBuf,
    quality: ipc::Quality,
    volume: u8,
    anime4k: bool,
    paused: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    version: u32,
    wallpapers: Vec<PersistedSession>,
}

impl DaemonState {
    fn handle(&mut self, request: ipc::Request) -> ipc::Response {
        if let Err(error) = request.validate() {
            let code = if matches!(error, ipc::ValidationError::UnsupportedVersion { .. }) {
                ipc::ErrorCode::UnsupportedVersion
            } else {
                ipc::ErrorCode::InvalidRequest
            };
            return ipc::Response::error(request.id, code, error.to_string());
        }

        let id = request.id;
        let mutates_wallpapers = matches!(
            request.command,
            ipc::Command::Play { .. }
                | ipc::Command::Stop { .. }
                | ipc::Command::Pause { .. }
                | ipc::Command::Resume { .. }
                | ipc::Command::SetVolume { .. }
                | ipc::Command::SetQuality { .. }
        );
        let result = match request.command {
            ipc::Command::Ping => Ok(ipc::ResponseData::Pong {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            }),
            ipc::Command::ListMonitors => self.list_monitors(),
            ipc::Command::Status => Ok(self.status()),
            ipc::Command::Play {
                monitor,
                path,
                quality,
                volume,
                anime4k,
            } => self.play(monitor, path, quality, volume, anime4k),
            ipc::Command::Stop { monitor } => self.stop(monitor),
            ipc::Command::Pause { monitor } => self.set_paused(monitor, true),
            ipc::Command::Resume { monitor } => self.set_paused(monitor, false),
            ipc::Command::SetVolume { monitor, volume } => self.set_volume(monitor, volume),
            ipc::Command::SetQuality {
                monitor,
                quality,
                anime4k,
            } => self.set_quality(monitor, quality, anime4k),
            ipc::Command::Shutdown => Ok(ipc::ResponseData::Acknowledged {
                status: "shutting_down".into(),
            }),
        };
        if mutates_wallpapers && result.is_ok() {
            self.save_state();
        }
        match result {
            Ok(data) => ipc::Response::success(id, data),
            Err((code, message)) => ipc::Response::error(id, code, message),
        }
    }

    /// Writes the wallpaper set to disk (atomically) so it survives restarts.
    fn save_state(&self) {
        let Some(path) = &self.state_path else { return };
        let mut wallpapers: Vec<PersistedSession> = self
            .sessions
            .values()
            .map(|session| PersistedSession {
                monitor: session.monitor.id,
                monitor_name: session.monitor.name.clone(),
                path: session.path.clone(),
                quality: session.quality,
                volume: session.volume,
                anime4k: session.anime4k,
                paused: session.paused,
            })
            .collect();
        wallpapers.sort_by_key(|wallpaper| wallpaper.monitor);
        let state = PersistedState {
            version: STATE_VERSION,
            wallpapers,
        };
        if let Err(error) = write_state(path, &state) {
            eprintln!("failed to save state to {}: {error}", path.display());
        }
    }

    /// Brings back wallpapers persisted by a previous daemon run. Entries for
    /// missing monitors or media are skipped but stay in the file, so they
    /// return when the monitor or drive does.
    fn restore_state(&mut self) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                eprintln!("failed to read state file {}: {error}", path.display());
                return;
            }
        };
        let persisted: PersistedState = match serde_json::from_slice(&bytes) {
            Ok(persisted) => persisted,
            Err(error) => {
                eprintln!("state file {} is corrupted: {error}", path.display());
                return;
            }
        };
        if persisted.version != STATE_VERSION {
            eprintln!(
                "state file {} has version {}, expected {STATE_VERSION}; skipping restore",
                path.display(),
                persisted.version
            );
            return;
        }
        let monitors = match self.host.enumerate_monitors() {
            Ok(monitors) => monitors,
            Err(error) => {
                eprintln!("restore skipped: cannot enumerate monitors: {error}");
                return;
            }
        };
        for entry in persisted.wallpapers {
            let Some(monitor) = resolve_restore_monitor(&entry, &monitors) else {
                eprintln!(
                    "restore skipped: monitor {} ({}) is not present",
                    entry.monitor, entry.monitor_name
                );
                continue;
            };
            if !entry.path.is_file() {
                eprintln!("restore skipped: media missing: {}", entry.path.display());
                continue;
            }
            match self.play(
                monitor,
                entry.path.clone(),
                entry.quality,
                entry.volume,
                entry.anime4k,
            ) {
                Ok(_) => {
                    if entry.paused {
                        let _ = self.set_paused(Some(monitor), true);
                    }
                    println!("restored wallpaper on monitor {monitor}");
                }
                Err((_, message)) => {
                    eprintln!("restore failed on monitor {monitor}: {message}");
                }
            }
        }
    }

    fn list_monitors(&self) -> CmdResult {
        let monitors = self.host.enumerate_monitors().map_err(internal)?;
        Ok(ipc::ResponseData::Monitors {
            monitors: monitors.into_iter().map(monitor_to_ipc).collect(),
        })
    }

    fn status(&self) -> ipc::ResponseData {
        let mut sessions: Vec<ipc::SessionStatus> = self
            .sessions
            .values()
            .map(|session| ipc::SessionStatus {
                monitor: session.monitor.id,
                state: if session.paused {
                    ipc::PlaybackState::Paused
                } else {
                    ipc::PlaybackState::Playing
                },
                path: Some(session.path.clone()),
                quality: session.quality,
                volume: session.volume,
                anime4k: session.anime4k,
            })
            .collect();
        sessions.sort_by_key(|session| session.monitor);
        ipc::ResponseData::Status { sessions }
    }

    fn play(
        &mut self,
        monitor: ipc::MonitorId,
        path: PathBuf,
        quality: ipc::Quality,
        volume: u8,
        anime4k: bool,
    ) -> CmdResult {
        if !path.is_file() {
            return Err((
                ipc::ErrorCode::MediaNotFound,
                format!("media file not found: {}", path.display()),
            ));
        }
        let info = self
            .host
            .enumerate_monitors()
            .map_err(internal)?
            .into_iter()
            .find(|m| m.id == monitor)
            .ok_or((
                ipc::ErrorCode::MonitorNotFound,
                format!("monitor {monitor} not found"),
            ))?;
        let api = match &self.api {
            Some(api) => Arc::clone(api),
            None => {
                let api = playback::load_libmpv()
                    .map_err(|error| (ipc::ErrorCode::Internal, format!("{error:#}")))?;
                self.api = Some(Arc::clone(&api));
                api
            }
        };

        // Replacing content on a monitor reuses its surface: the old player
        // must go first (it renders into that window), then the new one binds.
        let surface = match self.sessions.remove(&monitor) {
            Some(previous) => {
                drop(previous.player);
                previous.surface
            }
            None => self.host.create_surface(monitor).map_err(internal)?,
        };
        let wid = match self.host.surface_native_handle(surface) {
            Ok(wid) => wid,
            Err(error) => {
                let _ = self.host.destroy_surface(surface);
                return Err(internal(error));
            }
        };
        let started =
            match playback::start_player(api, wid, &path, quality.into(), volume, anime4k, &info) {
                Ok(started) => started,
                Err(error) => {
                    let _ = self.host.destroy_surface(surface);
                    return Err((ipc::ErrorCode::PlaybackFailed, format!("{error:#}")));
                }
            };
        let status = format!(
            "playing {} on monitor {monitor} ({}, {}x{}, hwdec {})",
            path.display(),
            started.codec,
            started.width,
            started.height,
            started.hwdec
        );
        println!("{status}");
        self.sessions.insert(
            monitor,
            Session {
                surface,
                player: started.player,
                path,
                quality,
                volume,
                anime4k,
                paused: false,
                width: started.width,
                height: started.height,
                monitor: info,
            },
        );
        Ok(ipc::ResponseData::Acknowledged { status })
    }

    fn stop(&mut self, monitor: Option<ipc::MonitorId>) -> CmdResult {
        let targets: Vec<ipc::MonitorId> = match monitor {
            Some(monitor) => {
                self.session(monitor)?;
                vec![monitor]
            }
            None => self.sessions.keys().copied().collect(),
        };
        if targets.is_empty() {
            return Ok(ipc::ResponseData::Acknowledged {
                status: "nothing to stop".into(),
            });
        }
        for monitor in &targets {
            if let Some(session) = self.sessions.remove(monitor) {
                // The player renders into the surface window: shut it down
                // before destroying the window.
                drop(session.player);
                if let Err(error) = self.host.destroy_surface(session.surface) {
                    eprintln!("failed to destroy surface on monitor {monitor}: {error}");
                }
                println!("stopped playback on monitor {monitor}");
            }
        }
        Ok(ipc::ResponseData::Acknowledged {
            status: format!("stopped on {} monitor(s)", targets.len()),
        })
    }

    fn set_paused(&mut self, monitor: Option<ipc::MonitorId>, paused: bool) -> CmdResult {
        let targets: Vec<ipc::MonitorId> = match monitor {
            Some(monitor) => {
                self.session(monitor)?;
                vec![monitor]
            }
            None => self.sessions.keys().copied().collect(),
        };
        if targets.is_empty() {
            return Err((ipc::ErrorCode::PlaybackFailed, "no active sessions".into()));
        }
        for monitor in &targets {
            if let Some(session) = self.sessions.get_mut(monitor) {
                session
                    .player
                    .set_property_bool("pause", paused)
                    .map_err(playback_err)?;
                session.paused = paused;
            }
        }
        Ok(ipc::ResponseData::Acknowledged {
            status: if paused { "paused" } else { "resumed" }.into(),
        })
    }

    fn set_volume(&mut self, monitor: ipc::MonitorId, volume: u8) -> CmdResult {
        let session = self.session_mut(monitor)?;
        playback::set_volume(&session.player, volume)
            .map_err(|error| (ipc::ErrorCode::PlaybackFailed, format!("{error:#}")))?;
        session.volume = volume;
        Ok(ipc::ResponseData::Acknowledged {
            status: format!("volume {volume} on monitor {monitor}"),
        })
    }

    fn set_quality(
        &mut self,
        monitor: ipc::MonitorId,
        quality: ipc::Quality,
        anime4k: bool,
    ) -> CmdResult {
        let session = self.session_mut(monitor)?;
        playback::set_quality(
            &session.player,
            quality.into(),
            anime4k,
            session.width,
            session.height,
            &session.monitor,
        )
        .map_err(|error| (ipc::ErrorCode::PlaybackFailed, format!("{error:#}")))?;
        session.quality = quality;
        session.anime4k = anime4k;
        Ok(ipc::ResponseData::Acknowledged {
            status: format!("quality updated on monitor {monitor}"),
        })
    }

    fn session(&self, monitor: ipc::MonitorId) -> Result<&Session, (ipc::ErrorCode, String)> {
        self.sessions
            .get(&monitor)
            .ok_or_else(|| no_session(monitor))
    }

    fn session_mut(
        &mut self,
        monitor: ipc::MonitorId,
    ) -> Result<&mut Session, (ipc::ErrorCode, String)> {
        self.sessions
            .get_mut(&monitor)
            .ok_or_else(|| no_session(monitor))
    }

    fn stop_all(&mut self) {
        for (_, session) in self.sessions.drain() {
            drop(session.player);
            let _ = self.host.destroy_surface(session.surface);
        }
    }
}

fn no_session(monitor: ipc::MonitorId) -> (ipc::ErrorCode, String) {
    (
        ipc::ErrorCode::PlaybackFailed,
        format!("no active session on monitor {monitor}"),
    )
}

/// Monitor for a persisted entry: device name first (indices shuffle when
/// the display topology changes), then the stored index.
fn resolve_restore_monitor(
    entry: &PersistedSession,
    monitors: &[platform::MonitorInfo],
) -> Option<ipc::MonitorId> {
    monitors
        .iter()
        .find(|monitor| monitor.name == entry.monitor_name)
        .or_else(|| monitors.iter().find(|monitor| monitor.id == entry.monitor))
        .map(|monitor| monitor.id)
}

fn write_state(path: &Path, state: &PersistedState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let json = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    // Write-then-rename keeps the previous state intact if we crash mid-write.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, json).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn internal(error: impl std::fmt::Display) -> (ipc::ErrorCode, String) {
    (ipc::ErrorCode::Internal, error.to_string())
}

fn playback_err(error: impl std::fmt::Display) -> (ipc::ErrorCode, String) {
    (ipc::ErrorCode::PlaybackFailed, error.to_string())
}

pub fn monitor_to_ipc(monitor: platform::MonitorInfo) -> ipc::Monitor {
    ipc::Monitor {
        id: monitor.id,
        name: monitor.name,
        bounds: ipc::Rect {
            x: monitor.bounds.x,
            y: monitor.bounds.y,
            width: monitor.bounds.width,
            height: monitor.bounds.height,
        },
        scale: monitor.scale,
        is_primary: monitor.is_primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: usize, name: &str) -> platform::MonitorInfo {
        platform::MonitorInfo {
            id,
            name: name.into(),
            bounds: platform::Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            scale: 1.0,
            is_primary: id == 0,
        }
    }

    fn entry(monitor: usize, monitor_name: &str) -> PersistedSession {
        PersistedSession {
            monitor,
            monitor_name: monitor_name.into(),
            path: PathBuf::from("wall.mp4"),
            quality: ipc::Quality::Balanced,
            volume: 0,
            anime4k: false,
            paused: false,
        }
    }

    #[test]
    fn restore_prefers_monitor_name_over_index() {
        // The monitor moved from index 1 to 0 (e.g. another display removed).
        let monitors = [monitor(0, r"\\.\DISPLAY2")];
        let resolved = resolve_restore_monitor(&entry(1, r"\\.\DISPLAY2"), &monitors);
        assert_eq!(resolved, Some(0));
    }

    #[test]
    fn restore_falls_back_to_index_when_name_changed() {
        let monitors = [monitor(0, r"\\.\DISPLAY1"), monitor(1, r"\\.\DISPLAY9")];
        let resolved = resolve_restore_monitor(&entry(1, r"\\.\DISPLAY2"), &monitors);
        assert_eq!(resolved, Some(1));
    }

    #[test]
    fn restore_skips_missing_monitors() {
        let monitors = [monitor(0, r"\\.\DISPLAY1")];
        let resolved = resolve_restore_monitor(&entry(3, r"\\.\DISPLAY4"), &monitors);
        assert_eq!(resolved, None);
    }

    #[test]
    fn persisted_state_round_trips_through_json() {
        let state = PersistedState {
            version: STATE_VERSION,
            wallpapers: vec![entry(0, r"\\.\DISPLAY1")],
        };
        let json = serde_json::to_vec(&state).expect("serialize");
        let back: PersistedState = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.version, STATE_VERSION);
        assert_eq!(back.wallpapers.len(), 1);
        assert_eq!(back.wallpapers[0].monitor_name, r"\\.\DISPLAY1");
    }
}
