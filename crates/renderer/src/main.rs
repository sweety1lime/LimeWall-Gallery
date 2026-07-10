use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "renderer", about = "LiveWall wallpaper renderer", version)]
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
    Ping,
    ListMonitors,
    Status,
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Quality {
    /// Cheapest scaling (bilinear), no shaders.
    Eco,
    /// Lanczos scaling (default).
    Balanced,
    /// Lanczos + FSR shaders when the source is smaller than the monitor.
    Max,
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
        Command::Serve { endpoint } => serve(endpoint.as_deref()),
        Command::Ctl { endpoint, command } => ctl(endpoint.as_deref(), command),
    }
}

// ---------------------------------------------------------------------------
// daemon skeleton
// ---------------------------------------------------------------------------

fn serve(endpoint: Option<&str>) -> anyhow::Result<()> {
    let endpoint = endpoint
        .map(str::to_owned)
        .unwrap_or_else(ipc::default_endpoint);
    let host = platform::create_host().context("failed to initialize wallpaper host")?;
    let server = ipc::LocalServer::bind(&endpoint)
        .with_context(|| format!("failed to bind renderer endpoint {endpoint:?}"))?;
    println!("renderer daemon listening at {endpoint}");

    loop {
        let mut stream = server.accept().context("failed to accept IPC client")?;
        if handle_daemon_connection(&mut stream, host.as_ref())? {
            break;
        }
    }
    println!("renderer daemon stopped");
    Ok(())
}

fn handle_daemon_connection(
    stream: &mut ipc::LocalStream,
    host: &dyn platform::WallpaperHost,
) -> anyhow::Result<bool> {
    let request: ipc::Request = match ipc::read_frame(stream) {
        Ok(request) => request,
        Err(error) => {
            let response = ipc::Response::error(
                0,
                ipc::ErrorCode::InvalidRequest,
                format!("invalid IPC frame: {error}"),
            );
            let _ = ipc::write_frame(stream, &response);
            eprintln!("rejected invalid IPC frame: {error}");
            return Ok(false);
        }
    };
    let shutdown = matches!(request.command, ipc::Command::Shutdown);
    let response = daemon_response(host, request);
    ipc::write_frame(stream, &response).context("failed to write IPC response")?;
    Ok(shutdown && matches!(response.body, ipc::ResponseBody::Success { .. }))
}

fn daemon_response(host: &dyn platform::WallpaperHost, request: ipc::Request) -> ipc::Response {
    if let Err(error) = request.validate() {
        let code = if matches!(error, ipc::ValidationError::UnsupportedVersion { .. }) {
            ipc::ErrorCode::UnsupportedVersion
        } else {
            ipc::ErrorCode::InvalidRequest
        };
        return ipc::Response::error(request.id, code, error.to_string());
    }

    let id = request.id;
    match request.command {
        ipc::Command::Ping => ipc::Response::success(
            id,
            ipc::ResponseData::Pong {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            },
        ),
        ipc::Command::ListMonitors => match host.enumerate_monitors() {
            Ok(monitors) => ipc::Response::success(
                id,
                ipc::ResponseData::Monitors {
                    monitors: monitors.into_iter().map(monitor_to_ipc).collect(),
                },
            ),
            Err(error) => ipc::Response::error(id, ipc::ErrorCode::Internal, error.to_string()),
        },
        ipc::Command::Status => ipc::Response::success(
            id,
            ipc::ResponseData::Status {
                sessions: Vec::new(),
            },
        ),
        ipc::Command::Shutdown => ipc::Response::success(
            id,
            ipc::ResponseData::Acknowledged {
                status: "shutting_down".into(),
            },
        ),
        _ => ipc::Response::error(
            id,
            ipc::ErrorCode::InvalidRequest,
            "playback commands are not implemented until phase 2.1c",
        ),
    }
}

fn monitor_to_ipc(monitor: platform::MonitorInfo) -> ipc::Monitor {
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

fn ctl(endpoint: Option<&str>, command: DaemonCommand) -> anyhow::Result<()> {
    let endpoint = endpoint
        .map(str::to_owned)
        .unwrap_or_else(ipc::default_endpoint);
    let command = match command {
        DaemonCommand::Ping => ipc::Command::Ping,
        DaemonCommand::ListMonitors => ipc::Command::ListMonitors,
        DaemonCommand::Status => ipc::Command::Status,
        DaemonCommand::Shutdown => ipc::Command::Shutdown,
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
            println!("active sessions: {}", sessions.len());
        }
        ipc::ResponseData::Acknowledged { status } => {
            println!("{status}");
        }
    }
    Ok(())
}

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

