//! Platform-neutral tray API. The daemon shows one tray icon with a small
//! menu; menu picks arrive through the callback on an internal thread.

/// What the user picked in the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    PauseAll,
    ResumeAll,
    OpenUi,
    Quit,
}

/// Keeps the tray icon alive; dropping it removes the icon.
pub struct TrayGuard {
    /// Held only for its Drop side effect.
    #[cfg(windows)]
    pub(crate) _inner: crate::tray_win32::Win32TrayGuard,
}

/// Shows the tray icon. `on_event` is called from the tray thread — forward
/// to a channel instead of doing real work inside.
pub fn spawn(
    tooltip: &str,
    on_event: impl Fn(TrayEvent) + Send + 'static,
) -> crate::Result<TrayGuard> {
    #[cfg(windows)]
    {
        Ok(TrayGuard {
            _inner: crate::tray_win32::spawn(tooltip, on_event)?,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (tooltip, on_event);
        Err(crate::HostError::Unsupported("tray icon"))
    }
}
