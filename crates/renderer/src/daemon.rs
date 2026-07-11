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

/// Name of the Run-key value used for autostart.
const AUTOSTART_APP: &str = "LimeWall";

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

    let (message_tx, message_rx) = mpsc::channel::<Message>();
    let request_tx = message_tx.clone();
    std::thread::Builder::new()
        .name("ipc-accept".into())
        .spawn(move || accept_loop(&server, &request_tx))
        .context("failed to spawn IPC accept thread")?;

    // The tray belongs to the daemon so it works while the UI is closed.
    // Headless operation (e.g. CI) is fine — just log and continue.
    let message_tx_watcher = message_tx.clone();
    let _tray = match platform::tray::spawn("LimeWall", move |event| {
        let _ = message_tx.send(Message::Tray(event));
    }) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("tray disabled: {error}");
            None
        }
    };

    // Politeness: pause on fullscreen apps, lock, dark display, battery.
    let watcher_tx = message_tx_watcher;
    let _watcher = match platform::watcher::spawn(move |event| {
        let _ = watcher_tx.send(Message::Activity(event));
    }) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("activity watcher disabled: {error}");
            None
        }
    };

    let mut state = DaemonState {
        host,
        api: None,
        sessions: HashMap::new(),
        state_path,
        fullscreen_monitors: Vec::new(),
        politeness: Politeness::default(),
        battery_policy: ipc::BatteryPolicy::Pause,
        battery_eco_active: false,
    };
    // Wallpapers applied before the last shutdown come back on their own;
    // clients connecting meanwhile just queue in the request channel.
    state.restore_state();
    // Ends when every sender is gone (accept loop died) or on shutdown.
    for message in message_rx {
        match message {
            Message::Request(envelope) => {
                let shutdown = matches!(envelope.request.command, ipc::Command::Shutdown);
                let response = state.handle(envelope.request);
                let ok = matches!(response.body, ipc::ResponseBody::Success { .. });
                let _ = envelope.reply.send(response);
                if shutdown && ok {
                    break;
                }
            }
            Message::Tray(event) => {
                if state.handle_tray(event) {
                    break;
                }
            }
            Message::Activity(event) => state.handle_activity(event),
        }
    }
    // Shutdown intentionally leaves the state file alone: these wallpapers
    // are meant to come back on the next start.
    state.stop_all();
    println!("renderer daemon stopped");
    Ok(())
}

/// Everything the daemon thread reacts to.
enum Message {
    Request(Envelope),
    Tray(platform::tray::TrayEvent),
    Activity(platform::watcher::ActivityEvent),
}

/// %APPDATA%/LimeWall/wallpapers.json (shared convention with the UI library
/// living next to it).
fn default_state_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("LimeWall").join("wallpapers.json"))
}

/// One decoded request plus the channel its response must go back through.
struct Envelope {
    request: ipc::Request,
    reply: mpsc::Sender<ipc::Response>,
}

fn accept_loop(server: &ipc::LocalServer, requests: &mpsc::Sender<Message>) {
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
fn handle_connection(mut stream: ipc::LocalStream, requests: &mpsc::Sender<Message>) {
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
    let envelope = Message::Request(Envelope {
        request,
        reply: reply_tx,
    });
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
    /// Pause requested by the user; persisted.
    user_paused: bool,
    /// Pause currently applied to the player (user intent or politeness).
    effective_paused: bool,
    /// Source size probed at load, for shader decisions on quality switches.
    width: i64,
    height: i64,
    monitor: platform::MonitorInfo,
}

/// System conditions that pause playback regardless of user intent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Politeness {
    session_locked: bool,
    display_off: bool,
    on_battery: bool,
}

impl Politeness {
    /// System-wide reasons to pause, independent of any single monitor.
    fn global_pause(&self, battery_policy: ipc::BatteryPolicy) -> bool {
        self.session_locked
            || self.display_off
            || (self.on_battery && battery_policy == ipc::BatteryPolicy::Pause)
    }

