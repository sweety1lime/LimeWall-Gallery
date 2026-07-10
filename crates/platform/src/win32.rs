//! Win32 backend: wallpaper surfaces behind the desktop icons via the WorkerW
//! technique. Strategy and sources: docs/research/workerw.md.
//!
//! Threading model: all windows live on a dedicated "wallpaper-host" thread
//! that runs the message pump. `Win32Host` methods marshal requests to that
//! thread with a synchronous cross-thread `SendMessageW` to a message-only
//! control window, so the public API stays plain and blocking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Once, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, EnumDisplayMonitors, FillRect,
    GetMonitorInfoW, HDC, HMONITOR, InvalidateRect, MONITORINFOEXW, MapWindowPoints, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EnumWindows, FindWindowExW, FindWindowW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    HWND_MESSAGE, IsWindow, KillTimer, MONITORINFOF_PRIMARY, MSG, PostQuitMessage, RegisterClassW,
    SMTO_NORMAL, SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
    SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SendMessageTimeoutW, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY, WM_ERASEBKGND,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE,
};
use windows::core::{BOOL, w};

use crate::{HostError, MonitorInfo, Rect, Result, SurfaceHandle, WallpaperHost};

/// Undocumented message that makes Progman spawn the WorkerW layer behind the
/// desktop icons (part of the wallpaper-transition machinery; idempotent).
const WM_SPAWN_WORKERW: u32 = 0x052C;
const WM_APP_REQUEST: u32 = WM_APP + 1;
const WATCHDOG_TIMER_ID: usize = 1;
const WATCHDOG_INTERVAL_MS: u32 = 1000;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_COLOR: [u8; 3] = [0, 0, 0];

const CONTROL_CLASS: windows::core::PCWSTR = w!("LimeWallControl");
const SURFACE_CLASS: windows::core::PCWSTR = w!("LimeWallSurface");

fn hwnd(value: isize) -> HWND {
    HWND(value as *mut core::ffi::c_void)
}

fn desktop_err(context: &str, error: windows::core::Error) -> HostError {
    HostError::Desktop(format!("{context}: {error}"))
}

fn colorref(rgb: [u8; 3]) -> COLORREF {
    COLORREF(rgb[0] as u32 | (rgb[1] as u32) << 8 | (rgb[2] as u32) << 16)
}

/// Request marshalled to the host thread. Results are written back in place;
/// safe because the cross-thread `SendMessageW` blocks until it is handled.
enum Request {
    Create {
        monitor_name: String,
        bounds: Rect,
        result: Result<SurfaceHandle>,
    },
    Destroy {
        surface: SurfaceHandle,
        result: Result<()>,
    },
    SetColor {
        surface: SurfaceHandle,
        rgb: [u8; 3],
        result: Result<()>,
    },
    SetVisible {
        visible: bool,
        result: Result<()>,
    },
    NativeHandle {
        surface: SurfaceHandle,
        result: Result<u64>,
    },
    Shutdown,
}

fn no_response() -> HostError {
    HostError::Desktop("host thread did not answer the request".into())
}

pub struct Win32Host {
    control: isize,
    thread: Option<JoinHandle<()>>,
}

