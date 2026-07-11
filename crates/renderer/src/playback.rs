//! Shared libmpv playback pipeline used by the interactive `play` command and
//! the daemon: player options, startup probing and upscaling shaders.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Quality {
    /// Cheapest scaling (bilinear), no shaders.
    Eco,
    /// Lanczos scaling (default).
    Balanced,
    /// Lanczos + FSR shaders when the source is smaller than the monitor.
    Max,
}

impl From<ipc::Quality> for Quality {
    fn from(quality: ipc::Quality) -> Self {
        match quality {
            ipc::Quality::Eco => Self::Eco,
            ipc::Quality::Balanced => Self::Balanced,
            ipc::Quality::Max => Self::Max,
        }
    }
}

impl From<Quality> for ipc::Quality {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Eco => Self::Eco,
            Quality::Balanced => Self::Balanced,
            Quality::Max => Self::Max,
        }
    }
}

/// A started player together with what was probed about the media.
pub struct StartedPlayback {
    pub player: mpv::Player,
    pub width: i64,
    pub height: i64,
    pub codec: String,
    pub hwdec: String,
    /// Human-readable outcome of the shader decision ("Anime4K active …",
    /// "FSR off: …"); surfaced to the user, who cannot see the daemon log.
    pub shaders: String,
}

/// Creates a player bound to `wid`, loads `file` and waits until playback is
/// ready, then applies the upscaling shaders that match the profile.
pub fn start_player(
    api: Arc<mpv::Api>,
    wid: u64,
    file: &Path,
    quality: Quality,
    volume: u8,
    anime4k: bool,
    monitor: &platform::MonitorInfo,
) -> anyhow::Result<StartedPlayback> {
    let options = player_options(wid, quality, volume);
    let option_refs: Vec<(&str, &str)> = options
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let player = mpv::Player::new(api, &option_refs).context("failed to initialize mpv")?;
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

    let shaders = apply_shaders(&player, quality, anime4k, width, height, monitor)?;

    Ok(StartedPlayback {
        player,
        width,
        height,
        codec,
        hwdec,
        shaders,
    })
}

