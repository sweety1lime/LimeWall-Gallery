//! Platform abstraction for placing wallpaper surfaces behind desktop icons.
//!
//! Each OS backend implements [`WallpaperHost`]. The rest of the workspace
//! must stay platform-agnostic and talk only to this trait.

/// Identifier of a monitor within the current session.
///
/// Indices follow the order returned by [`WallpaperHost::enumerate_monitors`]
/// and are not guaranteed to be stable across display topology changes.
pub type MonitorId = usize;

/// Rectangle in physical pixels, virtual-desktop coordinates.
///
/// The virtual-desktop origin is the top-left corner of the primary monitor,
/// so `x`/`y` are negative for monitors placed left of / above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A connected monitor as reported by the OS.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// Human-readable name (device or model name, backend-dependent).
    pub name: String,
    /// Position and resolution in physical pixels, virtual-desktop coordinates.
    pub bounds: Rect,
    /// DPI scale factor (1.0 = 96 dpi, 1.5 = 144 dpi, ...).
    pub scale: f64,
    pub is_primary: bool,
}

/// Opaque handle to a wallpaper surface created by a [`WallpaperHost`].
///
/// This is a stable key, not the native window value: backends may recreate
/// the underlying window (e.g. after explorer.exe restarts) without changing
/// the handle. Phase 1 adds an accessor for the current native window so the
/// renderer can hand the surface to libmpv via `--wid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceHandle(pub(crate) u64);

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("monitor {0} not found")]
    MonitorNotFound(MonitorId),
    #[error("surface {0:?} not found")]
    SurfaceNotFound(SurfaceHandle),
    #[error("desktop integration failed: {0}")]
    Desktop(String),
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, HostError>;

/// A platform backend that can host wallpaper surfaces behind desktop icons.
pub trait WallpaperHost {
    /// Lists connected monitors in backend order (see [`MonitorId`] caveats).
    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>>;

    /// Creates a surface covering the given monitor, placed behind the
    /// desktop icons. The surface stays alive until destroyed or the host is
    /// dropped.
    fn create_surface(&mut self, monitor: MonitorId) -> Result<SurfaceHandle>;

    /// Creates a surface hosting a webview that serves `root` over an internal
    /// protocol and loads `entry` (a file name inside `root`), placed behind
    /// the desktop icons. Used for HTML and glTF (three.js) wallpapers
    /// (phase 6). The protocol (not `file://`) lets pages fetch assets and
    /// load ES modules and 3D models.
    fn create_web_surface(
        &mut self,
        monitor: MonitorId,
        root: &std::path::Path,
        entry: &str,
    ) -> Result<SurfaceHandle> {
        let _ = (monitor, root, entry);
        Err(HostError::Unsupported("web surface"))
    }

    /// Suspends or resumes a web surface (WebView2 TrySuspend): a paused web
    /// wallpaper must drop to ~0% CPU. No-op for plain surfaces.
    fn set_web_suspended(&mut self, surface: SurfaceHandle, suspended: bool) -> Result<()> {
        let _ = (surface, suspended);
        Err(HostError::Unsupported("web suspend"))
    }

    /// Destroys a surface and restores the desktop area it covered.
    fn destroy_surface(&mut self, surface: SurfaceHandle) -> Result<()>;

    /// Hides all surfaces without destroying them (content playback is
    /// paused separately by the renderer).
    fn pause(&mut self) -> Result<()>;

    /// Undoes [`WallpaperHost::pause`].
    fn resume(&mut self) -> Result<()>;

    /// Fills a surface with a solid color. Diagnostic path for the phase 0
    /// `test-surface` command; real content rendering replaces it in phase 1.
    fn set_surface_color(&mut self, surface: SurfaceHandle, rgb: [u8; 3]) -> Result<()> {
        let _ = (surface, rgb);
        Err(HostError::Unsupported("solid-color fill"))
    }

    /// Current native window value of a surface (`HWND` on Windows), for
    /// embedding renderers such as libmpv (`wid`).
    ///
    /// The value goes stale if the backend has to recreate the window (e.g.
    /// explorer.exe restart) — callers must re-query after such events.
    fn surface_native_handle(&self, surface: SurfaceHandle) -> Result<u64> {
        let _ = surface;
        Err(HostError::Unsupported("native window handle"))
    }
}

#[cfg(windows)]
mod autostart_win32;
#[cfg(windows)]
mod harden_win32;
#[cfg(windows)]
mod resources_win32;
#[cfg(windows)]
mod tray_win32;
#[cfg(windows)]
mod watcher_win32;
#[cfg(windows)]
mod win32;

pub mod resources;
pub mod tray;
pub mod watcher;

/// Creates the backend for the current platform.
pub fn create_host() -> Result<Box<dyn WallpaperHost>> {
    #[cfg(windows)]
    {
        Ok(Box::new(win32::Win32Host::new()?))
    }
    #[cfg(not(windows))]
    {
        Err(HostError::Unsupported(
            "only Windows is targeted in phase 0",
        ))
    }
}

/// Applies the process-mitigation policies compatible with the renderer's stack
/// (see docs/research/renderer-sandbox.md). Best-effort, idempotent, and a no-op
/// on non-Windows. Call once, as early as possible in `main`, before libmpv or
/// WebView2 are loaded.
pub fn harden_process() {
    #[cfg(windows)]
    {
        harden_win32::harden_process();
    }
}

/// Whether `app` is registered to start with the user session.
pub fn autostart_enabled(app: &str) -> Result<bool> {
    #[cfg(windows)]
    {
        autostart_win32::enabled(app)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err(HostError::Unsupported("autostart"))
    }
}

/// Whether the desktop icon layer is currently visible. `None` when it cannot
/// be determined (shell window missing, non-Windows). Diagnostics use this to
/// flag the Windows 11 24H2 case where hidden icons make the wallpaper layer
/// invisible.
pub fn desktop_icons_visible() -> Option<bool> {
    #[cfg(windows)]
    {
        win32::desktop_icons_visible()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Registers `command` to run at logon under the name `app`, or removes the
/// registration when `command` is `None`.
pub fn set_autostart(app: &str, command: Option<&str>) -> Result<()> {
    #[cfg(windows)]
    {
        autostart_win32::set(app, command)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, command);
        Err(HostError::Unsupported("autostart"))
    }
}
