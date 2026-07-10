mod daemon;
mod playback;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use clap::{Parser, Subcommand};

use playback::Quality;

#[derive(Parser)]
#[command(name = "renderer", about = "LimeWall wallpaper renderer", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fill a monitor with a solid color behind the desktop icons (phase 0).
    TestSurface {
        /// Monitor index as listed by the platform backend.
        #[arg(long, default_value_t = 0)]
        monitor: platform::MonitorId,
        /// Fill color as "#RRGGBB".
        #[arg(long, default_value = "#336699", value_parser = parse_color)]
        color: Rgb,
    },
    /// Play a video, GIF or image behind the desktop icons.
    Play {
        /// Path to the media file.
        file: PathBuf,
        /// Monitor index as listed by the platform backend.
        #[arg(long, default_value_t = 0)]
        monitor: platform::MonitorId,
        /// Upscaling/quality profile.
        #[arg(long, value_enum, default_value_t = Quality::Balanced)]
        quality: Quality,
        /// Initial volume 0-100; 0 keeps the player muted (the default).
        #[arg(long, default_value_t = 0)]
        volume: u8,
        /// Use the Anime4K Mode B (Fast) shader chain while upscaling.
        #[arg(long)]
        anime4k: bool,
    },
    /// Run the long-lived local IPC daemon (phase 2).
    Serve {
        /// Override the per-user local socket name (primarily for tests).
        #[arg(long)]
        endpoint: Option<String>,
        /// Override the wallpaper state file (primarily for tests);
        /// defaults to the per-user data directory.
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Send one control request to a running renderer daemon.
    Ctl {
        /// Override the per-user local socket name (primarily for tests).
        #[arg(long)]
        endpoint: Option<String>,
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Check that the daemon is alive.
    Ping,
    /// List monitors as seen by the daemon.
    ListMonitors,
    /// Show active wallpaper sessions.
    Status,
    /// Stop all sessions and exit the daemon.
    Shutdown,
    /// Start or replace playback on a monitor.
    Play {
        /// Path to the media file.
        file: PathBuf,
        #[arg(long, default_value_t = 0)]
        monitor: platform::MonitorId,
        #[arg(long, value_enum, default_value_t = Quality::Balanced)]
        quality: Quality,
        /// Volume 0-100; 0 keeps the player muted.
        #[arg(long, default_value_t = 0)]
        volume: u8,
        /// Use the Anime4K Mode B (Fast) shader chain while upscaling.
        #[arg(long)]
        anime4k: bool,
    },
    /// Stop playback on one monitor, or everywhere.
    Stop {
        #[arg(long)]
        monitor: Option<platform::MonitorId>,
    },
    /// Pause decoding on one monitor, or everywhere.
    Pause {
        #[arg(long)]
        monitor: Option<platform::MonitorId>,
    },
    /// Resume decoding on one monitor, or everywhere.
    Resume {
        #[arg(long)]
        monitor: Option<platform::MonitorId>,
    },
    /// Change volume of a running session.
    Volume {
        /// Volume 0-100; 0 mutes.
        volume: u8,
        #[arg(long, default_value_t = 0)]
        monitor: platform::MonitorId,
    },
    /// Change the quality profile of a running session.
    Quality {
        /// Upscaling/quality profile.
        #[arg(value_enum)]
        quality: Quality,
        #[arg(long, default_value_t = 0)]
        monitor: platform::MonitorId,
        /// Use the Anime4K Mode B (Fast) shader chain while upscaling.
        #[arg(long)]
        anime4k: bool,
    },
    /// Manage starting the daemon with the user session.
    Autostart {
        #[arg(value_enum)]
        action: AutostartAction,
    },
    /// Manage what playback does on battery power.
    Battery {
        #[arg(value_enum)]
        action: BatteryAction,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum AutostartAction {
    Status,
    On,
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum BatteryAction {
    Status,
    /// Pause playback entirely on battery.
    Pause,
    /// Drop to the Eco profile on battery.
    Eco,
    /// Keep playing as configured.
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

fn parse_color(s: &str) -> Result<Rgb, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 || !hex.is_ascii() {
        return Err(format!("expected \"#RRGGBB\", got {s:?}"));
    }
    let byte = |range| {
        u8::from_str_radix(&hex[range], 16).map_err(|_| format!("invalid hex digits in {s:?}"))
    };
    Ok(Rgb {
        r: byte(0..2)?,
        g: byte(2..4)?,
        b: byte(4..6)?,
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::TestSurface { monitor, color } => test_surface(monitor, color),
        Command::Play {
            file,
            monitor,
            quality,
            volume,
            anime4k,
        } => play(&file, monitor, quality, volume, anime4k),
        Command::Serve { endpoint, state } => daemon::run(endpoint.as_deref(), state.as_deref()),
        Command::Ctl { endpoint, command } => ctl(endpoint.as_deref(), command),
    }
}

// ---------------------------------------------------------------------------
// ctl client
// ---------------------------------------------------------------------------

fn ctl(endpoint: Option<&str>, command: DaemonCommand) -> anyhow::Result<()> {
    let endpoint = endpoint
        .map(str::to_owned)
        .unwrap_or_else(ipc::default_endpoint);
    let command = match command {
        DaemonCommand::Ping => ipc::Command::Ping,
        DaemonCommand::ListMonitors => ipc::Command::ListMonitors,
        DaemonCommand::Status => ipc::Command::Status,
        DaemonCommand::Shutdown => ipc::Command::Shutdown,
        DaemonCommand::Play {
            file,
            monitor,
            quality,
            volume,
            anime4k,
        } => ipc::Command::Play {
            monitor,
            path: file
                .canonicalize()
                .with_context(|| format!("file not found: {}", file.display()))?,
            quality: quality.into(),
            volume,
            anime4k,
        },
        DaemonCommand::Stop { monitor } => ipc::Command::Stop { monitor },
        DaemonCommand::Pause { monitor } => ipc::Command::Pause { monitor },
        DaemonCommand::Resume { monitor } => ipc::Command::Resume { monitor },
        DaemonCommand::Volume { volume, monitor } => ipc::Command::SetVolume { monitor, volume },
        DaemonCommand::Quality {
            quality,
            monitor,
            anime4k,
        } => ipc::Command::SetQuality {
            monitor,
            quality: quality.into(),
            anime4k,
        },
        DaemonCommand::Autostart { action } => match action {
            AutostartAction::Status => ipc::Command::GetAutostart,
            AutostartAction::On => ipc::Command::SetAutostart { enabled: true },
            AutostartAction::Off => ipc::Command::SetAutostart { enabled: false },
        },
        DaemonCommand::Battery { action } => match action {
            BatteryAction::Status => ipc::Command::GetBatteryPolicy,
            BatteryAction::Pause => ipc::Command::SetBatteryPolicy {
                policy: ipc::BatteryPolicy::Pause,
            },
            BatteryAction::Eco => ipc::Command::SetBatteryPolicy {
                policy: ipc::BatteryPolicy::Eco,
            },
            BatteryAction::Keep => ipc::Command::SetBatteryPolicy {
                policy: ipc::BatteryPolicy::Keep,
            },
        },
    };
    let response = ipc::send_request(&endpoint, &ipc::Request::new(1, command))
        .with_context(|| format!("failed to contact renderer at {endpoint:?}"))?;
    match response.body {
        ipc::ResponseBody::Success { result } => print_daemon_result(result),
        ipc::ResponseBody::Error { error } => {
            anyhow::bail!("daemon error {:?}: {}", error.code, error.message)
        }
    }
}

fn print_daemon_result(result: ipc::ResponseData) -> anyhow::Result<()> {
    match result {
        ipc::ResponseData::Pong { daemon_version } => {
            println!("renderer daemon v{daemon_version} is ready");
        }
        ipc::ResponseData::Monitors { monitors } => {
            for monitor in monitors {
                println!(
                    "{}: {}  {}x{} at ({}, {})  scale {:.2}{}",
                    monitor.id,
                    monitor.name,
                    monitor.bounds.width,
                    monitor.bounds.height,
                    monitor.bounds.x,
                    monitor.bounds.y,
                    monitor.scale,
                    if monitor.is_primary { "  primary" } else { "" }
                );
            }
        }
        ipc::ResponseData::Status { sessions } => {
            if sessions.is_empty() {
                println!("no active sessions");
            }
            for session in sessions {
                println!(
                    "monitor {}: {} {}  quality {}{}  volume {}",
                    session.monitor,
                    match session.state {
                        ipc::PlaybackState::Playing => "playing",
                        ipc::PlaybackState::Paused => "paused",
                        ipc::PlaybackState::Stopped => "stopped",
                    },
                    session
                        .path
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "-".into()),
                    match session.quality {
                        ipc::Quality::Eco => "eco",
                        ipc::Quality::Balanced => "balanced",
                        ipc::Quality::Max => "max",
                    },
                    if session.anime4k { " + anime4k" } else { "" },
                    session.volume
                );
            }
        }
        ipc::ResponseData::Autostart { enabled } => {
            println!(
                "autostart: {}",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        ipc::ResponseData::BatteryPolicy { policy } => {
            println!(
                "battery policy: {}",
                match policy {
                    ipc::BatteryPolicy::Pause => "pause",
                    ipc::BatteryPolicy::Eco => "eco",
                    ipc::BatteryPolicy::Keep => "keep",
                }
            );
        }
        ipc::ResponseData::Acknowledged { status } => {
            println!("{status}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// interactive commands
// ---------------------------------------------------------------------------

/// Creates the host, prints the monitor list and returns the target monitor.
fn pick_monitor(
    host: &dyn platform::WallpaperHost,
    monitor: platform::MonitorId,
) -> anyhow::Result<platform::MonitorInfo> {
    let monitors = host.enumerate_monitors()?;
    anyhow::ensure!(!monitors.is_empty(), "no monitors detected");
    println!("monitors:");
    for m in &monitors {
        println!(
            "  {}: {}  {}x{} at ({}, {})  scale {:.2}{}",
            m.id,
            m.name,
            m.bounds.width,
            m.bounds.height,
            m.bounds.x,
            m.bounds.y,
            m.scale,
            if m.is_primary { "  primary" } else { "" }
        );
    }
    monitors
        .into_iter()
        .find(|m| m.id == monitor)
        .ok_or_else(|| platform::HostError::MonitorNotFound(monitor).into())
}

fn wait_for_ctrl_c() -> anyhow::Result<mpsc::Receiver<()>> {
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .context("failed to install Ctrl+C handler")?;
    Ok(rx)
}

fn test_surface(monitor: platform::MonitorId, color: Rgb) -> anyhow::Result<()> {
    let mut host = platform::create_host().context("failed to initialize wallpaper host")?;
    let info = pick_monitor(host.as_ref(), monitor)?;
    println!(
        "creating {}x{} test surface on monitor {} ({}), color #{:02X}{:02X}{:02X}",
        info.bounds.width, info.bounds.height, info.id, info.name, color.r, color.g, color.b
    );

    let surface = host.create_surface(monitor)?;
    host.set_surface_color(surface, [color.r, color.g, color.b])?;

    let stop = wait_for_ctrl_c()?;
    println!("test surface is up behind the desktop icons — press Ctrl+C to stop");
    let _ = stop.recv();

    println!("restoring desktop...");
    host.destroy_surface(surface)?;
    drop(host);
    println!("desktop restored");
    Ok(())
}

/// Commands accepted on stdin while playing.
#[derive(Debug, PartialEq, Eq)]
enum Control {
    Pause,
    Resume,
    Volume(u8),
    Screenshot(PathBuf),
    Stop,
}

fn play(
    file: &Path,
    monitor: platform::MonitorId,
    quality: Quality,
    volume: u8,
    anime4k: bool,
) -> anyhow::Result<()> {
    let file = file
        .canonicalize()
        .with_context(|| format!("file not found: {}", file.display()))?;

    let mut host = platform::create_host().context("failed to initialize wallpaper host")?;
    let info = pick_monitor(host.as_ref(), monitor)?;
    let surface = host.create_surface(monitor)?;
    let result = play_on_surface(
        host.as_mut(),
        surface,
        &info,
        &file,
        quality,
        volume,
        anime4k,
    );

    println!("restoring desktop...");
    host.destroy_surface(surface)?;
    drop(host);
    println!("desktop restored");
    result
}

fn play_on_surface(
    host: &mut dyn platform::WallpaperHost,
    surface: platform::SurfaceHandle,
    info: &platform::MonitorInfo,
    file: &Path,
    quality: Quality,
    volume: u8,
    anime4k: bool,
) -> anyhow::Result<()> {
    let wid = host.surface_native_handle(surface)?;

    let api = playback::load_libmpv()?;
    let (major, minor) = api.version();
    println!("libmpv loaded, client API v{major}.{minor}");

    let started = playback::start_player(api, wid, file, quality, volume, anime4k, info)?;
    println!(
        "playing {} ({}, {}x{}) on monitor {}",
        file.display(),
        started.codec,
        started.width,
        started.height,
        info.id
    );
    println!("hardware decoding: {}", started.hwdec);
    let player = started.player;

    let controls = spawn_control_channel()?;
    println!("controls: pause | resume | volume <0-100> | screenshot <path> | stop (or Ctrl+C)");
    loop {
        match controls.recv() {
            Ok(Control::Pause) => {
                player.set_property_bool("pause", true)?;
                println!("paused (decoding stopped)");
            }
            Ok(Control::Resume) => {
                player.set_property_bool("pause", false)?;
                println!("resumed");
            }
            Ok(Control::Volume(v)) => {
                playback::set_volume(&player, v)?;
                println!("volume: {v}");
            }
            Ok(Control::Screenshot(path)) => {
                let path = path.to_string_lossy();
                player.command(&["screenshot-to-file", &path, "scaled"])?;
                println!("screenshot requested: {path}");
            }
            Ok(Control::Stop) | Err(_) => break,
        }
    }
    // Player must shut down before the surface window is destroyed.
    drop(player);
    Ok(())
}

/// Merges stdin commands and Ctrl+C into one control channel.
fn spawn_control_channel() -> anyhow::Result<mpsc::Receiver<Control>> {
    let (tx, rx) = mpsc::channel();

    let ctrlc_tx = tx.clone();
    ctrlc::set_handler(move || {
        let _ = ctrlc_tx.send(Control::Stop);
    })
    .context("failed to install Ctrl+C handler")?;

    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            let command = match parse_control_line(&line) {
                Ok(Some(command)) => command,
                Ok(None) => continue,
                Err(message) => {
                    println!("{message}");
                    continue;
                }
            };
            if tx.send(command).is_err() {
                break;
            }
        }
        // stdin EOF (e.g. detached run): keep playing until Ctrl+C.
    });

    Ok(rx)
}

fn parse_control_line(line: &str) -> std::result::Result<Option<Control>, String> {
    // Piped input may carry a BOM before the first command.
    let line = line.trim_start_matches('\u{FEFF}').trim();
    if line.is_empty() {
        return Ok(None);
    }
    let split = line.find(char::is_whitespace).unwrap_or(line.len());
    let (command, argument) = line.split_at(split);
    let argument = argument.trim();
    match command {
        "pause" if argument.is_empty() => Ok(Some(Control::Pause)),
        "resume" if argument.is_empty() => Ok(Some(Control::Resume)),
        "stop" | "quit" if argument.is_empty() => Ok(Some(Control::Stop)),
        "volume" => argument
            .parse::<u8>()
            .ok()
            .filter(|volume| *volume <= 100)
            .map(Control::Volume)
            .map(Some)
            .ok_or_else(|| "volume must be 0-100".into()),
        "screenshot" if !argument.is_empty() => {
            let path = unquote_path(argument);
            if path.is_empty() {
                Err("screenshot path must not be empty".into())
            } else {
                Ok(Some(Control::Screenshot(PathBuf::from(path))))
            }
        }
        _ => Err("commands: pause | resume | volume <0-100> | screenshot <path> | stop".into()),
    }
}

fn unquote_path(path: &str) -> &str {
    if path.len() >= 2
        && ((path.starts_with('"') && path.ends_with('"'))
            || (path.starts_with('\'') && path.ends_with('\'')))
    {
        &path[1..path.len() - 1]
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_color_with_and_without_hash() {
        let expected = Rgb {
            r: 0x33,
            g: 0x66,
            b: 0x99,
        };
        assert_eq!(parse_color("#336699"), Ok(expected));
        assert_eq!(parse_color("336699"), Ok(expected));
    }

    #[test]
    fn rejects_malformed_colors() {
        for bad in ["", "#36", "#3366", "#33669Z", "#3366999", "надпись"] {
            assert!(parse_color(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn parses_anime4k_play_flag() {
        let cli = Cli::try_parse_from(["renderer", "play", "wallpaper.mp4", "--anime4k"])
            .unwrap_or_else(|error| panic!("CLI should parse: {error}"));
        match cli.command {
            Command::Play { anime4k, .. } => assert!(anime4k),
            _ => panic!("expected play command"),
        }
    }

    #[test]
    fn parses_ctl_playback_commands() {
        let cli = Cli::try_parse_from([
            "renderer",
            "ctl",
            "play",
            "a.mp4",
            "--monitor",
            "1",
            "--quality",
            "max",
        ])
        .unwrap_or_else(|error| panic!("CLI should parse: {error}"));
        match cli.command {
            Command::Ctl {
                command:
                    DaemonCommand::Play {
                        monitor, quality, ..
                    },
                ..
            } => {
                assert_eq!(monitor, 1);
                assert_eq!(quality, Quality::Max);
            }
            _ => panic!("expected ctl play command"),
        }

        let cli = Cli::try_parse_from(["renderer", "ctl", "volume", "40"])
            .unwrap_or_else(|error| panic!("CLI should parse: {error}"));
        match cli.command {
            Command::Ctl {
                command: DaemonCommand::Volume { volume, monitor },
                ..
            } => {
                assert_eq!(volume, 40);
                assert_eq!(monitor, 0);
            }
            _ => panic!("expected ctl volume command"),
        }
    }

    #[test]
    fn anime4k_bundle_is_complete() {
        let shaders =
            playback::find_anime4k_shaders().expect("vendored Anime4K bundle should be present");
        assert_eq!(shaders.len(), playback::ANIME4K_MODE_B_FAST.len());
    }

    #[test]
    fn parses_runtime_controls() {
        assert_eq!(parse_control_line("pause"), Ok(Some(Control::Pause)));
        assert_eq!(
            parse_control_line("volume 42"),
            Ok(Some(Control::Volume(42)))
        );
        assert_eq!(
            parse_control_line(r#"screenshot "docs/comparisons/eco frame.png""#),
            Ok(Some(Control::Screenshot(PathBuf::from(
                "docs/comparisons/eco frame.png"
            ))))
        );
        assert_eq!(parse_control_line("\u{FEFF}stop"), Ok(Some(Control::Stop)));
        assert_eq!(parse_control_line("  "), Ok(None));
    }

    #[test]
    fn rejects_invalid_runtime_controls() {
        assert!(parse_control_line("volume 101").is_err());
        assert!(parse_control_line("screenshot \"\"").is_err());
        assert!(parse_control_line("unknown").is_err());
    }
}