    /// Whether sessions should run in the temporary battery Eco profile.
    fn wants_eco(&self, battery_policy: ipc::BatteryPolicy) -> bool {
        self.on_battery && battery_policy == ipc::BatteryPolicy::Eco
    }
}

/// Effective pause for one monitor: the user's own pause, a fullscreen app on
/// that monitor, or any system-wide reason. Pure so it can be tested directly.
fn desired_pause(user_paused: bool, fullscreen: bool, global_pause: bool) -> bool {
    user_paused || fullscreen || global_pause
}

struct DaemonState {
    host: Box<dyn platform::WallpaperHost>,
    /// Loaded lazily on the first play request.
    api: Option<Arc<mpv::Api>>,
    sessions: HashMap<ipc::MonitorId, Session>,
    /// Where applied wallpapers persist across daemon restarts.
    state_path: Option<PathBuf>,
    /// Monitor device names covered by a fullscreen foreground window.
    fullscreen_monitors: Vec<String>,
    politeness: Politeness,
    battery_policy: ipc::BatteryPolicy,
    /// Sessions currently downgraded to Eco because of the battery policy.
    battery_eco_active: bool,
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
    /// Battery policy; absent in files from older builds.
    #[serde(default = "default_battery_policy")]
    on_battery: ipc::BatteryPolicy,
    wallpapers: Vec<PersistedSession>,
}

