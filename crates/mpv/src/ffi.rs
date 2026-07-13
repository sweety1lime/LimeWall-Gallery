//! Hand-written declarations for the documented libmpv C client API
//! (client API v2.x). Written from the public API documentation at
//! <https://mpv.io/manual/master/> and the libmpv doxygen; no mpv header code
//! was copied, so this crate carries no LGPL obligations of its own — the
//! LGPL library is loaded dynamically at runtime (docs/research/libmpv.md).

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_double, c_int, c_ulong, c_void};

use libloading::Library;

/// Opaque client handle; libmpv guarantees it is safe to use from any thread.
pub type mpv_handle = c_void;

// mpv_format values (subset we use).
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;

// mpv_event_id values (subset we use).
pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

// ---------------------------------------------------------------------------
// Render API (render.h): offscreen rendering for the phase-2 decoder sandbox.
// Software mode ("sw") makes libmpv render frames straight into a CPU buffer —
// no GL/EGL context — which is the cheapest way to prove the pipeline.
// ---------------------------------------------------------------------------

/// Opaque render context handle.
pub type mpv_render_context = c_void;

// mpv_render_param_type values (render.h). The SW group is 17..=20.
pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
pub const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
pub const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
pub const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

/// `mpv_render_context_update` flag: a new frame is ready to be rendered.
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

/// One entry of the null-terminated `mpv_render_param` array. `kind` is the
/// C `type` field (renamed — `type` is reserved in Rust).
#[repr(C)]
pub struct mpv_render_param {
    pub kind: c_int,
    pub data: *mut c_void,
}

macro_rules! api_table {
    ($( $field:ident : fn($($arg:ty),*) $(-> $ret:ty)? = $symbol:literal; )*) => {
        /// Function table resolved from `libmpv-2.dll`. Keeping the `Library`
        /// inside guarantees the pointers stay valid for the table's lifetime.
        pub struct Api {
            _lib: Library,
            $( pub $field: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        impl Api {
            /// Resolves every symbol eagerly so a broken dll fails fast.
            ///
            /// # Safety
            /// The file must be a genuine libmpv build; calling arbitrary
            /// code from a foreign dll is inherently unsafe.
            pub unsafe fn from_library(lib: Library) -> Result<Self, libloading::Error> {
                unsafe {
                    Ok(Self {
                        $( $field: *lib.get(concat!($symbol, "\0").as_bytes())?, )*
                        _lib: lib,
                    })
                }
            }
        }
    };
}

api_table! {
    client_api_version: fn() -> c_ulong = "mpv_client_api_version";
    error_string: fn(c_int) -> *const c_char = "mpv_error_string";
    free: fn(*mut c_void) = "mpv_free";
    create: fn() -> *mut mpv_handle = "mpv_create";
    initialize: fn(*mut mpv_handle) -> c_int = "mpv_initialize";
    terminate_destroy: fn(*mut mpv_handle) = "mpv_terminate_destroy";
    set_option_string: fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int
        = "mpv_set_option_string";
    set_property_string: fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int
        = "mpv_set_property_string";
    set_property: fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int
        = "mpv_set_property";
    get_property_string: fn(*mut mpv_handle, *const c_char) -> *mut c_char
        = "mpv_get_property_string";
    get_property: fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int
        = "mpv_get_property";
    command: fn(*mut mpv_handle, *mut *const c_char) -> c_int = "mpv_command";
    wait_event: fn(*mut mpv_handle, c_double) -> *mut mpv_event = "mpv_wait_event";
    render_context_create: fn(*mut *mut mpv_render_context, *mut mpv_handle, *mut mpv_render_param) -> c_int
        = "mpv_render_context_create";
    render_context_render: fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int
        = "mpv_render_context_render";
    render_context_update: fn(*mut mpv_render_context) -> u64 = "mpv_render_context_update";
    render_context_free: fn(*mut mpv_render_context) = "mpv_render_context_free";
}
