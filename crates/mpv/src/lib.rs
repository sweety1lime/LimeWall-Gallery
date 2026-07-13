//! Safe wrapper over hand-written libmpv bindings (see `ffi`).
//!
//! `libmpv-2.dll` is loaded at runtime (LGPL compliance via dynamic linking;
//! docs/research/libmpv.md). One [`Api`] can serve many [`Player`]s.

mod ffi;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to load {name}: {source}")]
    Load {
        name: String,
        #[source]
        source: libloading::Error,
    },
    #[error("libmpv has client API v{major}.{minor}, expected v2.x")]
    ApiVersion { major: u32, minor: u32 },
    #[error("mpv_create returned null")]
    Create,
    #[error("{context}: {message} (mpv error {code})")]
    Api {
        context: String,
        code: i32,
        message: String,
    },
    #[error("string contains an interior NUL byte: {0:?}")]
    Nul(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error::Nul(value.to_owned()))
}

/// Splits MPV_MAKE_VERSION-style value into (major, minor). `c_ulong` differs
/// per platform, hence the widening conversion.
fn split_version(version: std::os::raw::c_ulong) -> (u32, u32) {
    let version = u64::from(version);
    ((version >> 16) as u32, (version & 0xFFFF) as u32)
}

/// Loaded libmpv library. Cheap to clone via [`Arc`].
pub struct Api {
    ffi: ffi::Api,
}

impl Api {
    /// Loads `libmpv-2.dll` using the platform's default search order
    /// (executable directory first).
    pub fn load() -> Result<Arc<Self>> {
        Self::load_from("libmpv-2.dll")
    }

    /// Loads libmpv from an explicit file name or path.
    pub fn load_from(name: impl AsRef<Path>) -> Result<Arc<Self>> {
        let name = name.as_ref();
        let display = name.display().to_string();
        let map_err = |source| Error::Load {
            name: display.clone(),
            source,
        };
        // SAFETY: loading and resolving symbols of a foreign dll; we trust
        // the pinned libmpv build (integrity checked by the fetch script).
        let ffi = unsafe {
            let lib = libloading::Library::new(name).map_err(map_err)?;
            ffi::Api::from_library(lib).map_err(map_err)?
        };
        let (major, minor) = split_version(unsafe { (ffi.client_api_version)() });
        if major != 2 {
            return Err(Error::ApiVersion { major, minor });
        }
        Ok(Arc::new(Self { ffi }))
    }

    /// Client API version as (major, minor).
    pub fn version(&self) -> (u32, u32) {
        split_version(unsafe { (self.ffi.client_api_version)() })
    }

    fn error_message(&self, code: c_int) -> String {
        unsafe {
            let ptr = (self.ffi.error_string)(code);
            if ptr.is_null() {
                format!("unknown error {code}")
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }

    fn check(&self, context: &str, code: c_int) -> Result<()> {
        if code >= 0 {
            Ok(())
        } else {
            Err(Error::Api {
                context: context.to_owned(),
                code,
                message: self.error_message(code),
            })
        }
    }
}

/// Events surfaced to the renderer (subset of mpv events we care about).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A file was loaded and playback is about to start.
    FileLoaded,
    /// Playback of the current file ended.
    EndFile,
    /// The player is shutting down.
    Shutdown,
    /// Any other event, by raw id.
    Other(i32),
}

/// One mpv player instance bound to a window (`wid`).
pub struct Player {
    api: Arc<Api>,
    handle: *mut ffi::mpv_handle,
}

// SAFETY: libmpv documents mpv_handle as safe to use from any thread.
unsafe impl Send for Player {}

impl Player {
    /// Creates a player, applies `options` before initialization, then
    /// initializes it. Option values use mpv's string syntax.
    pub fn new(api: Arc<Api>, options: &[(&str, &str)]) -> Result<Self> {
        let handle = unsafe { (api.ffi.create)() };
        if handle.is_null() {
            return Err(Error::Create);
        }
        let player = Self { api, handle };
        for (name, value) in options {
            let name_c = cstring(name)?;
            let value_c = cstring(value)?;
            let code = unsafe {
                (player.api.ffi.set_option_string)(player.handle, name_c.as_ptr(), value_c.as_ptr())
            };
            player
                .api
                .check(&format!("set option {name}={value}"), code)?;
        }
        let code = unsafe { (player.api.ffi.initialize)(player.handle) };
        player.api.check("mpv_initialize", code)?;
        Ok(player)
    }

    /// Runs an mpv command, e.g. `["loadfile", "video.mp4"]`.
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args
            .iter()
            .map(|a| cstring(a))
            .collect::<Result<Vec<_>>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { (self.api.ffi.command)(self.handle, ptrs.as_mut_ptr()) };
        self.api.check(&format!("command {args:?}"), code)
    }

    pub fn set_property_str(&self, name: &str, value: &str) -> Result<()> {
        let name_c = cstring(name)?;
        let value_c = cstring(value)?;
        let code = unsafe {
            (self.api.ffi.set_property_string)(self.handle, name_c.as_ptr(), value_c.as_ptr())
        };
        self.api.check(&format!("set {name}={value}"), code)
    }

    pub fn set_property_bool(&self, name: &str, value: bool) -> Result<()> {
        let name_c = cstring(name)?;
        let mut flag: c_int = if value { 1 } else { 0 };
        let code = unsafe {
            (self.api.ffi.set_property)(
                self.handle,
                name_c.as_ptr(),
                ffi::MPV_FORMAT_FLAG,
                (&raw mut flag).cast::<c_void>(),
            )
        };
        self.api.check(&format!("set {name}={value}"), code)
    }