fn default_battery_policy() -> ipc::BatteryPolicy {
    ipc::BatteryPolicy::Pause
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
                | ipc::Command::SetBatteryPolicy { .. }
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
            ipc::Command::GetAutostart => platform::autostart_enabled(AUTOSTART_APP)
                .map(|enabled| ipc::ResponseData::Autostart { enabled })
                .map_err(internal),
            ipc::Command::SetAutostart { enabled } => self.set_autostart(enabled),
            ipc::Command::GetBatteryPolicy => Ok(ipc::ResponseData::BatteryPolicy {
                policy: self.battery_policy,
            }),
            ipc::Command::SetBatteryPolicy { policy } => {
                self.battery_policy = policy;
                self.apply_politeness();
                Ok(ipc::ResponseData::Acknowledged {
                    status: format!("battery policy: {policy:?}").to_lowercase(),
                })
            }
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

    /// Registers or removes the daemon in the per-user Run key.
    fn set_autostart(&self, enabled: bool) -> CmdResult {
        let command = if enabled {
            let exe = std::env::current_exe()
                .map_err(|error| internal(format!("cannot resolve own path: {error}")))?;
            Some(format!("\"{}\" serve", exe.display()))
        } else {
            None
        };
        platform::set_autostart(AUTOSTART_APP, command.as_deref()).map_err(internal)?;
        Ok(ipc::ResponseData::Acknowledged {
            status: if enabled {
                "autostart enabled"
            } else {
                "autostart disabled"
            }
            .into(),
        })
    }

    /// Applies a system activity change to every session.
    fn handle_activity(&mut self, event: platform::watcher::ActivityEvent) {
        use platform::watcher::ActivityEvent;
        match event {
            ActivityEvent::Fullscreen(monitors) => {
                if !monitors.is_empty() {
                    println!("fullscreen app detected on {}", monitors.join(", "));
                } else if !self.fullscreen_monitors.is_empty() {
                    println!("fullscreen app gone");
                }
                self.fullscreen_monitors = monitors;
            }
            ActivityEvent::Battery(on_battery) => {
                println!(
                    "power source: {}",
                    if on_battery { "battery" } else { "AC" }
                );
                self.politeness.on_battery = on_battery;
            }
            ActivityEvent::SessionLocked(locked) => {
                println!("session {}", if locked { "locked" } else { "unlocked" });
                self.politeness.session_locked = locked;
            }
            ActivityEvent::DisplayOff(off) => {
                println!("display {}", if off { "off" } else { "on" });
                self.politeness.display_off = off;
            }
        }
        self.apply_politeness();
    }

    /// Reconciles every player's pause/quality with user intent and the
    /// current system conditions.
    fn apply_politeness(&mut self) {
        let global_pause = self.politeness.global_pause(self.battery_policy);
        for session in self.sessions.values_mut() {
            let fullscreen = self.fullscreen_monitors.contains(&session.monitor.name);
            let desired = desired_pause(session.user_paused, fullscreen, global_pause);
            if desired != session.effective_paused {
                match session.player.set_property_bool("pause", desired) {
                    Ok(()) => {
                        session.effective_paused = desired;
                        println!(
                            "monitor {}: {} ({})",
                            session.monitor.id,
                            if desired { "paused" } else { "resumed" },
                            pause_reason(session.user_paused, global_pause, fullscreen)
                        );
                    }
                    Err(error) => eprintln!(
                        "failed to toggle pause on monitor {}: {error}",
                        session.monitor.id
                    ),
                }
            }
        }

        // Battery Eco: a temporary downgrade, session.quality keeps the
        // user's choice for persistence and for the way back.
        let want_eco = self.politeness.wants_eco(self.battery_policy);
        if want_eco != self.battery_eco_active {
            for session in self.sessions.values() {
                let (quality, anime4k) = if want_eco {
                    (playback::Quality::Eco, false)
                } else {
                    (session.quality.into(), session.anime4k)
                };
                if let Err(error) = playback::set_quality(
                    &session.player,
                    quality,
                    anime4k,
                    session.width,
                    session.height,
                    &session.monitor,
                ) {
                    eprintln!(
                        "failed to switch quality on monitor {}: {error}",
                        session.monitor.id
                    );
                }
            }
            println!(
                "battery eco {}",
                if want_eco { "engaged" } else { "released" }
            );
            self.battery_eco_active = want_eco;
        }
    }

    /// Returns `true` when the daemon should exit (tray Quit).
    fn handle_tray(&mut self, event: platform::tray::TrayEvent) -> bool {
        use platform::tray::TrayEvent;
        match event {
            TrayEvent::PauseAll => {
                if let Err((_, message)) = self.set_paused(None, true) {
                    eprintln!("tray pause failed: {message}");
                } else {
                    self.save_state();
                }
            }
            TrayEvent::ResumeAll => {
                if let Err((_, message)) = self.set_paused(None, false) {
                    eprintln!("tray resume failed: {message}");
                } else {
                    self.save_state();
                }
            }
            TrayEvent::OpenUi => {
                if let Err(error) = spawn_ui() {
                    eprintln!("failed to open the UI: {error}");
                }
            }
            TrayEvent::Quit => {
                println!("quit requested from the tray");
                return true;
            }
        }
        false
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
                paused: session.user_paused,
            })
            .collect();
        wallpapers.sort_by_key(|wallpaper| wallpaper.monitor);
        let state = PersistedState {
            version: STATE_VERSION,
            on_battery: self.battery_policy,
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
        self.battery_policy = persisted.on_battery;
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
                // Effective state: politeness pauses show up here too.
                state: if session.effective_paused {
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
            "playing {} on monitor {monitor} ({}, {}x{}, hwdec {}); {}",
            path.display(),
            started.codec,
            started.width,
            started.height,
            started.hwdec,
            started.shaders
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
                user_paused: false,
                effective_paused: false,
                width: started.width,
                height: started.height,
                monitor: info,
            },
        );
        // A wallpaper applied while e.g. a fullscreen game runs (or on
        // battery with the Eco policy) must obey the rules immediately;
        // clearing the flag makes the eco branch re-apply to the newcomer.
        self.battery_eco_active = false;
        self.apply_politeness();
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
                session.user_paused = paused;
            }
        }
        // The player state follows through the same reconciliation as the
        // politeness rules, so a resume during e.g. a fullscreen game stays
        // paused until the game ends.
        self.apply_politeness();
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
        let shaders = playback::set_quality(
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
        let status = format!("monitor {monitor}: {shaders}");
        println!("{status}");
        Ok(ipc::ResponseData::Acknowledged { status })
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

fn pause_reason(user: bool, global: bool, fullscreen: bool) -> &'static str {
    if user {
        "user"
    } else if fullscreen {
        "fullscreen app"
    } else if global {
        "system state"
    } else {
        "conditions cleared"
    }
}