// ---------------------------------------------------------------------------
// play
// ---------------------------------------------------------------------------

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

    let api = load_libmpv()?;
    let (major, minor) = api.version();
    println!("libmpv loaded, client API v{major}.{minor}");

    let wid_string = wid.to_string();
    let volume_string = f64::from(volume).to_string();
    let mut options: Vec<(&str, &str)> = vec![
        ("wid", wid_string.as_str()),
        // Hardware decoding with safe fallback (D3D11VA on Windows).
        ("hwdec", "auto-safe"),
        ("loop-file", "inf"),
        // Static images stay up forever through the same pipeline.
        ("image-display-duration", "inf"),
        // Fill the monitor without black bars (crop instead of letterbox).
        ("panscan", "1.0"),
        // A wallpaper must never keep the system awake.
        ("stop-screensaver", "no"),
        // Known 24H2 fix: without it the surface may not cover the screen.
        ("border", "no"),
        // Headless embedding: no OSD, no scripts, no input handling.
        ("input-default-bindings", "no"),
        ("osd-level", "0"),
        ("load-scripts", "no"),
        ("config", "no"),
        // Capture the same GPU-rendered output (including GLSL shaders) that
        // is visible in the wallpaper window.
        ("screenshot-sw", "no"),
    ];
    options.push(("mute", if volume == 0 { "yes" } else { "no" }));
    if volume > 0 {
        options.push(("volume", volume_string.as_str()));
    }
    match quality {
        Quality::Eco => {
            options.push(("scale", "bilinear"));
            options.push(("dscale", "bilinear"));
            options.push(("cscale", "bilinear"));
        }
        Quality::Balanced | Quality::Max => options.push(("scale", "lanczos")),
    }

    let player = mpv::Player::new(api, &options).context("failed to initialize mpv")?;
    player
        .command(&["loadfile", &file.to_string_lossy()])
        .context("loadfile failed")?;

    // Wait for the file to load so we can inspect what we are playing.
    let mut loaded = false;
    for _ in 0..60 {
        match player.wait_event(0.25) {
            Some(mpv::Event::FileLoaded) => {
                loaded = true;
                break;
            }
            Some(mpv::Event::EndFile) => anyhow::bail!("mpv could not play the file"),
            _ => {}
        }
    }
    anyhow::ensure!(loaded, "timed out waiting for the file to load");

    let (width, height) = video_size(&player);
    let codec = player
        .get_property_str("video-codec")
        .unwrap_or_else(|_| "unknown".into());
    println!(
        "playing {} ({codec}, {width}x{height}) on monitor {}",
        file.display(),
        info.id
    );

    // hwdec-current settles once decoding actually starts.
    let mut hwdec = String::from("no");
    for _ in 0..20 {
        if let Ok(current) = player.get_property_str("hwdec-current")
            && !current.is_empty()
            && current != "no"
        {
            hwdec = current;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    println!("hardware decoding: {hwdec}");

    if anime4k {
        apply_anime4k(&player, width, height, info)?;
    } else if quality == Quality::Max {
        apply_fsr(&player, width, height, info)?;
    }

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
                player.set_property_bool("mute", v == 0)?;
                player.set_property_f64("volume", f64::from(v))?;
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

fn video_size(player: &mpv::Player) -> (i64, i64) {
    // video-params appears shortly after FILE_LOADED; retry briefly.
    for _ in 0..20 {
        let size = player
            .get_property_i64("video-params/w")
            .and_then(|w| player.get_property_i64("video-params/h").map(|h| (w, h)));
        if let Ok(size) = size {
            return size;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    (0, 0)
}

/// Enables the FSR shader only when the source is smaller than the monitor
/// (upscaling); at native size or above it would only waste GPU cycles.
fn apply_fsr(
    player: &mpv::Player,
    width: i64,
    height: i64,
    info: &platform::MonitorInfo,
) -> anyhow::Result<()> {
    if width <= 0 || height <= 0 {
        println!("FSR skipped: unknown source size");
        return Ok(());
    }
    if !needs_upscaling(width, height, info) {
        println!("FSR skipped: source ({width}x{height}) >= monitor resolution");
        return Ok(());
    }
    let Some(shader) = find_fsr_shader() else {
        println!("FSR skipped: assets/shaders/FSR.glsl not found");
        return Ok(());
    };
    // mpv wants forward slashes in list options on all platforms.
    let shader = shader.to_string_lossy().replace('\\', "/");
    player.set_property_str("glsl-shaders", &shader)?;
    println!("FSR upscaling enabled ({shader})");
    Ok(())
}

const ANIME4K_MODE_B_FAST: [&str; 6] = [
    "Anime4K_Clamp_Highlights.glsl",
    "Anime4K_Restore_CNN_Soft_M.glsl",
    "Anime4K_Upscale_CNN_x2_M.glsl",
    "Anime4K_AutoDownscalePre_x2.glsl",
    "Anime4K_AutoDownscalePre_x4.glsl",
    "Anime4K_Upscale_CNN_x2_S.glsl",
];

/// Enables Anime4K's official Mode B (Fast) chain only while upscaling.
/// Anime4K replaces FSR when both `--quality max` and `--anime4k` are set.
fn apply_anime4k(
    player: &mpv::Player,
    width: i64,
    height: i64,
    info: &platform::MonitorInfo,
) -> anyhow::Result<()> {
    if width <= 0 || height <= 0 {
        println!("Anime4K skipped: unknown source size");
        return Ok(());
    }
    if !needs_upscaling(width, height, info) {
        println!("Anime4K skipped: source ({width}x{height}) >= monitor resolution");
        return Ok(());
    }
    let Some(shaders) = find_anime4k_shaders() else {
        println!("Anime4K skipped: required assets/shaders/anime4k files not found");
        return Ok(());
    };
    let shader_list = shader_path_list(&shaders);
    player.set_property_str("glsl-shaders", &shader_list)?;
    println!("Anime4K enabled: Mode B (Fast)");
    Ok(())
}

fn needs_upscaling(width: i64, height: i64, info: &platform::MonitorInfo) -> bool {
    let monitor_width = i64::from(info.bounds.width.cast_signed().max(0));
    let monitor_height = i64::from(info.bounds.height.cast_signed().max(0));
    width < monitor_width || height < monitor_height
}

fn find_anime4k_shaders() -> Option<Vec<PathBuf>> {
    shader_roots()
        .map(|root| root.join("anime4k"))
        .find_map(|root| {
            let shaders: Vec<_> = ANIME4K_MODE_B_FAST
                .iter()
                .map(|name| root.join(name))
                .collect();
            shaders.iter().all(|path| path.is_file()).then_some(shaders)
        })
}

fn shader_path_list(paths: &[PathBuf]) -> String {
    let separator = if cfg!(windows) { ";" } else { ":" };
    paths
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join(separator)
}

fn find_fsr_shader() -> Option<PathBuf> {
    shader_roots()
        .map(|root| root.join("FSR.glsl"))
        .find(|path| path.is_file())
}

fn shader_roots() -> impl Iterator<Item = PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        roots.push(dir.join("shaders"));
    }
    // Development checkout: workspace assets directory.
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders"));
    roots.into_iter()
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

/// Loads libmpv-2.dll: exe directory / system search first, then the
/// development checkout location filled by scripts/fetch-libmpv.ps1.
fn load_libmpv() -> anyhow::Result<std::sync::Arc<mpv::Api>> {
    let primary = mpv::Api::load();
    if let Ok(api) = primary {
        return Ok(api);
    }
    let dev_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/mpv/unpacked/libmpv-2.dll");
    if dev_path.exists()
        && let Ok(api) = mpv::Api::load_from(&dev_path)
    {
        return Ok(api);
    }
    primary.context(
        "libmpv-2.dll not found: run scripts/fetch-libmpv.ps1 or place it next to renderer.exe",
    )
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
    fn anime4k_bundle_is_complete() {
        let shaders = find_anime4k_shaders().expect("vendored Anime4K bundle should be present");
        assert_eq!(shaders.len(), ANIME4K_MODE_B_FAST.len());
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
