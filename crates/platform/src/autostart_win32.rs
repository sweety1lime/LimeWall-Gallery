//! Per-user autostart via the HKCU Run registry key.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::{HostError, Result};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

fn registry_err(context: &str, status: WIN32_ERROR) -> HostError {
    HostError::Desktop(format!("{context}: registry error {}", status.0))
}

pub fn enabled(app: &str) -> Result<bool> {
    let name = HSTRING::from(app);
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    };
    if status.is_ok() {
        Ok(true)
    } else if status == ERROR_FILE_NOT_FOUND {
        Ok(false)
    } else {
        Err(registry_err("failed to query the Run key", status))
    }
}

pub fn command(app: &str) -> Result<Option<String>> {
    let name = HSTRING::from(app);
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if !status.is_ok() {
        return Err(registry_err("failed to size the Run key value", status));
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(size_of::<u16>())];
    let mut size = (buffer.len() * size_of::<u16>()) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if !status.is_ok() {
        return Err(registry_err("failed to read the Run key value", status));
    }
    let text = &buffer[..(size as usize) / size_of::<u16>()];
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    Ok(Some(String::from_utf16_lossy(text)))
}

pub fn set(app: &str, command: Option<&str>) -> Result<()> {
    let name = HSTRING::from(app);
    match command {
        Some(command) => {
            let value = HSTRING::from(command);
            // Length in bytes, including the terminating NUL.
            let size = (value.len() + 1) * size_of::<u16>();
            let status = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    PCWSTR(name.as_ptr()),
                    REG_SZ.0,
                    Some(value.as_ptr().cast()),
                    size as u32,
                )
            };
            if status.is_ok() {
                Ok(())
            } else {
                Err(registry_err("failed to write the Run key", status))
            }
        }
        None => {
            let status =
                unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, PCWSTR(name.as_ptr())) };
            if status.is_ok() || status == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(registry_err("failed to delete the Run key value", status))
            }
        }
    }
}
