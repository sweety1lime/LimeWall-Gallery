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
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::playback;
use crate::playlist;

/// Upper bound on connection threads; above it clients get a busy error.
const MAX_CONNECTIONS: usize = 16;

const STATE_VERSION: u32 = 1;

/// Name of the Run-key value used for autostart.
const AUTOSTART_APP: &str = "LimeWall";

/// Windowless daemon binary, the one autostart must launch.
const DAEMON_EXE: &str = if cfg!(windows) {
    "limewall-daemon.exe"
} else {
    "limewall-daemon"
};

/// The CLI binary, which serves the daemon too — but with a console attached.
const RENDERER_EXE: &str = if cfg!(windows) {
    "renderer.exe"
} else {
    "renderer"
};

/// Command autostart should run, given the path of the executable doing the
/// registering: the windowless daemon sitting next to it, so that no console
/// window shows up at logon. Falls back to `renderer serve` when that binary is
/// missing, which costs the user a console window but still restores wallpapers.
fn autostart_command_for(exe: &Path) -> String {
    match exe.parent().map(|dir| dir.join(DAEMON_EXE)) {
        Some(daemon) if daemon.is_file() => format!("\"{}\"", daemon.display()),
        _ => format!("\"{}\" serve", exe.display()),
    }
}

/// The executable a `"<path>" serve` registration launches, or `None` for
/// anything else — including a registration already pointing at the windowless
/// daemon, which takes no arguments.
fn console_autostart_target(command: &str) -> Option<&str> {
    let rest = command.trim().strip_prefix('"')?;
    let (path, args) = rest.split_once('"')?;
    args.trim().eq_ignore_ascii_case("serve").then_some(path)
}

/// Whether a console-based registration should be replaced by this install.
///
/// It should when it belongs to this install, and also when the executable it
/// names is gone: a portable copy unpacked into a new folder leaves the Run key
/// pointing at the deleted one, where it starts nothing at all. A registration
/// naming another install that still exists is left to that install.
fn should_migrate_autostart(command: &str, exe: &Path) -> bool {
    let Some(target) = console_autostart_target(command) else {
        return false;
    };
    let target = Path::new(target);
    let same_install = match (target.parent(), exe.parent()) {
        (Some(theirs), Some(ours)) => {
            theirs.as_os_str().eq_ignore_ascii_case(ours.as_os_str())
                && target.file_name() == Some(RENDERER_EXE.as_ref())
        }
        _ => false,
    };
    same_install || !target.exists()
}