/// Initialization-time mpv options shared by every wallpaper session.
fn player_options(wid: u64, quality: Quality, volume: u8) -> Vec<(String, String)> {
    let mut options: Vec<(&str, String)> = vec![
        ("wid", wid.to_string()),
        // Hardware decoding with safe fallback (D3D11VA on Windows).
        ("hwdec", "auto-safe".into()),
        ("loop-file", "inf".into()),
        // Static images stay up forever through the same pipeline.
        ("image-display-duration", "inf".into()),
        // Fill the monitor without black bars (crop instead of letterbox).
        ("panscan", "1.0".into()),
        // A wallpaper must never keep the system awake.
        ("stop-screensaver", "no".into()),
        // Known 24H2 fix: without it the surface may not cover the screen.
        ("border", "no".into()),
        // Headless embedding: no OSD, no scripts, no input handling.
        ("input-default-bindings", "no".into()),
        ("osd-level", "0".into()),
        ("load-scripts", "no".into()),
        ("config", "no".into()),
        // Capture the same GPU-rendered output (including GLSL shaders) that
        // is visible in the wallpaper window.
        ("screenshot-sw", "no".into()),
        ("mute", if volume == 0 { "yes" } else { "no" }.into()),
    ];
    if volume > 0 {
        options.push(("volume", f64::from(volume).to_string()));
    }
    for (name, value) in profile_scalers(quality) {
        options.push((name, value.into()));
    }
    options
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

/// Scaler properties per quality profile; also settable at runtime.
fn profile_scalers(quality: Quality) -> [(&'static str, &'static str); 3] {
    match quality {
        Quality::Eco => [
            ("scale", "bilinear"),
            ("dscale", "bilinear"),
            ("cscale", "bilinear"),
        ],
        Quality::Balanced | Quality::Max => [
            ("scale", "lanczos"),
            ("dscale", "mitchell"),
            ("cscale", "bilinear"),
        ],
    }
}

/// Switches scalers and shaders of a running player to another profile.
/// Returns the human-readable shader outcome.
pub fn set_quality(
    player: &mpv::Player,
    quality: Quality,
    anime4k: bool,
    width: i64,
    height: i64,
    monitor: &platform::MonitorInfo,
) -> anyhow::Result<String> {
    for (name, value) in profile_scalers(quality) {
        player.set_property_str(name, value)?;
    }
    apply_shaders(player, quality, anime4k, width, height, monitor)
}

pub fn set_volume(player: &mpv::Player, volume: u8) -> anyhow::Result<()> {
    player.set_property_bool("mute", volume == 0)?;
    player.set_property_f64("volume", f64::from(volume))?;
    Ok(())
}

/// Applies the shader chain matching the profile, or clears it. Shaders are
/// used only while upscaling; at native size they would only waste GPU
/// cycles. Returns a human-readable outcome for the user.
fn apply_shaders(
    player: &mpv::Player,
    quality: Quality,
    anime4k: bool,
    width: i64,
    height: i64,
    monitor: &platform::MonitorInfo,
) -> anyhow::Result<String> {
    let target = format!(
        "{width}x{height} -> {}x{}",
        monitor.bounds.width, monitor.bounds.height
    );
    if anime4k {
        if let Some(reason) = upscaling_block_reason(width, height, monitor) {
            player.set_property_str("glsl-shaders", "")?;
            return Ok(format!("Anime4K off: {reason}"));
        }
        if let Some(shaders) = find_anime4k_shaders() {
            player.set_property_str("glsl-shaders", &shader_path_list(&shaders))?;
            return Ok(format!("Anime4K Mode B active ({target})"));
        }
        player.set_property_str("glsl-shaders", "")?;
        return Ok("Anime4K off: assets/shaders/anime4k files not found".into());
    }
    if quality == Quality::Max {
        if let Some(reason) = upscaling_block_reason(width, height, monitor) {
            player.set_property_str("glsl-shaders", "")?;
            return Ok(format!("FSR off: {reason}"));
        }
        if let Some(shader) = find_fsr_shader() {
            // mpv wants forward slashes in list options on all platforms.
            let shader = shader.to_string_lossy().replace('\\', "/");
            player.set_property_str("glsl-shaders", &shader)?;
            return Ok(format!("FSR active ({target})"));
        }
        player.set_property_str("glsl-shaders", "")?;
        return Ok("FSR off: assets/shaders/FSR.glsl not found".into());
    }
    player.set_property_str("glsl-shaders", "")?;
    Ok(format!(
        "plain {} scaling",
        match quality {
            Quality::Eco => "bilinear",
            Quality::Balanced | Quality::Max => "lanczos",
        }
    ))
}

/// `Some(reason)` when upscaling shaders must stay off.
fn upscaling_block_reason(
    width: i64,
    height: i64,
    monitor: &platform::MonitorInfo,
) -> Option<String> {
    if width <= 0 || height <= 0 {
        return Some("unknown source size".into());
    }
    let monitor_width = i64::from(monitor.bounds.width.cast_signed().max(0));
    let monitor_height = i64::from(monitor.bounds.height.cast_signed().max(0));
    if width >= monitor_width && height >= monitor_height {
        return Some(format!("source ({width}x{height}) >= monitor resolution"));
    }
    None
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

pub const ANIME4K_MODE_B_FAST: [&str; 6] = [
    "Anime4K_Clamp_Highlights.glsl",
    "Anime4K_Restore_CNN_Soft_M.glsl",
    "Anime4K_Upscale_CNN_x2_M.glsl",
    "Anime4K_AutoDownscalePre_x2.glsl",
    "Anime4K_AutoDownscalePre_x4.glsl",
    "Anime4K_Upscale_CNN_x2_S.glsl",
];

pub fn find_anime4k_shaders() -> Option<Vec<PathBuf>> {
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

/// Loads libmpv-2.dll: exe directory / system search first, then the
/// development checkout location filled by scripts/fetch-libmpv.ps1.
pub fn load_libmpv() -> anyhow::Result<Arc<mpv::Api>> {
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
