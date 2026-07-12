//! Win32 tray icon on its own thread. A hidden (not message-only) window
//! receives the icon callbacks and the TaskbarCreated broadcast, so the icon
//! survives explorer.exe restarts.

use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ExtractIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, IDI_APPLICATION, LoadIconW,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK,
    WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

use crate::tray::TrayEvent;
use crate::{HostError, Result};

const WM_TRAY_CALLBACK: u32 = WM_APP + 10;
/// Guard → tray thread: the shared tooltip changed, refresh the icon.
const WM_APP_SET_TOOLTIP: u32 = WM_APP + 11;
const TRAY_ICON_ID: u32 = 1;

/// Tooltip text as a NUL-terminated UTF-16 buffer, capped to the field size.
fn encode_tip(text: &str) -> Vec<u16> {
    let mut tip: Vec<u16> = text.encode_utf16().take(127).collect();
    tip.push(0);
    tip
}

fn lock_tip(tip: &Mutex<Vec<u16>>) -> std::sync::MutexGuard<'_, Vec<u16>> {
    tip.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const CMD_PAUSE_ALL: usize = 1;
const CMD_RESUME_ALL: usize = 2;
const CMD_OPEN_UI: usize = 3;
const CMD_QUIT: usize = 4;
const CMD_NEXT: usize = 5;

const TRAY_CLASS: windows::core::PCWSTR = w!("LimeWallTray");

thread_local! {
    static TRAY: std::cell::RefCell<Option<TrayState>> = const { std::cell::RefCell::new(None) };
}

struct TrayState {
    on_event: Box<dyn Fn(TrayEvent) + Send>,
    /// Shared with the guard so `set_tooltip` can update it from another thread.
    tooltip: Arc<Mutex<Vec<u16>>>,
    taskbar_created: u32,
}

pub struct Win32TrayGuard {
    window: isize,
    thread: Option<JoinHandle<()>>,
    tooltip: Arc<Mutex<Vec<u16>>>,
}

impl Win32TrayGuard {
    /// Updates the hover tooltip and asks the tray thread to refresh the icon.
    pub(crate) fn set_tooltip(&self, text: &str) {
        *lock_tip(&self.tooltip) = encode_tip(text);
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.window as *mut core::ffi::c_void)),
                WM_APP_SET_TOOLTIP,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

impl Drop for Win32TrayGuard {
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

pub fn spawn(
    tooltip: &str,
    on_event: impl Fn(TrayEvent) + Send + 'static,
) -> Result<Win32TrayGuard> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let tooltip = Arc::new(Mutex::new(encode_tip(tooltip)));
    let thread_tooltip = Arc::clone(&tooltip);
    let on_event: Box<dyn Fn(TrayEvent) + Send> = Box::new(on_event);
    let thread = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || tray_main(&ready_tx, thread_tooltip, on_event))
        .map_err(|error| HostError::Desktop(format!("failed to spawn tray thread: {error}")))?;
    match ready_rx.recv() {
        Ok(Ok(window)) => Ok(Win32TrayGuard {
            window,
            thread: Some(thread),
            tooltip,
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err(HostError::Desktop("tray thread died during startup".into()))
        }
    }
}

fn tray_main(
    ready: &mpsc::Sender<std::result::Result<isize, HostError>>,
    tooltip: Arc<Mutex<Vec<u16>>>,
    on_event: Box<dyn Fn(TrayEvent) + Send>,
) {
    let window = match init_tray_window() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TRAY.with_borrow_mut(|slot| {
        *slot = Some(TrayState {
            on_event,
            tooltip,
            taskbar_created,
        });
    });
    if let Err(error) = add_icon(window) {
        let _ = ready.send(Err(error));
        return;
    }
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
}

fn init_tray_window() -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None) }
        .map_err(|error| HostError::Desktop(format!("GetModuleHandleW failed: {error}")))?;
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(tray_proc),
            hInstance: instance.into(),
            lpszClassName: TRAY_CLASS,
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&class);
        }
    });
    // Hidden but real (not message-only): broadcasts like TaskbarCreated
    // are not delivered to message-only windows.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            TRAY_CLASS,
            w!("LimeWall tray"),
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
    .map_err(|error| HostError::Desktop(format!("failed to create tray window: {error}")))
}

fn notify_icon_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        ..Default::default()
    };
    // The brand icon is embedded into the executable as its first icon
    // resource; a plain build without it falls back to the system icon.
    data.hIcon = own_executable_icon()
        .unwrap_or_else(|| unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default());
    TRAY.with_borrow(|slot| {
        if let Some(state) = slot.as_ref() {
            let tip = lock_tip(&state.tooltip);
            let len = tip.len().min(data.szTip.len());
            data.szTip[..len].copy_from_slice(&tip[..len]);
        }
    });
    data
}

/// First icon of the running executable, when one is embedded.
fn own_executable_icon() -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use std::os::windows::ffi::OsStrExt;
    let exe = std::env::current_exe().ok()?;
    let mut wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    wide.push(0);
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let icon = unsafe {
        ExtractIconW(
            Some(instance.into()),
            windows::core::PCWSTR(wide.as_ptr()),
            0,
        )
    };
    // NULL means no icons; 1 is the documented "not an executable" marker.
    if icon.is_invalid() || icon.0 as usize == 1 {
        None
    } else {
        Some(icon)
    }
}

fn add_icon(window: HWND) -> Result<()> {
    let data = notify_icon_data(window);
    if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        Ok(())
    } else {
        Err(HostError::Desktop(
            "Shell_NotifyIcon(NIM_ADD) failed".into(),
        ))
    }
}

fn remove_icon(window: HWND) {
    let data = notify_icon_data(window);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn modify_icon(window: HWND) {
    let data = notify_icon_data(window);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn send_event(event: TrayEvent) {
    TRAY.with_borrow(|slot| {
        if let Some(state) = slot.as_ref() {
            (state.on_event)(event);
        }
    });
}

fn show_menu(window: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, CMD_PAUSE_ALL, w!("Pause all"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_RESUME_ALL, w!("Resume all"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_NEXT, w!("Next wallpaper"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_UI, w!("Open LimeWall"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));

        let mut cursor = Default::default();
        let _ = GetCursorPos(&mut cursor);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(window);
        let _ = TrackPopupMenu(
            menu,
            TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            Some(0),
            window,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn tray_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAY_CALLBACK => {
            match lparam.0 as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(window),
                WM_LBUTTONDBLCLK => send_event(TrayEvent::OpenUi),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_SET_TOOLTIP => {
            modify_icon(window);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                CMD_PAUSE_ALL => send_event(TrayEvent::PauseAll),
                CMD_RESUME_ALL => send_event(TrayEvent::ResumeAll),
                CMD_NEXT => send_event(TrayEvent::NextWallpaper),
                CMD_OPEN_UI => send_event(TrayEvent::OpenUi),
                CMD_QUIT => send_event(TrayEvent::Quit),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_icon(window);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let taskbar_created =
                TRAY.with_borrow(|slot| slot.as_ref().map(|state| state.taskbar_created));
            if Some(message) == taskbar_created {
                // Explorer restarted: the notification area forgot our icon.
                let _ = add_icon(window);
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}