/// Repoints an existing autostart registration at the windowless daemon.
///
/// Beta testers already have `renderer.exe serve` in their Run key, which greets
/// them with a console window at every logon; they must not have to toggle the
/// setting off and on to get the fix.
fn migrate_autostart() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let current = match platform::autostart_command(AUTOSTART_APP) {
        Ok(Some(command)) => command,
        _ => return,
    };
    if !should_migrate_autostart(&current, &exe) {
        return;
    }
    let command = autostart_command_for(&exe);
    if command == current {
        return;
    }
    match platform::set_autostart(AUTOSTART_APP, Some(&command)) {
        Ok(()) => println!("autostart migrated to the windowless daemon: {command}"),
        Err(error) => eprintln!("failed to migrate autostart: {error}"),
    }
}

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
    migrate_autostart();

    let (message_tx, message_rx) = mpsc::channel::<Message>();
    let request_tx = message_tx.clone();
    #[cfg(windows)]
    let watchdog_tx = message_tx.clone();
    std::thread::Builder::new()
        .name("ipc-accept".into())
        .spawn(move || accept_loop(&server, &request_tx))
        .context("failed to spawn IPC accept thread")?;

    // The tray belongs to the daemon so it works while the UI is closed.
    // Headless operation (e.g. CI) is fine — just log and continue.
    let message_tx_watcher = message_tx.clone();
    let tray = match platform::tray::spawn("LimeWall", move |event| {
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

    // Resource watchdog: pause a wallpaper whose process tree pegs the CPU.
    #[cfg(windows)]
    if let Err(error) = std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || run_watchdog(watchdog_tx))
    {
        eprintln!("resource watchdog disabled: {error}");
    }

    let mut state = DaemonState {
        host,
        api: None,
        sessions: HashMap::new(),
        state_path,
        fullscreen_monitors: Vec::new(),
        politeness: Politeness::default(),
        battery_policy: ipc::BatteryPolicy::Pause,
        battery_eco_active: false,
        last_cpu: None,
        playlists: HashMap::new(),
    };
    // Wallpapers applied before the last shutdown come back on their own;
    // clients connecting meanwhile just queue in the request channel.
    state.restore_state();
    // Ends on shutdown or channel disconnect. Playlist rotations fire on their
    // own schedule via the recv timeout — no dedicated thread. `tick_playlists`
    // runs after every wake (not just on timeout), because the watchdog's 4 s
    // CpuSample messages would otherwise always preempt the timeout arm.
    let mut last_tip = String::new();
    loop {
        let timeout = state.next_wakeup();
        match message_rx.recv_timeout(timeout) {
            Ok(Message::Request(envelope)) => {
                let shutdown = matches!(envelope.request.command, ipc::Command::Shutdown);
                let response = state.handle(envelope.request);
                let ok = matches!(response.body, ipc::ResponseBody::Success { .. });
                let _ = envelope.reply.send(response);
                if shutdown && ok {
                    break;
                }
            }
            Ok(Message::Tray(event)) => {
                if state.handle_tray(event) {
                    break;
                }
            }
            Ok(Message::Activity(event)) => state.handle_activity(event),
            Ok(Message::ResourcePressure(percent)) => state.handle_resource_pressure(percent),
            Ok(Message::CpuSample(percent)) => {
                state.last_cpu = Some((percent, Instant::now()));
                if let Some(tray) = &tray {
                    // Surface load without opening the window; only touch the
                    // icon when the text actually changes (not every 4 s).
                    let tip = state.tray_tooltip(percent);
                    if tip != last_tip {
                        tray.set_tooltip(&tip);
                        last_tip = tip;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        state.tick_playlists(Instant::now());
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
    /// Sustained CPU pressure from the wallpaper stack (percent of the whole
    /// machine), raised by the resource watchdog thread.
    ResourcePressure(f32),
    /// Latest wallpaper-stack CPU reading (percent of the whole machine), for
    /// display; sent by the watchdog every sample.
    CpuSample(f32),
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

/// What is drawing into a session's surface.
enum SessionKind {
    /// libmpv video/image/GIF. `width`/`height` are the source size, for
    /// shader decisions on quality switches.
    Mpv {
        player: mpv::Player,
        width: i64,
        height: i64,
    },
    /// A WebView2 page (HTML / three.js). Lives in the platform host, keyed by
    /// the surface; pause is a webview suspend, not an mpv property.
    Web,
}

struct Session {
    surface: platform::SurfaceHandle,
    kind: SessionKind,
    path: PathBuf,
    quality: ipc::Quality,
    volume: u8,
    anime4k: bool,
    /// Pause requested by the user; persisted.
    user_paused: bool,
    /// Pause currently applied (user intent or politeness).
    effective_paused: bool,
    /// Latched by the resource watchdog; cleared when the user resumes.
    overbudget: bool,
    monitor: platform::MonitorInfo,
}

/// Media whose entry file ends in one of these is played as a web surface.
fn is_web_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
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
/// that monitor, any system-wide reason, or the resource watchdog latching a
/// runaway wallpaper. Pure so it can be tested directly.
fn desired_pause(
    user_paused: bool,
    fullscreen: bool,
    global_pause: bool,
    overbudget: bool,
) -> bool {
    user_paused || fullscreen || global_pause || overbudget
}

/// Resource watchdog tuning. A wallpaper is decoration; sustained use of a
/// meaningful slice of the whole machine means a runaway or hostile page. These
/// defaults are deliberately conservative to avoid false pauses and want live
/// tuning against real hardware (see docs/research/security-model.md).
#[cfg(any(windows, test))]
const CPU_BUDGET_PERCENT: f32 = 25.0; // percent of total machine capacity
#[cfg(any(windows, test))]
const BREACH_SAMPLES: u32 = 5; // consecutive over-budget samples before acting
#[cfg(windows)]
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(4);

/// A CPU reading older than this is stale (watchdog thread wedged / stopped) and
/// is not reported to clients. Kept cross-platform (`WATCHDOG_INTERVAL` is
/// Windows-only).
const CPU_SAMPLE_FRESH: Duration = Duration::from_secs(15);

/// The freshest CPU sample still considered live, rounded to 0.1%. Pure for tests.
fn fresh_cpu(sample: Option<(f32, Instant)>, now: Instant) -> Option<f32> {
    sample
        .filter(|(_, at)| now.duration_since(*at) < CPU_SAMPLE_FRESH)
        .map(|(percent, _)| (percent * 10.0).round() / 10.0)
}

/// Turns a stream of CPU% samples into a single "pause it" signal, with
/// hysteresis so a paused-then-recovered wallpaper does not flap.
#[cfg(any(windows, test))]
#[derive(Default)]
struct BreachDetector {
    over: u32,
    fired: bool,
}

#[cfg(any(windows, test))]
impl BreachDetector {
    /// Feeds one CPU% sample; returns true exactly once when a sustained breach
    /// first appears, and rearms only after CPU falls back under budget.
    fn observe(&mut self, percent: f32) -> bool {
        if percent >= CPU_BUDGET_PERCENT {
            self.over = self.over.saturating_add(1);
            if self.over >= BREACH_SAMPLES && !self.fired {
                self.fired = true;
                return true;
            }
        } else {
            self.over = 0;
            self.fired = false;
        }
        false
    }
}

/// Samples the wallpaper stack's CPU and asks the daemon to pause a wallpaper
/// on sustained pressure. Ends when the daemon's receiver is gone.
#[cfg(windows)]
fn run_watchdog(tx: mpsc::Sender<Message>) {
    let mut sampler = platform::resources::StackSampler::new();
    let mut detector = BreachDetector::default();
    loop {
        std::thread::sleep(WATCHDOG_INTERVAL);
        let Some(percent) = sampler.sample() else {
            continue;
        };
        // Feed the live reading for display, then the breach detector.
        if tx.send(Message::CpuSample(percent)).is_err() {
            break; // daemon gone
        }
        if detector.observe(percent) && tx.send(Message::ResourcePressure(percent)).is_err() {
            break;
        }
    }
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
    /// Latest wallpaper-stack CPU reading from the watchdog, for `status`.
    last_cpu: Option<(f32, Instant)>,
    /// Per-monitor auto-rotating playlists.
    playlists: HashMap<ipc::MonitorId, PlaylistState>,
}

/// A monitor's running playlist: its items, timing and rotation state.
struct PlaylistState {
    /// Device name, for persistence across topology changes.
    monitor_name: String,
    items: Vec<PathBuf>,
    interval: Duration,
    /// Kept alongside `interval` for persistence and the status summary.
    interval_minutes: u32,
    shuffle: bool,
    rotation: playlist::Rotation,
    next_rotate_at: Instant,
    quality: ipc::Quality,
    volume: u8,
    anime4k: bool,
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
    /// Playlists; absent in files from builds before the feature (kept
    /// separate from `wallpapers` so an older daemon still restores the current
    /// wallpaper as a static one — a graceful downgrade).
    #[serde(default)]
    playlists: Vec<PersistedPlaylist>,
}

/// One persisted playlist entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPlaylist {
    monitor: ipc::MonitorId,
    monitor_name: String,
    items: Vec<PathBuf>,
    interval_minutes: u32,
    shuffle: bool,
    /// Item index showing at save time, resumed on restore.
    position: usize,
    quality: ipc::Quality,
    volume: u8,
    anime4k: bool,
}

fn default_battery_policy() -> ipc::BatteryPolicy {
    ipc::BatteryPolicy::Pause
}

/// A time-derived non-zero seed for playlist shuffles.
fn seed_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64 | 1)
        .unwrap_or(0x1234_5678)
}

/// Clamps the sleep before the next wakeup: never busy-loop (≥100 ms), never
/// oversleep a schedule (≤1 h). Pure for tests.
fn clamp_wakeup(nearest: Option<Duration>) -> Duration {
    const IDLE: Duration = Duration::from_secs(3600);
    const MIN: Duration = Duration::from_millis(100);
    nearest.map(|d| d.clamp(MIN, IDLE)).unwrap_or(IDLE)
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
                | ipc::Command::SetPlaylist { .. }
                | ipc::Command::ClearPlaylist { .. }
                | ipc::Command::PlaylistNext { .. }
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
            } => {
                // A manual play cancels the monitor's playlist.
                self.playlists.remove(&monitor);
                self.play(monitor, path, quality, volume, anime4k)
            }
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
            ipc::Command::SetPlaylist {
                monitor,
                items,
                interval_minutes,
                shuffle,
                quality,
                volume,
                anime4k,
            } => self.set_playlist(
                monitor,
                items,
                interval_minutes,
                shuffle,
                quality,
                volume,
                anime4k,
            ),
            ipc::Command::ClearPlaylist { monitor } => self.clear_playlist(monitor),
            ipc::Command::PlaylistNext { monitor } => self.playlist_next(monitor),
            ipc::Command::GetPlaylist { monitor } => Ok(self.get_playlist(monitor)),
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
            Some(autostart_command_for(&exe))
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

    /// Reconciles every session's pause/quality with user intent and the
    /// current system conditions.
    fn apply_politeness(&mut self) {
        let global_pause = self.politeness.global_pause(self.battery_policy);
        // Web suspends need &self.host, which conflicts with iterating
        // sessions mutably — collect them and apply after the loop.
        let mut web_toggles: Vec<(platform::SurfaceHandle, bool, usize)> = Vec::new();
        for session in self.sessions.values_mut() {
            let fullscreen = self.fullscreen_monitors.contains(&session.monitor.name);
            let desired = desired_pause(
                session.user_paused,
                fullscreen,
                global_pause,
                session.overbudget,
            );
            if desired == session.effective_paused {
                continue;
            }
            let reason = pause_reason(
                session.user_paused,
                global_pause,
                fullscreen,
                session.overbudget,
            );
            match &session.kind {
                SessionKind::Mpv { player, .. } => match player.set_property_bool("pause", desired)
                {
                    Ok(()) => {
                        session.effective_paused = desired;
                        println!(
                            "monitor {}: {} ({reason})",
                            session.monitor.id,
                            if desired { "paused" } else { "resumed" },
                        );
                    }
                    Err(error) => eprintln!(
                        "failed to toggle pause on monitor {}: {error}",
                        session.monitor.id
                    ),
                },
                SessionKind::Web => {
                    session.effective_paused = desired;
                    web_toggles.push((session.surface, desired, session.monitor.id));
                }
            }
        }
        for (surface, suspended, monitor) in web_toggles {
            if let Err(error) = self.host.set_web_suspended(surface, suspended) {
                eprintln!("failed to suspend web surface on monitor {monitor}: {error}");
            } else {
                println!(
                    "monitor {monitor}: web {}",
                    if suspended { "suspended" } else { "resumed" }
                );
            }
        }

        // Battery Eco: a temporary downgrade for mpv sessions; web pages are
        // unaffected. session.quality keeps the user's choice for the way back.
        let want_eco = self.politeness.wants_eco(self.battery_policy);
        if want_eco != self.battery_eco_active {
            for session in self.sessions.values() {
                let SessionKind::Mpv {
                    player,
                    width,
                    height,
                } = &session.kind
                else {
                    continue;
                };
                let (quality, anime4k) = if want_eco {
                    (playback::Quality::Eco, false)
                } else {
                    (session.quality.into(), session.anime4k)
                };
                if let Err(error) = playback::set_quality(
                    player,
                    quality,
                    anime4k,
                    *width,
                    *height,
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

    /// Reacts to sustained CPU pressure by latch-pausing the likely culprit.
    /// Web wallpapers run untrusted code and are the usual offender, so they
    /// are paused first; mpv sessions only if no web wallpaper is live. The
    /// pause is latched (`overbudget`) and cleared only when the user resumes,
    /// so a runaway page cannot immediately un-pause itself.
    fn handle_resource_pressure(&mut self, percent: f32) {
        let has_live_web = self
            .sessions
            .values()
            .any(|s| !s.effective_paused && matches!(s.kind, SessionKind::Web));
        let mut hit = Vec::new();
        for session in self.sessions.values_mut() {
            if session.effective_paused || session.overbudget {
                continue;
            }
            let is_web = matches!(session.kind, SessionKind::Web);
            if has_live_web && !is_web {
                continue; // blame the code-driven wallpaper first
            }
            session.overbudget = true;
            hit.push(session.monitor.id);
        }
        if hit.is_empty() {
            return; // nothing running to blame — likely another app's load
        }
        eprintln!(
            "resource guard: wallpaper stack at {percent:.0}% CPU — pausing monitor(s) {hit:?}"
        );
        self.apply_politeness();
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
            TrayEvent::NextWallpaper => {
                // Best-effort: silent when no playlist is running.
                if self.playlist_next(None).is_ok() {
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
        let mut playlists: Vec<PersistedPlaylist> = self
            .playlists
            .iter()
            .map(|(monitor, ps)| PersistedPlaylist {
                monitor: *monitor,
                monitor_name: ps.monitor_name.clone(),
                items: ps.items.clone(),
                interval_minutes: ps.interval_minutes,
                shuffle: ps.shuffle,
                position: ps.rotation.current(),
                quality: ps.quality,
                volume: ps.volume,
                anime4k: ps.anime4k,
            })
            .collect();
        playlists.sort_by_key(|playlist| playlist.monitor);
        let state = PersistedState {
            version: STATE_VERSION,
            on_battery: self.battery_policy,
            wallpapers,
            playlists,
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
        // Playlists first: they own their monitor, so the matching plain
        // wallpaper entry is skipped below to avoid a double play.
        let mut playlist_monitors = std::collections::HashSet::new();
        for entry in persisted.playlists {
            if entry.items.is_empty() {
                continue;
            }
            let Some(monitor) = resolve_monitor(entry.monitor, &entry.monitor_name, &monitors)
            else {
                eprintln!(
                    "restore skipped playlist: monitor {} ({}) is not present",
                    entry.monitor, entry.monitor_name
                );
                continue;
            };
            let interval = Duration::from_secs(u64::from(entry.interval_minutes) * 60);
            let mut rotation =
                playlist::Rotation::new(entry.items.len(), entry.shuffle, seed_now());
            rotation.seek_to_item(entry.position);
            let mut ps = PlaylistState {
                monitor_name: entry.monitor_name,
                items: entry.items,
                interval,
                interval_minutes: entry.interval_minutes,
                shuffle: entry.shuffle,
                rotation,
                next_rotate_at: Instant::now() + interval,
                quality: entry.quality,
                volume: entry.volume,
                anime4k: entry.anime4k,
            };
            self.play_playlist_current(monitor, &mut ps);
            self.playlists.insert(monitor, ps);
            playlist_monitors.insert(monitor);
            println!("restored playlist on monitor {monitor}");
        }
        for entry in persisted.wallpapers {
            let Some(monitor) = resolve_restore_monitor(&entry, &monitors) else {
                eprintln!(
                    "restore skipped: monitor {} ({}) is not present",
                    entry.monitor, entry.monitor_name
                );
                continue;
            };
            if playlist_monitors.contains(&monitor) {
                continue; // handled by its playlist
            }
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

    /// The most-specific reason a session is paused, for the UI. Mirrors the
    /// precedence of [`pause_reason`]; `None` when the session is playing.
    fn paused_reason(&self, session: &Session) -> Option<ipc::PausedReason> {
        if !session.effective_paused {
            return None;
        }
        if session.user_paused {
            Some(ipc::PausedReason::User)
        } else if session.overbudget {
            Some(ipc::PausedReason::Resources)
        } else if self.fullscreen_monitors.contains(&session.monitor.name) {
            Some(ipc::PausedReason::Game)
        } else if self.politeness.session_locked {
            Some(ipc::PausedReason::Lock)
        } else if self.politeness.display_off {
            Some(ipc::PausedReason::DisplayOff)
        } else if self.politeness.on_battery && self.battery_policy == ipc::BatteryPolicy::Pause {
            Some(ipc::PausedReason::Battery)
        } else {
            None
        }
    }

    // ---- playlists ------------------------------------------------------

    /// How long to sleep before the next event or the earliest playlist tick.
    fn next_wakeup(&self) -> Duration {
        let now = Instant::now();
        let nearest = self
            .playlists
            .values()
            .map(|ps| ps.next_rotate_at.saturating_duration_since(now))
            .min();
        clamp_wakeup(nearest)
    }

    /// Plays the rotation's current item on `monitor`, advancing past any
    /// missing files (at most one full cycle). Returns whether anything played.
    /// `ps` is owned by the caller, not the map, to avoid aliasing `self`.
    fn play_playlist_current(&mut self, monitor: ipc::MonitorId, ps: &mut PlaylistState) -> bool {
        if ps.items.is_empty() {
            return false;
        }
        for _ in 0..ps.items.len() {
            let path = ps.items[ps.rotation.current()].clone();
            if path.is_file() {
                match self.play(monitor, path, ps.quality, ps.volume, ps.anime4k) {
                    Ok(_) => return true,
                    Err((_, message)) => {
                        eprintln!("playlist play failed on monitor {monitor}: {message}")
                    }
                }
            } else {
                eprintln!("playlist skip missing file: {}", path.display());
            }
            ps.rotation.advance();
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn set_playlist(
        &mut self,
        monitor: ipc::MonitorId,
        items: Vec<PathBuf>,
        interval_minutes: u32,
        shuffle: bool,
        quality: ipc::Quality,
        volume: u8,
        anime4k: bool,
    ) -> CmdResult {
        let name = self
            .host
            .enumerate_monitors()
            .map_err(internal)?
            .into_iter()
            .find(|m| m.id == monitor)
            .ok_or((
                ipc::ErrorCode::MonitorNotFound,
                format!("monitor {monitor} not found"),
            ))?
            .name;
        let interval = Duration::from_secs(u64::from(interval_minutes) * 60);
        let count = items.len();
        let mut ps = PlaylistState {
            monitor_name: name,
            rotation: playlist::Rotation::new(count, shuffle, seed_now()),
            items,
            interval,
            interval_minutes,
            shuffle,
            next_rotate_at: Instant::now() + interval,
            quality,
            volume,
            anime4k,
        };
        let played = self.play_playlist_current(monitor, &mut ps);
        self.playlists.insert(monitor, ps);
        Ok(ipc::ResponseData::Acknowledged {
            status: if played {
                format!("playlist of {count} on monitor {monitor}")
            } else {
                format!("playlist set but nothing playable on monitor {monitor}")
            },
        })
    }

    fn clear_playlist(&mut self, monitor: Option<ipc::MonitorId>) -> CmdResult {
        match monitor {
            Some(monitor) => {
                self.playlists.remove(&monitor);
            }
            None => self.playlists.clear(),
        }
        Ok(ipc::ResponseData::Acknowledged {
            status: "playlist cleared".into(),
        })
    }

    fn playlist_next(&mut self, monitor: Option<ipc::MonitorId>) -> CmdResult {
        let targets: Vec<ipc::MonitorId> = match monitor {
            Some(monitor) => vec![monitor],
            None => self.playlists.keys().copied().collect(),
        };
        let mut advanced = false;
        for monitor in targets {
            if let Some(mut ps) = self.playlists.remove(&monitor) {
                ps.rotation.advance();
                self.play_playlist_current(monitor, &mut ps);
                ps.next_rotate_at = Instant::now() + ps.interval;
                self.playlists.insert(monitor, ps);
                advanced = true;
            }
        }
        if advanced {
            Ok(ipc::ResponseData::Acknowledged {
                status: "advanced".into(),
            })
        } else {
            Err((
                ipc::ErrorCode::PlaybackFailed,
                "no playlist on that monitor".into(),
            ))
        }
    }

    fn get_playlist(&self, monitor: ipc::MonitorId) -> ipc::ResponseData {
        let playlist = self.playlists.get(&monitor).map(|ps| ipc::PlaylistInfo {
            monitor,
            items: ps.items.clone(),
            interval_minutes: ps.interval_minutes,
            shuffle: ps.shuffle,
            position: ps.rotation.current(),
        });
        ipc::ResponseData::Playlist { playlist }
    }

    /// Fires any playlists whose interval elapsed. Rotation is deferred while a
    /// monitor's wallpaper is paused (game / lock / battery) so it never churns
    /// behind a fullscreen app.
    fn tick_playlists(&mut self, now: Instant) {
        let due: Vec<ipc::MonitorId> = self
            .playlists
            .iter()
            .filter(|(_, ps)| now >= ps.next_rotate_at)
            .map(|(monitor, _)| *monitor)
            .collect();
        let mut advanced = false;
        for monitor in due {
            let Some(mut ps) = self.playlists.remove(&monitor) else {
                continue;
            };
            let paused = self
                .sessions
                .get(&monitor)
                .is_some_and(|session| session.effective_paused);
            if paused {
                ps.next_rotate_at = now + ps.interval;
                self.playlists.insert(monitor, ps);
                println!("playlist on monitor {monitor} deferred (paused)");
                continue;
            }
            ps.rotation.advance();
            self.play_playlist_current(monitor, &mut ps);
            ps.next_rotate_at = now + ps.interval;
            self.playlists.insert(monitor, ps);
            advanced = true;
        }
        if advanced {
            self.save_state();
        }
    }

    /// Hover text for the tray icon: wallpaper count and live CPU.
    fn tray_tooltip(&self, percent: f32) -> String {
        let count = self.sessions.len();
        if count == 0 {
            "LimeWall — обои не запущены".to_string()
        } else {
            format!(
                "LimeWall — обоев: {count} · CPU: {}%",
                percent.round() as i64
            )
        }
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
                paused_reason: self.paused_reason(session),
            })
            .collect();
        sessions.sort_by_key(|session| session.monitor);
        let mut playlists: Vec<ipc::PlaylistSummary> = self
            .playlists
            .iter()
            .map(|(monitor, ps)| ipc::PlaylistSummary {
                monitor: *monitor,
                len: ps.items.len(),
                interval_minutes: ps.interval_minutes,
                shuffle: ps.shuffle,
            })
            .collect();
        playlists.sort_by_key(|playlist| playlist.monitor);
        ipc::ResponseData::Status {
            sessions,
            stack_cpu_percent: fresh_cpu(self.last_cpu, Instant::now()),
            playlists,
        }
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
        // Content replacement tears the old session down first; a web and an
        // mpv surface are not interchangeable, so we always recreate.
        if let Some(previous) = self.sessions.remove(&monitor) {
            self.drop_session(previous);
        }

        let status;
        if is_web_path(&path) {
            let root = path.parent().ok_or((
                ipc::ErrorCode::PlaybackFailed,
                "no folder for web entry".into(),
            ))?;
            let entry = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or((ipc::ErrorCode::PlaybackFailed, "bad web entry name".into()))?;
            let surface = self
                .host
                .create_web_surface(monitor, root, entry)
                .map_err(|error| (ipc::ErrorCode::PlaybackFailed, error.to_string()))?;
            self.sessions.insert(
                monitor,
                Session {
                    surface,
                    kind: SessionKind::Web,
                    path: path.clone(),
                    quality,
                    volume,
                    anime4k,
                    user_paused: false,
                    effective_paused: false,
                    overbudget: false,
                    monitor: info,
                },
            );
            status = format!("web wallpaper {} on monitor {monitor}", path.display());
            println!("{status}");
        } else {
            let api = match &self.api {
                Some(api) => Arc::clone(api),
                None => {
                    let api = playback::load_libmpv()
                        .map_err(|error| (ipc::ErrorCode::Internal, format!("{error:#}")))?;
                    self.api = Some(Arc::clone(&api));
                    api
                }
            };
            let surface = self.host.create_surface(monitor).map_err(internal)?;
            let wid = match self.host.surface_native_handle(surface) {
                Ok(wid) => wid,
                Err(error) => {
                    let _ = self.host.destroy_surface(surface);
                    return Err(internal(error));
                }
            };
            let started = match playback::start_player(
                api,
                wid,
                &path,
                quality.into(),
                volume,
                anime4k,
                &info,
            ) {
                Ok(started) => started,
                Err(error) => {
                    let _ = self.host.destroy_surface(surface);
                    return Err((ipc::ErrorCode::PlaybackFailed, format!("{error:#}")));
                }
            };
            status = format!(
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
                    kind: SessionKind::Mpv {
                        player: started.player,
                        width: started.width,
                        height: started.height,
                    },
                    path,
                    quality,
                    volume,
                    anime4k,
                    user_paused: false,
                    effective_paused: false,
                    overbudget: false,
                    monitor: info,
                },
            );
        }
        // A wallpaper applied while e.g. a fullscreen game runs (or on
        // battery with the Eco policy) must obey the rules immediately;
        // clearing the flag makes the eco branch re-apply to the newcomer.
        self.battery_eco_active = false;
        self.apply_politeness();
        Ok(ipc::ResponseData::Acknowledged { status })
    }

    /// Tears a session down: an mpv player must stop rendering into the
    /// surface window before the window is destroyed; a web surface's webview
    /// is dropped by destroy_surface.
    fn drop_session(&mut self, session: Session) {
        let Session { kind, surface, .. } = session;
        drop(kind);
        if let Err(error) = self.host.destroy_surface(surface) {
            eprintln!("failed to destroy surface: {error}");
        }
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
            // Stopping a wallpaper also ends its playlist.
            self.playlists.remove(monitor);
            if let Some(session) = self.sessions.remove(monitor) {
                self.drop_session(session);
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
                // Resuming is an explicit "bring it back" — clear the watchdog
                // latch so a once-runaway wallpaper can play again.
                if !paused {
                    session.overbudget = false;
                }
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
        let SessionKind::Mpv { player, .. } = &session.kind else {
            return Err((
                ipc::ErrorCode::InvalidRequest,
                "web wallpapers have no volume".into(),
            ));
        };
        playback::set_volume(player, volume)
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
        let SessionKind::Mpv {
            player,
            width,
            height,
        } = &session.kind
        else {
            return Err((
                ipc::ErrorCode::InvalidRequest,
                "web wallpapers have no quality profile".into(),
            ));
        };
        let shaders = playback::set_quality(
            player,
            quality.into(),
            anime4k,
            *width,
            *height,
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
        for session in self.sessions.drain().map(|(_, s)| s).collect::<Vec<_>>() {
            self.drop_session(session);
        }
    }
}

fn no_session(monitor: ipc::MonitorId) -> (ipc::ErrorCode, String) {
    (
        ipc::ErrorCode::PlaybackFailed,
        format!("no active session on monitor {monitor}"),
    )
}

fn pause_reason(user: bool, global: bool, fullscreen: bool, overbudget: bool) -> &'static str {
    if user {
        "user"
    } else if overbudget {
        "resource guard"
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
    let mut command = std::process::Command::new(&ui);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console flash if the UI happens to be a console-subsystem build.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
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
    resolve_monitor(entry.monitor, &entry.monitor_name, monitors)
}

/// Monitor for a persisted id/name: device name first (indices shuffle when the
/// topology changes), then the stored index.
fn resolve_monitor(
    id: ipc::MonitorId,
    name: &str,
    monitors: &[platform::MonitorInfo],
) -> Option<ipc::MonitorId> {
    monitors
        .iter()
        .find(|monitor| monitor.name == name)
        .or_else(|| monitors.iter().find(|monitor| monitor.id == id))
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
    fn autostart_falls_back_to_the_cli_when_the_daemon_binary_is_missing() {
        let exe = PathBuf::from("nowhere").join(RENDERER_EXE);
        let command = autostart_command_for(&exe);
        assert!(command.ends_with(" serve"), "{command}");
    }

    #[test]
    fn console_autostart_of_our_own_install_is_migrated() {
        let dir = PathBuf::from(r"C:\Program Files\LimeWall");
        let exe = dir.join(DAEMON_EXE);
        let old = format!("\"{}\" serve", dir.join(RENDERER_EXE).display());
        assert!(should_migrate_autostart(&old, &exe));
        // The registry hands the path back as it was written.
        assert!(should_migrate_autostart(&old.to_uppercase(), &exe));
    }

    #[test]
    fn autostart_of_a_vanished_install_is_taken_over() {
        // The tester unpacked the new build elsewhere and dropped the old
        // folder: that registration starts nothing, so it is ours to fix.
        let exe = PathBuf::from(r"C:\Portable\LimeWall-new").join(DAEMON_EXE);
        let gone = r#""C:\Portable\LimeWall-old\renderer.exe" serve"#;
        assert!(should_migrate_autostart(gone, &exe));
    }

    #[test]
    fn autostart_of_a_live_install_elsewhere_is_left_alone() {
        // A copy whose files still exist keeps owning its registration; the
        // test binary stands in for one.
        let live = std::env::current_exe().expect("test binary path");
        let exe = PathBuf::from(r"C:\Program Files\LimeWall").join(DAEMON_EXE);
        let other = format!("\"{}\" serve", live.display());
        assert!(!should_migrate_autostart(&other, &exe));
    }

    #[test]
    fn already_migrated_autostart_is_not_touched() {
        let exe = PathBuf::from(r"C:\Program Files\LimeWall").join(DAEMON_EXE);
        // The windowless daemon takes no arguments: not a `serve` command.
        let migrated = format!("\"{}\"", exe.display());
        assert!(!should_migrate_autostart(&migrated, &exe));
        assert_eq!(console_autostart_target("not even quoted"), None);
    }

    #[test]
    fn persisted_state_round_trips_through_json() {
        let state = PersistedState {
            version: STATE_VERSION,
            on_battery: ipc::BatteryPolicy::Eco,
            wallpapers: vec![entry(0, r"\\.\DISPLAY1")],
            playlists: vec![],
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

    #[test]
    fn state_files_without_playlists_default_to_empty() {
        // A file written before the playlist feature restores with none.
        let json = format!(r#"{{"version":{STATE_VERSION},"wallpapers":[]}}"#);
        let back: PersistedState = serde_json::from_str(&json).expect("deserialize");
        assert!(back.playlists.is_empty());
    }

    #[test]
    fn persisted_playlist_round_trips() {
        let state = PersistedState {
            version: STATE_VERSION,
            on_battery: ipc::BatteryPolicy::Pause,
            wallpapers: vec![],
            playlists: vec![PersistedPlaylist {
                monitor: 0,
                monitor_name: r"\\.\DISPLAY1".into(),
                items: vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")],
                interval_minutes: 15,
                shuffle: true,
                position: 1,
                quality: ipc::Quality::Balanced,
                volume: 0,
                anime4k: false,
            }],
        };
        let json = serde_json::to_vec(&state).expect("serialize");
        let back: PersistedState = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.playlists.len(), 1);
        assert_eq!(back.playlists[0].position, 1);
        assert_eq!(back.playlists[0].items.len(), 2);
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
        assert!(!desired_pause(false, false, false, false));
    }

    #[test]
    fn user_pause_and_fullscreen_pause_a_single_monitor() {
        // No system-wide reason, but the user paused this monitor.
        assert!(desired_pause(true, false, false, false));
        // A fullscreen app on this monitor pauses only it.
        assert!(desired_pause(false, true, false, false));
    }

    #[test]
    fn resource_guard_pauses_and_names_its_reason() {
        // The watchdog latch pauses even when nothing else would.
        assert!(desired_pause(false, false, false, true));
        assert_eq!(pause_reason(false, false, false, true), "resource guard");
        // The user's own pause still takes precedence in the reason.
        assert_eq!(pause_reason(true, false, false, true), "user");
    }

    #[test]
    fn breach_detector_fires_once_after_sustained_pressure() {
        let mut d = BreachDetector::default();
        // Below budget never fires.
        for _ in 0..10 {
            assert!(!d.observe(CPU_BUDGET_PERCENT - 1.0));
        }
        // BREACH_SAMPLES consecutive over-budget reads fire exactly once.
        for _ in 0..BREACH_SAMPLES - 1 {
            assert!(!d.observe(CPU_BUDGET_PERCENT + 5.0));
        }
        assert!(d.observe(CPU_BUDGET_PERCENT + 5.0));
        assert!(!d.observe(CPU_BUDGET_PERCENT + 5.0)); // latched, no repeat
        // Cooling down rearms; a fresh sustained breach fires again.
        assert!(!d.observe(0.0));
        for _ in 0..BREACH_SAMPLES - 1 {
            assert!(!d.observe(CPU_BUDGET_PERCENT + 5.0));
        }
        assert!(d.observe(CPU_BUDGET_PERCENT + 5.0));
    }

    #[test]
    fn clamp_wakeup_never_busy_loops_or_oversleeps() {
        // No playlists → idle hour.
        assert_eq!(clamp_wakeup(None), Duration::from_secs(3600));
        // Already-due (0) is floored to 100 ms, not a hot spin.
        assert_eq!(
            clamp_wakeup(Some(Duration::ZERO)),
            Duration::from_millis(100)
        );
        // A normal interval passes through.
        assert_eq!(
            clamp_wakeup(Some(Duration::from_secs(50))),
            Duration::from_secs(50)
        );
        // Oversized is capped to the idle hour.
        assert_eq!(
            clamp_wakeup(Some(Duration::from_secs(9999))),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn fresh_cpu_reports_recent_rounded_and_drops_stale() {
        let now = Instant::now();
        // A recent sample is reported, rounded to 0.1%.
        assert_eq!(fresh_cpu(Some((12.34, now)), now), Some(12.3));
        // A stale sample (watchdog wedged) is dropped.
        let old = now - (CPU_SAMPLE_FRESH + Duration::from_secs(1));
        assert_eq!(fresh_cpu(Some((12.3, old)), now), None);
        // No sample yet.
        assert_eq!(fresh_cpu(None, now), None);
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
        assert!(desired_pause(false, true, global, false));
    }
}
