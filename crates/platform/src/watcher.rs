//! Platform-neutral activity watcher: reports the system states that a
//! polite wallpaper must react to. Events fire on change only, from an
//! internal thread — forward them to a channel instead of doing real work
//! inside the callback.

/// A change in system activity relevant to wallpaper playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvent {
    /// Device names of monitors currently covered by a fullscreen foreground
    /// window (empty when none). Own and shell windows are excluded.
    Fullscreen(Vec<String>),
    /// The machine switched to battery power (true) or back to AC (false).
    Battery(bool),
    /// The user session was locked / unlocked.
    SessionLocked(bool),
    /// The console display turned off / on.
    DisplayOff(bool),
}

/// Keeps the watcher alive; dropping it stops the thread.
pub struct WatcherGuard {
    /// Held only for its Drop side effect.
    #[cfg(windows)]
    pub(crate) _inner: crate::watcher_win32::Win32WatcherGuard,
}

/// Starts the watcher. Initial states are reported as events right away so
/// the consumer does not need a separate snapshot call.
pub fn spawn(on_event: impl Fn(ActivityEvent) + Send + 'static) -> crate::Result<WatcherGuard> {
    #[cfg(windows)]
    {
        Ok(WatcherGuard {
            _inner: crate::watcher_win32::spawn(on_event)?,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = on_event;
        Err(crate::HostError::Unsupported("activity watcher"))
    }
}