impl Win32Host {
    pub fn new() -> Result<Self> {
        unsafe {
            // Physical-pixel coordinates on every monitor. Fails when a DPI
            // context is already set for the process — that is fine.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("wallpaper-host".into())
            .spawn(move || worker_main(&ready_tx))
            .map_err(|e| HostError::Desktop(format!("failed to spawn host thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(control)) => Ok(Self {
                control,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(HostError::Desktop("host thread died during startup".into()))
            }
        }
    }

    fn request(&self, request: &mut Request) {
        unsafe {
            SendMessageW(
                hwnd(self.control),
                WM_APP_REQUEST,
                Some(WPARAM(0)),
                Some(LPARAM(request as *mut Request as isize)),
            );
        }
    }
}

impl WallpaperHost for Win32Host {
    fn enumerate_monitors(&self) -> Result<Vec<MonitorInfo>> {
        enumerate_monitors_impl()
    }

    fn create_surface(&mut self, monitor: crate::MonitorId) -> Result<SurfaceHandle> {
        let target = enumerate_monitors_impl()?
            .into_iter()
            .find(|m| m.id == monitor)
            .ok_or(HostError::MonitorNotFound(monitor))?;
        let mut request = Request::Create {
            monitor_name: target.name,
            bounds: target.bounds,
            result: Err(no_response()),
        };
        self.request(&mut request);
        match request {
            Request::Create { result, .. } => result,
            _ => Err(no_response()),
        }
    }

    fn destroy_surface(&mut self, surface: SurfaceHandle) -> Result<()> {
        let mut request = Request::Destroy {
            surface,
            result: Err(no_response()),
        };
        self.request(&mut request);
        match request {
            Request::Destroy { result, .. } => result,
            _ => Err(no_response()),
        }
    }

    fn pause(&mut self) -> Result<()> {
        self.set_visible(false)
    }

    fn resume(&mut self) -> Result<()> {
        self.set_visible(true)
    }

    fn set_surface_color(&mut self, surface: SurfaceHandle, rgb: [u8; 3]) -> Result<()> {
        let mut request = Request::SetColor {
            surface,
            rgb,
            result: Err(no_response()),
        };
        self.request(&mut request);
        match request {
            Request::SetColor { result, .. } => result,
            _ => Err(no_response()),
        }
    }

    fn surface_native_handle(&self, surface: SurfaceHandle) -> Result<u64> {
        let mut request = Request::NativeHandle {
            surface,
            result: Err(no_response()),
        };
        self.request(&mut request);
        match request {
            Request::NativeHandle { result, .. } => result,
            _ => Err(no_response()),
        }
    }
}

impl Win32Host {
    fn set_visible(&self, visible: bool) -> Result<()> {
        let mut request = Request::SetVisible {
            visible,
            result: Err(no_response()),
        };
        self.request(&mut request);
        match request {
            Request::SetVisible { result, .. } => result,
            _ => Err(no_response()),
        }
    }
}

impl Drop for Win32Host {
    fn drop(&mut self) {
        self.request(&mut Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Host thread
// ---------------------------------------------------------------------------

struct SurfaceState {
    hwnd: isize,
    /// Stable display device name (for example `\\.\DISPLAY1`). Monitor
    /// indices can change whenever the display topology changes.
    monitor_name: String,
    bounds: Rect,
    color: [u8; 3],
    connected: bool,
}

struct Worker {
    control: isize,
    /// WorkerW (or Progman as last resort); 0 until the first surface.
    parent: isize,
    next_id: u64,
    surfaces: HashMap<u64, SurfaceState>,
    visible: bool,
}

thread_local! {
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

fn worker_main(ready: &mpsc::Sender<std::result::Result<isize, HostError>>) {
    let control = match init_worker() {
        Ok(control) => control,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    WORKER.with_borrow_mut(|slot| {
        *slot = Some(Worker {
            control,
            parent: 0,
            next_id: 1,
            surfaces: HashMap::new(),
            visible: true,
        });
    });
    let _ = ready.send(Ok(control));

    let mut msg = MSG::default();
    loop {
        let status = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if status.0 <= 0 {
            break;
        }
        unsafe {
            DispatchMessageW(&msg);
        }
    }
}

fn init_worker() -> Result<isize> {
    let instance =
        unsafe { GetModuleHandleW(None) }.map_err(|e| desktop_err("GetModuleHandleW failed", e))?;

    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let control_class = WNDCLASSW {
            lpfnWndProc: Some(control_proc),
            hInstance: instance.into(),
            lpszClassName: CONTROL_CLASS,
            ..Default::default()
        };
        let surface_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(surface_proc),
            hInstance: instance.into(),
            lpszClassName: SURFACE_CLASS,
            ..Default::default()
        };
        unsafe {
            RegisterClassW(&control_class);
            RegisterClassW(&surface_class);
        }
    });

    let control = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CONTROL_CLASS,
            w!("LimeWall control"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| desktop_err("failed to create control window", e))?;
    Ok(control.0 as isize)
}

unsafe extern "system" fn control_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_APP_REQUEST => {
            // The sender blocks in SendMessageW until we return, so the
            // pointer stays valid for the whole call.
            let request = unsafe { &mut *(lparam.0 as *mut Request) };
            handle_request(request);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == WATCHDOG_TIMER_ID => {
            watchdog_tick();
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

unsafe extern "system" fn surface_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(window, &mut paint);
            let color = COLORREF(GetWindowLongPtrW(window, GWLP_USERDATA) as u32);
            let brush = CreateSolidBrush(color);
            FillRect(hdc, &paint.rcPaint, brush);
            let _ = DeleteObject(brush.into());
            let _ = EndPaint(window, &paint);
            LRESULT(0)
        },
        // WM_PAINT covers the full surface, skip background erase flicker.
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn handle_request(request: &mut Request) {
    WORKER.with_borrow_mut(|slot| {
        let Some(worker) = slot.as_mut() else {
            return;
        };
        match request {
            Request::Create {
                monitor_name,
                bounds,
                result,
            } => *result = worker.create_surface(monitor_name.clone(), *bounds),
            Request::Destroy { surface, result } => *result = worker.destroy_surface(*surface),
            Request::SetColor {
                surface,
                rgb,
                result,
            } => *result = worker.set_color(*surface, *rgb),
            Request::SetVisible { visible, result } => *result = worker.set_visible(*visible),
            Request::NativeHandle { surface, result } => {
                *result = worker.native_handle(*surface);
            }
            Request::Shutdown => {
                worker.shutdown();
                *slot = None;
            }
        }
    });
}

fn watchdog_tick() {
    WORKER.with_borrow_mut(|slot| {
        let Some(worker) = slot.as_mut() else {
            return;
        };
        worker.watchdog_tick();
    });
}

impl Worker {
    /// Returns the current WorkerW/Progman parent, re-running discovery when
    /// the previous one died (wallpaper change, explorer restart).
    fn ensure_parent(&mut self) -> Result<HWND> {
        if self.parent != 0 && unsafe { IsWindow(Some(hwnd(self.parent))) }.as_bool() {
            return Ok(hwnd(self.parent));
        }
        let parent = find_wallpaper_parent()?;
        self.parent = parent.0 as isize;
        unsafe {
            SetTimer(
                Some(hwnd(self.control)),
                WATCHDOG_TIMER_ID,
                WATCHDOG_INTERVAL_MS,
                None,
            );
        }
        Ok(parent)
    }

    fn create_surface(&mut self, monitor_name: String, bounds: Rect) -> Result<SurfaceHandle> {
        let parent = self.ensure_parent()?;
        let window = create_surface_window(parent, bounds, DEFAULT_COLOR, self.visible)?;
        let id = self.next_id;
        self.next_id += 1;
        self.surfaces.insert(
            id,
            SurfaceState {
                hwnd: window.0 as isize,
                monitor_name,
                bounds,
                color: DEFAULT_COLOR,
                connected: true,
            },
        );
        Ok(SurfaceHandle(id))
    }

    fn destroy_surface(&mut self, surface: SurfaceHandle) -> Result<()> {
        let state = self
            .surfaces
            .remove(&surface.0)
            .ok_or(HostError::SurfaceNotFound(surface))?;
        unsafe {
            if IsWindow(Some(hwnd(state.hwnd))).as_bool() {
                let _ = DestroyWindow(hwnd(state.hwnd));
            }
        }
        restore_desktop_wallpaper();
        Ok(())
    }

    fn set_color(&mut self, surface: SurfaceHandle, rgb: [u8; 3]) -> Result<()> {
        let state = self
            .surfaces
            .get_mut(&surface.0)
            .ok_or(HostError::SurfaceNotFound(surface))?;
        state.color = rgb;
        unsafe {
            SetWindowLongPtrW(hwnd(state.hwnd), GWLP_USERDATA, colorref(rgb).0 as isize);
            let _ = InvalidateRect(Some(hwnd(state.hwnd)), None, true);
        }
        Ok(())
    }

    fn native_handle(&self, surface: SurfaceHandle) -> Result<u64> {
        let state = self
            .surfaces
            .get(&surface.0)
            .ok_or(HostError::SurfaceNotFound(surface))?;
        Ok(state.hwnd as u64)
    }

    fn set_visible(&mut self, visible: bool) -> Result<()> {
        self.visible = visible;
        for state in self.surfaces.values() {
            let command = if visible && state.connected {
                SW_SHOWNA
            } else {
                SW_HIDE
            };
            unsafe {
                let _ = ShowWindow(hwnd(state.hwnd), command);
            }
        }
        if !visible {
            restore_desktop_wallpaper();
        }
        Ok(())
    }

    fn watchdog_tick(&mut self) {
        if self.surfaces.is_empty() {
            return;
        }
        let monitors = enumerate_monitors_impl().ok();
        let parent_alive =
            self.parent != 0 && unsafe { IsWindow(Some(hwnd(self.parent))) }.as_bool();
        let surfaces_alive = self
            .surfaces
            .values()
            .all(|s| unsafe { IsWindow(Some(hwnd(s.hwnd))) }.as_bool());
        if parent_alive && surfaces_alive {
            if let Some(monitors) = monitors.as_deref() {
                self.reconcile_display_topology(hwnd(self.parent), monitors);
            }
            return;
        }
        // The desktop hierarchy was rebuilt (wallpaper change or explorer
        // restart) and took our children with it — rediscover and re-attach.
        self.parent = 0;
        let Ok(parent) = self.ensure_parent() else {
            return; // explorer still starting; retry on the next tick
        };
        let visible = self.visible;
        for state in self.surfaces.values_mut() {
            if let Some(monitors) = monitors.as_deref() {
                if let Some(bounds) = target_monitor_bounds(monitors, &state.monitor_name) {
                    state.bounds = bounds;
                    state.connected = true;
                } else {
                    state.connected = false;
                }
            }
            unsafe {
                if IsWindow(Some(hwnd(state.hwnd))).as_bool() {
                    let _ = DestroyWindow(hwnd(state.hwnd));
                }
            }
            if let Ok(window) = create_surface_window(
                parent,
                state.bounds,
                state.color,
                visible && state.connected,
            ) {
                state.hwnd = window.0 as isize;
            }
        }
    }

    /// Keeps surfaces attached to their display device across resolution,
    /// position and topology changes. A disconnected display is hidden and
    /// restored automatically if the same device name reappears.
    fn reconcile_display_topology(&mut self, parent: HWND, monitors: &[MonitorInfo]) {
        for state in self.surfaces.values_mut() {
            let Some(bounds) = target_monitor_bounds(monitors, &state.monitor_name) else {
                state.connected = false;
                unsafe {
                    let _ = ShowWindow(hwnd(state.hwnd), SW_HIDE);
                }
                continue;
            };

            let changed = !state.connected || state.bounds != bounds;
            if changed {
                if position_surface_window(hwnd(state.hwnd), parent, bounds).is_err() {
                    // Keep the last successfully applied bounds and mark the
                    // target disconnected so the next watchdog tick retries.
                    state.connected = false;
                    unsafe {
                        let _ = ShowWindow(hwnd(state.hwnd), SW_HIDE);
                    }
                    continue;
                }
                state.bounds = bounds;
            }
            state.connected = true;
            let command = if self.visible { SW_SHOWNA } else { SW_HIDE };
            unsafe {
                let _ = ShowWindow(hwnd(state.hwnd), command);
            }
        }
    }

    fn shutdown(&mut self) {
        for (_, state) in self.surfaces.drain() {
            unsafe {
                if IsWindow(Some(hwnd(state.hwnd))).as_bool() {
                    let _ = DestroyWindow(hwnd(state.hwnd));
                }
            }
        }
        unsafe {
            let _ = KillTimer(Some(hwnd(self.control)), WATCHDOG_TIMER_ID);
        }
        restore_desktop_wallpaper();
        unsafe {
            let _ = DestroyWindow(hwnd(self.control));
        }
    }
}

// ---------------------------------------------------------------------------
// WorkerW discovery (see docs/research/workerw.md)
// ---------------------------------------------------------------------------

fn find_wallpaper_parent() -> Result<HWND> {
    let progman = unsafe { FindWindowW(w!("Progman"), None) }
        .map_err(|e| desktop_err("Progman window not found", e))?;
    for lparam in [0isize, 1] {
        unsafe {
            SendMessageTimeoutW(
                progman,
                WM_SPAWN_WORKERW,
                WPARAM(0xD),
                LPARAM(lparam),
                SMTO_NORMAL,
                1000,
                None,
            );
        }
    }
    // 24H2 spawns WorkerW with a delay (especially right after logon).
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    loop {
        // Windows 11 24H2+ (build 26100+): WorkerW is a child of Progman.
        if let Ok(worker) = unsafe { FindWindowExW(Some(progman), None, w!("WorkerW"), None) }
            && !worker.is_invalid()
        {
            return Ok(worker);
        }
        // Windows 10 .. 11 23H2: top-level WorkerW right after the one that
        // hosts SHELLDLL_DefView.
        if let Some(worker) = find_workerw_sibling() {
            return Ok(worker);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Last resort: some builds keep SHELLDLL_DefView directly under Progman
    // and painting into Progman itself lands behind the icons.
    Ok(progman)
}

fn find_workerw_sibling() -> Option<HWND> {
    unsafe extern "system" fn enum_proc(window: HWND, out: LPARAM) -> BOOL {
        unsafe {
            let def_view = FindWindowExW(Some(window), None, w!("SHELLDLL_DefView"), None);
            if def_view.is_ok_and(|w| !w.is_invalid())
                && let Ok(worker) = FindWindowExW(None, Some(window), w!("WorkerW"), None)
                && !worker.is_invalid()
            {
                *(out.0 as *mut isize) = worker.0 as isize;
                return BOOL(0); // found — stop enumerating
            }
        }
        BOOL(1)
    }

    let mut found: isize = 0;
    unsafe {
        // EnumWindows reports an error when the callback stops early — that is
        // our success path, so the status is ignored in favor of `found`.
        let _ = EnumWindows(Some(enum_proc), LPARAM(&raw mut found as isize));
    }
    (found != 0).then(|| hwnd(found))
}

// ---------------------------------------------------------------------------
// Surface window
// ---------------------------------------------------------------------------

fn create_surface_window(
    parent: HWND,
    bounds: Rect,
    color: [u8; 3],
    visible: bool,
) -> Result<HWND> {
    let instance =
        unsafe { GetModuleHandleW(None) }.map_err(|e| desktop_err("GetModuleHandleW failed", e))?;
    let point = surface_origin_in_parent(parent, bounds);
    let window = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            SURFACE_CLASS,
            w!("LimeWall surface"),
            WS_CHILD,
            point.x,
            point.y,
            bounds.width as i32,
            bounds.height as i32,
            Some(parent),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| desktop_err("failed to create surface window", e))?;
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, colorref(color).0 as isize);
        if visible {
            let _ = ShowWindow(window, SW_SHOWNA);
        }
    }
    Ok(window)
}

fn surface_origin_in_parent(parent: HWND, bounds: Rect) -> POINT {
    // Child coordinates are relative to the parent's client area, which spans
    // the whole virtual desktop; map instead of assuming its origin.
    let mut points = [POINT {
        x: bounds.x,
        y: bounds.y,
    }];
    unsafe {
        MapWindowPoints(None, Some(parent), &mut points);
    }
    points[0]
}

fn position_surface_window(window: HWND, parent: HWND, bounds: Rect) -> Result<()> {
    let point = surface_origin_in_parent(parent, bounds);
    unsafe {
        SetWindowPos(
            window,
            None,
            point.x,
            point.y,
            bounds.width as i32,
            bounds.height as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    }
    .map_err(|error| desktop_err("failed to reposition surface window", error))
}

/// Re-applies the current system wallpaper so the desktop repaints the area we
/// covered; keeps the user's wallpaper untouched.
fn restore_desktop_wallpaper() {
    let mut path = [0u16; 512];
    unsafe {
        if SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            path.len() as u32,
            Some(path.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_err()
        {
            return;
        }
    }
    // Re-applying makes Windows reload the file from disk. If the registry
    // points at a deleted file (or is empty for slideshow/Spotlight setups),
    // that reload CLEARS the wallpaper the user still sees from the cache —
    // in that case doing nothing is the correct restore.
    let length = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    if length == 0 {
        return;
    }
    let current = PathBuf::from(String::from_utf16_lossy(&path[..length]));
    if !current.is_file() {
        return;
    }
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(path.as_mut_ptr().cast()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

fn enumerate_monitors_impl() -> Result<Vec<MonitorInfo>> {
    unsafe extern "system" fn enum_proc(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        out: LPARAM,
    ) -> BOOL {
        unsafe {
            (*(out.0 as *mut Vec<HMONITOR>)).push(monitor);
        }
        BOOL(1)
    }

    let mut handles: Vec<HMONITOR> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&raw mut handles as isize),
        )
    };
    if !ok.as_bool() {
        return Err(HostError::Desktop("EnumDisplayMonitors failed".into()));
    }

    let mut monitors = Vec::with_capacity(handles.len());
    for monitor in handles {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            continue;
        }
        let rc = info.monitorInfo.rcMonitor;
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        }
        let name_len = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        monitors.push(MonitorInfo {
            id: 0, // assigned after sorting
            name: String::from_utf16_lossy(&info.szDevice[..name_len]),
            bounds: Rect {
                x: rc.left,
                y: rc.top,
                width: (rc.right - rc.left).max(0) as u32,
                height: (rc.bottom - rc.top).max(0) as u32,
            },
            scale: dpi_x as f64 / 96.0,
            is_primary: (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
        });
    }
    // Deterministic ids: primary first, then virtual-desktop position.
    monitors.sort_by_key(|m| (!m.is_primary, m.bounds.x, m.bounds.y));
    for (index, monitor) in monitors.iter_mut().enumerate() {
        monitor.id = index;
    }
    Ok(monitors)
}

fn target_monitor_bounds(monitors: &[MonitorInfo], monitor_name: &str) -> Option<Rect> {
    monitors
        .iter()
        .find(|monitor| monitor.name == monitor_name)
        .map(|monitor| monitor.bounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: usize, name: &str, bounds: Rect) -> MonitorInfo {
        MonitorInfo {
            id,
            name: name.into(),
            bounds,
            scale: 1.0,
            is_primary: id == 0,
        }
    }

    #[test]
    fn surface_target_survives_monitor_id_reordering() {
        let expected = Rect {
            x: 1920,
            y: 0,
            width: 2560,
            height: 1440,
        };
        let monitors = [
            monitor(
                0,
                r"\\.\DISPLAY2",
                Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            ),
            monitor(1, r"\\.\DISPLAY1", expected),
        ];

        assert_eq!(
            target_monitor_bounds(&monitors, r"\\.\DISPLAY1"),
            Some(expected)
        );
    }

    #[test]
    fn missing_surface_target_is_reported() {
        assert_eq!(target_monitor_bounds(&[], r"\\.\DISPLAY1"), None);
    }
}
