//! Windowless entry point for the wallpaper daemon, used by autostart.
//!
//! `renderer.exe` is a console application, so launching it from the Run key at
//! logon gives it a console window: it pops up in the user's face, and closing
//! it kills the daemon. This binary runs the very same daemon in the windows
//! subsystem — no console, nothing to close — and leaves `renderer.exe` a
//! proper CLI with working stdout and stdin.
#![windows_subsystem = "windows"]

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "limewall-daemon",
    about = "LimeWall wallpaper daemon (no console window)",
    version
)]
struct Cli {
    /// IPC endpoint to listen on (default: the per-user LimeWall endpoint).
    #[arg(long)]
    endpoint: Option<String>,
    /// Wallpaper state file (default: %APPDATA%/LimeWall/wallpapers.json).
    #[arg(long)]
    state: Option<PathBuf>,
    /// Log file (default: %APPDATA%/LimeWall/daemon.log).
    #[arg(long)]
    log: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Without a console there is nowhere for the daemon's output to go, and a
    // silent daemon cannot be diagnosed from a bug report. Redirect first, so
    // even a failing start leaves a trace. A fresh file per start, matching the
    // log the UI keeps when it spawns the daemon itself.
    if let Some(log) = cli.log.or_else(default_log_path)
        && let Err(error) = platform::redirect_output_to_file(&log)
    {
        eprintln!("logging to file disabled: {error}");
    }
    renderer::daemon::run(cli.endpoint.as_deref(), cli.state.as_deref())
}

fn default_log_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("LimeWall").join("daemon.log"))
}
