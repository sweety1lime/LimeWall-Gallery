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
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use anyhow::Context;

use crate::playback;

/// Upper bound on connection threads; above it clients get a busy error.
const MAX_CONNECTIONS: usize = 16;

pub fn run(endpoint: Option<&str>) -> anyhow::Result<()> {
    let endpoint = endpoint
        .map(str::to_owned)
        .unwrap_or_else(ipc::default_endpoint);
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
    };
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
    state.stop_all();
    println!("renderer daemon stopped");
    Ok(())
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
        match result {
            Ok(data) => ipc::Response::success(id, data),
            Err((code, message)) => ipc::Response::error(id, code, message),
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