/// Starts the control UI detached: explicit override, next to the renderer
/// executable (bundled install), then the development build location.
fn spawn_ui() -> Result<(), String> {
    let exe_name = if cfg!(windows) { "ui.exe" } else { "ui" };
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("LIMEWALL_UI") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Ok(renderer) = std::env::current_exe()
        && let Some(dir) = renderer.parent()
    {
        candidates.push(dir.join(exe_name));
        candidates.push(dir.join("LimeWall.exe"));
        // Development layout: target/debug next to the UI workspace build.
        candidates.push(
            dir.join("../../apps/ui/src-tauri/target/debug")
                .join(exe_name),
        );
    }
    let ui = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or("UI executable not found (set LIMEWALL_UI)")?;
    std::process::Command::new(&ui)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|error| format!("failed to start {}: {error}", ui.display()))
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
            on_battery: ipc::BatteryPolicy::Eco,
            wallpapers: vec![entry(0, r"\\.\DISPLAY1")],
        };
        let json = serde_json::to_vec(&state).expect("serialize");
        let back: PersistedState = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.version, STATE_VERSION);
        assert_eq!(back.on_battery, ipc::BatteryPolicy::Eco);
        assert_eq!(back.wallpapers.len(), 1);
        assert_eq!(back.wallpapers[0].monitor_name, r"\\.\DISPLAY1");
    }

    #[test]
    fn state_files_without_battery_policy_default_to_pause() {
        let json = format!(r#"{{"version":{STATE_VERSION},"wallpapers":[]}}"#);
        let back: PersistedState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.on_battery, ipc::BatteryPolicy::Pause);
    }

    fn politeness(locked: bool, display_off: bool, on_battery: bool) -> Politeness {
        Politeness {
            session_locked: locked,
            display_off,
            on_battery,
        }
    }

    #[test]
    fn nothing_pauses_when_idle_and_unpaused() {
        let calm = politeness(false, false, false);
        assert!(!calm.global_pause(ipc::BatteryPolicy::Pause));
        assert!(!desired_pause(false, false, false));
    }

    #[test]
    fn user_pause_and_fullscreen_pause_a_single_monitor() {
        // No system-wide reason, but the user paused this monitor.
        assert!(desired_pause(true, false, false));
        // A fullscreen app on this monitor pauses only it.
        assert!(desired_pause(false, true, false));
    }

    #[test]
    fn lock_and_dark_display_pause_regardless_of_policy() {
        for policy in [
            ipc::BatteryPolicy::Pause,
            ipc::BatteryPolicy::Eco,
            ipc::BatteryPolicy::Keep,
        ] {
            assert!(
                politeness(true, false, false).global_pause(policy),
                "lock, {policy:?}"
            );
            assert!(
                politeness(false, true, false).global_pause(policy),
                "dark, {policy:?}"
            );
        }
    }

    #[test]
    fn battery_policy_controls_the_battery_pause() {
        let on_battery = politeness(false, false, true);
        assert!(on_battery.global_pause(ipc::BatteryPolicy::Pause));
        assert!(!on_battery.global_pause(ipc::BatteryPolicy::Eco));
        assert!(!on_battery.global_pause(ipc::BatteryPolicy::Keep));
        // Eco is a downgrade, not a pause.
        assert!(on_battery.wants_eco(ipc::BatteryPolicy::Eco));
        assert!(!on_battery.wants_eco(ipc::BatteryPolicy::Pause));
        assert!(!politeness(false, false, false).wants_eco(ipc::BatteryPolicy::Eco));
    }

    #[test]
    fn resume_stays_paused_while_a_condition_holds() {
        // The user resumed (user_paused = false) but a fullscreen game runs:
        // the monitor must remain paused until the game ends.
        let global = politeness(false, false, false).global_pause(ipc::BatteryPolicy::Pause);
        assert!(desired_pause(false, true, global));
    }
}
