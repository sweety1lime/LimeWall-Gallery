//! Win32 activity watcher: a hidden window thread that polls the foreground
//! window and battery state (500 ms) and receives session-lock and display
//! power notifications. The 2-second reaction budget of the phase 3
//! acceptance criterion allows comfortable polling; one poll is a handful of
//! user32 calls, far below measurable CPU cost.

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    GetSystemPowerStatus, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, KillTimer, MSG, PBT_POWERSETTINGCHANGE, PostMessageW,
    PostQuitMessage, RegisterClassW, SetTimer, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY,
    WM_POWERBROADCAST, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

use crate::watcher::ActivityEvent;
use crate::{HostError, Result};

const POLL_TIMER_ID: usize = 1;
const POLL_INTERVAL_MS: u32 = 500;

/// WM_WTSSESSION_CHANGE and its lock/unlock codes (winuser).
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;

const WATCHER_CLASS: windows::core::PCWSTR = w!("LimeWallWatcher");

thread_local! {
    static WATCHER: std::cell::RefCell<Option<WatcherState>> =
        const { std::cell::RefCell::new(None) };
}

struct WatcherState {
    on_event: Box<dyn Fn(ActivityEvent) + Send>,
    fullscreen: Vec<String>,
    on_battery: Option<bool>,
    display_off: bool,
}

pub struct Win32WatcherGuard {
    window: isize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Win32WatcherGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.window as *mut core::ffi::c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn spawn(on_event: impl Fn(ActivityEvent) + Send + 'static) -> Result<Win32WatcherGuard> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let on_event: Box<dyn Fn(ActivityEvent) + Send> = Box::new(on_event);
    let thread = std::thread::Builder::new()
        .name("activity-watcher".into())
        .spawn(move || watcher_main(&ready_tx, on_event))
        .map_err(|error| HostError::Desktop(format!("failed to spawn watcher thread: {error}")))?;
    match ready_rx.recv() {
        Ok(Ok(window)) => Ok(Win32WatcherGuard {
            window,
            thread: Some(thread),
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err(HostError::Desktop(
                "watcher thread died during startup".into(),
            ))
        }
    }
}

fn watcher_main(
    ready: &mpsc::Sender<std::result::Result<isize, HostError>>,
    on_event: Box<dyn Fn(ActivityEvent) + Send>,
) {
    let window = match init_watcher_window() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    WATCHER.with_borrow_mut(|slot| {
        *slot = Some(WatcherState {
            on_event,
            fullscreen: Vec::new(),
            on_battery: None,
            display_off: false,
        });
    });

    // Session lock and display power arrive as messages; polling covers the
    // foreground window and battery line status.
    let session_notifications =
        unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) }.is_ok();
    if !session_notifications {
        eprintln!("watcher: session lock notifications unavailable");
    }
    let power_notification = unsafe {
        RegisterPowerSettingNotification(
            window.into(),
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    }
    .ok();
    unsafe {
        SetTimer(Some(window), POLL_TIMER_ID, POLL_INTERVAL_MS, None);
    }
    // First poll immediately so consumers get the initial state.
    poll_tick();

    let _ = ready.send(Ok(window.0 as isize));

    let mut message = MSG::default();
    loop {
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if status.0 <= 0 {
            break;
        }
        unsafe {
            DispatchMessageW(&message);
        }
    }

    unsafe {
        if session_notifications {
            let _ = WTSUnRegisterSessionNotification(window);
        }
        if let Some(notification) = power_notification {
            let _ = UnregisterPowerSettingNotification(notification);
        }
    }
}

fn init_watcher_window() -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None) }
        .map_err(|error| HostError::Desktop(format!("GetModuleHandleW failed: {error}")))?;
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(watcher_proc),
            hInstance: instance.into(),
            lpszClassName: WATCHER_CLASS,
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&class);
        }
    });
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            WATCHER_CLASS,
            w!("LimeWall watcher"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| HostError::Desktop(format!("failed to create watcher window: {error}")))
}

fn emit(event: ActivityEvent) {
    WATCHER.with_borrow(|slot| {
        if let Some(state) = slot.as_ref() {
            (state.on_event)(event);
        }
    });
}

fn poll_tick() {
    let fullscreen = fullscreen_monitors();
    let on_battery = battery_state();
    WATCHER.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else { return };
        if state.fullscreen != fullscreen {
            state.fullscreen = fullscreen.clone();
            (state.on_event)(ActivityEvent::Fullscreen(fullscreen));
        }
        if let Some(on_battery) = on_battery
            && state.on_battery != Some(on_battery)
        {
            state.on_battery = Some(on_battery);
            (state.on_event)(ActivityEvent::Battery(on_battery));
        }
    });
}

/// Device names of monitors fully covered by the foreground window.
fn fullscreen_monitors() -> Vec<String> {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return Vec::new();
        }
        // Never our own process (wallpaper surfaces, UI) …
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        if pid == std::process::id() {
            return Vec::new();
        }
        // … and never the shell's own desktop machinery.
        let mut class = [0u16; 64];
        let length = GetClassNameW(foreground, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..length]);
        if class == "Progman" || class == "WorkerW" {
            return Vec::new();
        }

        let monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() {
            return Vec::new();
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            return Vec::new();
        }
        let mut window_rect = RECT::default();
        if GetWindowRect(foreground, &mut window_rect).is_err() {
            return Vec::new();
        }
        let screen = info.monitorInfo.rcMonitor;
        let covers_monitor = window_rect.left <= screen.left
            && window_rect.top <= screen.top
            && window_rect.right >= screen.right
            && window_rect.bottom >= screen.bottom;
        if !covers_monitor {
            return Vec::new();
        }
        let name_length = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        vec![String::from_utf16_lossy(&info.szDevice[..name_length])]
    }
}

/// `Some(true)` on battery, `Some(false)` on AC, `None` when unknown.
fn battery_state() -> Option<bool> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe {
        GetSystemPowerStatus(&mut status).ok()?;
    }
    match status.ACLineStatus {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

unsafe extern "system" fn watcher_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER if wparam.0 == POLL_TIMER_ID => {
            poll_tick();
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            match wparam.0 {
                WTS_SESSION_LOCK => emit(ActivityEvent::SessionLocked(true)),
                WTS_SESSION_UNLOCK => emit(ActivityEvent::SessionLocked(false)),
                _ => {}
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_POWERSETTINGCHANGE && lparam.0 != 0 {
                let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
                    // Data[0]: 0 = off, 1 = on, 2 = dimmed.
                    let display_off = setting.Data[0] == 0;
                    WATCHER.with_borrow_mut(|slot| {
                        if let Some(state) = slot.as_mut()
                            && state.display_off != display_off
                        {
                            state.display_off = display_off;
                            (state.on_event)(ActivityEvent::DisplayOff(display_off));
                        }
                    });
                }
            }
            LRESULT(1)
        }
        WM_CLOSE => {
            unsafe {
                let _ = KillTimer(Some(window), POLL_TIMER_ID);
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
