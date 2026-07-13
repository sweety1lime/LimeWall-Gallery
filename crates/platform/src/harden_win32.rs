//! Applies the Win32 process-mitigation policies that are compatible with the
//! renderer's stack — libmpv (a non-Microsoft DLL loaded at runtime), WebView2,
//! the `SetParent`-into-WorkerW reparenting and the child processes the daemon
//! and CLI launch. The aggressive policies are deliberately left out (see
//! docs/research/renderer-sandbox.md): Code Integrity Guard would block the
//! unsigned libmpv/ffmpeg, Arbitrary Code Guard breaks WebView2's JIT, and a
//! child-process or Win32k lockdown breaks launching the UI / ffmpeg / the GUI.
//!
//! Best-effort: an unsupported policy on an older Windows is logged and skipped,
//! never fatal.

use windows::Win32::System::Threading::{
    PROCESS_MITIGATION_POLICY, ProcessExtensionPointDisablePolicy, ProcessImageLoadPolicy,
    SetProcessMitigationPolicy,
};

// Bit positions inside each single-DWORD mitigation policy (winnt.h layout).
const DISABLE_EXTENSION_POINTS: u32 = 0x1;
const NO_REMOTE_IMAGES: u32 = 0x1;
const NO_LOW_LABEL_IMAGES: u32 = 0x2;
const PREFER_SYSTEM32_IMAGES: u32 = 0x4;

pub fn harden_process() {
    // Block legacy code injection via AppInit_DLLs, global window hooks and IMEs.
    set(
        ProcessExtensionPointDisablePolicy,
        DISABLE_EXTENSION_POINTS,
        "extension-point-disable",
    );

    // Harden DLL loading: refuse images from remote (UNC) paths and images
    // carrying a low-integrity label, and prefer System32 for system DLLs. Our
    // own libmpv/ffmpeg live in the install directory and load by full path, so
    // they are unaffected.
    set(
        ProcessImageLoadPolicy,
        NO_REMOTE_IMAGES | NO_LOW_LABEL_IMAGES | PREFER_SYSTEM32_IMAGES,
        "image-load",
    );
}

/// Applies one policy. Each of these two policies is a single DWORD of flag bits
/// (winnt.h), so a `u32` is a byte-identical, ABI-correct buffer — this avoids
/// the policy struct types, which the enabled windows-rs feature set does not
/// export.
fn set(policy: PROCESS_MITIGATION_POLICY, flags: u32, name: &str) {
    // SAFETY: the buffer is a single DWORD, matching these policies' layout and
    // the length passed; SetProcessMitigationPolicy only reads it.
    let result = unsafe {
        SetProcessMitigationPolicy(
            policy,
            std::ptr::from_ref(&flags).cast(),
            std::mem::size_of::<u32>(),
        )
    };
    if let Err(error) = result {
        eprintln!("process hardening: {name} skipped ({error})");
    }
}