    pub fn set_property_f64(&self, name: &str, value: f64) -> Result<()> {
        let name_c = cstring(name)?;
        let mut value = value;
        let code = unsafe {
            (self.api.ffi.set_property)(
                self.handle,
                name_c.as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                (&raw mut value).cast::<c_void>(),
            )
        };
        self.api.check(&format!("set {name}={value}"), code)
    }

    pub fn get_property_str(&self, name: &str) -> Result<String> {
        let name_c = cstring(name)?;
        unsafe {
            let ptr = (self.api.ffi.get_property_string)(self.handle, name_c.as_ptr());
            if ptr.is_null() {
                return Err(Error::Api {
                    context: format!("get {name}"),
                    code: -1,
                    message: "property unavailable".into(),
                });
            }
            let value = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            (self.api.ffi.free)(ptr.cast::<c_void>());
            Ok(value)
        }
    }

    pub fn get_property_i64(&self, name: &str) -> Result<i64> {
        let name_c = cstring(name)?;
        let mut value: i64 = 0;
        let code = unsafe {
            (self.api.ffi.get_property)(
                self.handle,
                name_c.as_ptr(),
                ffi::MPV_FORMAT_INT64,
                (&raw mut value).cast::<c_void>(),
            )
        };
        self.api.check(&format!("get {name}"), code)?;
        Ok(value)
    }

    pub fn get_property_bool(&self, name: &str) -> Result<bool> {
        let name_c = cstring(name)?;
        let mut value: c_int = 0;
        let code = unsafe {
            (self.api.ffi.get_property)(
                self.handle,
                name_c.as_ptr(),
                ffi::MPV_FORMAT_FLAG,
                (&raw mut value).cast::<c_void>(),
            )
        };
        self.api.check(&format!("get {name}"), code)?;
        Ok(value != 0)
    }

    /// Waits up to `timeout` seconds for the next event; `None` on timeout.
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        // SAFETY: the returned event pointer is valid until the next
        // wait_event call on this handle; we copy what we need immediately.
        let id = unsafe { (*(self.api.ffi.wait_event)(self.handle, timeout)).event_id };
        match id {
            ffi::MPV_EVENT_NONE => None,
            ffi::MPV_EVENT_FILE_LOADED => Some(Event::FileLoaded),
            ffi::MPV_EVENT_END_FILE => Some(Event::EndFile),
            ffi::MPV_EVENT_SHUTDOWN => Some(Event::Shutdown),
            other => Some(Event::Other(other)),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { (self.api.ffi.terminate_destroy)(self.handle) };
    }
}

/// Software-mode render context: libmpv renders frames straight into a CPU pixel
/// buffer, with no GL/EGL context. This is the foundation for the phase-2
/// decoder that renders off-screen so it can run sandboxed
/// (docs/research/renderer-sandbox-phase2.md).
///
/// Borrows its [`Player`] so it is always freed before the player is destroyed,
/// which libmpv requires.
pub struct RenderContext<'a> {
    player: &'a Player,
    ctx: *mut ffi::mpv_render_context,
}

impl<'a> RenderContext<'a> {
    /// Creates a software render context. The player must have been created with
    /// `vo=libmpv`.
    pub fn new_sw(player: &'a Player) -> Result<Self> {
        let api_type = c"sw";
        let mut params = [
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_API_TYPE,
                data: api_type.as_ptr().cast::<c_void>().cast_mut(),
            },
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut ffi::mpv_render_context = std::ptr::null_mut();
        let code = unsafe {
            (player.api.ffi.render_context_create)(&raw mut ctx, player.handle, params.as_mut_ptr())
        };
        player.api.check("mpv_render_context_create", code)?;
        Ok(Self { player, ctx })
    }

    /// True when libmpv has a new frame ready to be rendered.
    pub fn frame_ready(&self) -> bool {
        let flags = unsafe { (self.player.api.ffi.render_context_update)(self.ctx) };
        flags & ffi::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Renders the current frame into `buffer` as `width`x`height` pixels of the
    /// software `format` (e.g. `"rgb0"`, 4 bytes/pixel) with `stride` bytes per
    /// row. `buffer` must hold at least `stride * height` bytes.
    pub fn render_sw(
        &self,
        width: i32,
        height: i32,
        format: &str,
        stride: usize,
        buffer: &mut [u8],
    ) -> Result<()> {
        let needed = stride.saturating_mul(height.max(0) as usize);
        assert!(buffer.len() >= needed, "render buffer too small");
        let format_c = cstring(format)?;
        let mut size = [width, height];
        let mut stride = stride;
        let mut params = [
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr().cast::<c_void>(),
            },
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_SW_FORMAT,
                data: format_c.as_ptr().cast::<c_void>().cast_mut(),
            },
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_SW_STRIDE,
                data: (&raw mut stride).cast::<c_void>(),
            },
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_SW_POINTER,
                data: buffer.as_mut_ptr().cast::<c_void>(),
            },
            ffi::mpv_render_param {
                kind: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let code =
            unsafe { (self.player.api.ffi.render_context_render)(self.ctx, params.as_mut_ptr()) };
        self.player.api.check("mpv_render_context_render", code)
    }
}

impl Drop for RenderContext<'_> {
    fn drop(&mut self) {
        unsafe { (self.player.api.ffi.render_context_free)(self.ctx) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves every symbol — including the render API just added — against the
    /// pinned libmpv build when present. Catches a mistyped render symbol name
    /// that would otherwise only surface at runtime.
    #[test]
    fn resolves_all_symbols_against_real_libmpv() {
        let dll = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/mpv/unpacked/libmpv-2.dll");
        if !dll.is_file() {
            eprintln!("skipped: libmpv not fetched");
            return;
        }
        let api = Api::load_from(&dll).expect("all symbols (incl. render API) must resolve");
        assert_eq!(api.version().0, 2, "expected client API v2.x");
    }
}
