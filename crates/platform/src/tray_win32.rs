//! Win32 tray icon on its own thread. A hidden (not message-only) window
//! receives the icon callbacks and the TaskbarCreated broadcast, so the icon
//! survives explorer.exe restarts.

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
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
const TRAY_ICON_ID: u32 = 1;

const CMD_PAUSE_ALL: usize = 1;
const CMD_RESUME_ALL: usize = 2;
const CMD_OPEN_UI: usize = 3;
const CMD_QUIT: usize = 4;

const TRAY_CLASS: windows::core::PCWSTR = w!("LiveWallTray");

thread_local! {
    static TRAY: std::cell::RefCell<Option<TrayState>> = const { std::cell::RefCell::new(None) };
}

struct TrayState {
    on_event: Box<dyn Fn(TrayEvent) + Send>,
    tooltip: Vec<u16>,
    taskbar_created: u32,
}

pub struct Win32TrayGuard {
    window: isize,
    thread: Option<JoinHandle<()>>,
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
    let tooltip = tooltip.to_owned();
    let on_event: Box<dyn Fn(TrayEvent) + Send> = Box::new(on_event);
    let thread = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || tray_main(&ready_tx, &tooltip, on_event))
        .map_err(|error| HostError::Desktop(format!("failed to spawn tray thread: {error}")))?;
    match ready_rx.recv() {
        Ok(Ok(window)) => Ok(Win32TrayGuard {
            window,
            thread: Some(thread),
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
    tooltip: &str,
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
    let mut tip: Vec<u16> = tooltip.encode_utf16().take(127).collect();
    tip.push(0);
    TRAY.with_borrow_mut(|slot| {
        *slot = Some(TrayState {
            on_event,
            tooltip: tip,
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
            w!("LiveWall tray"),
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
    // Branded icon comes with the bundling task; the generic one works
    // everywhere and needs no resource compilation.
    data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    TRAY.with_borrow(|slot| {
        if let Some(state) = slot.as_ref() {
            let len = state.tooltip.len().min(data.szTip.len());
            data.szTip[..len].copy_from_slice(&state.tooltip[..len]);
        }
    });
    data
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
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_UI, w!("Open LiveWall"));
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
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                CMD_PAUSE_ALL => send_event(TrayEvent::PauseAll),
                CMD_RESUME_ALL => send_event(TrayEvent::ResumeAll),
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
