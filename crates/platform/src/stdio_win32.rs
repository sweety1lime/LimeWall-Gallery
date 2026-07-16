//! Pointing the process standard handles at a log file.

use std::fs::File;
use std::os::windows::io::IntoRawHandle;
use std::path::Path;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

use crate::{HostError, Result};

fn io_err(context: &str, error: std::io::Error) -> HostError {
    HostError::Desktop(format!("{context}: {error}"))
}

pub fn redirect_to_file(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| io_err("failed to create the log directory", error))?;
    }
    let file =
        File::create(path).map_err(|error| io_err("failed to create the log file", error))?;
    // Deliberately leaked: the handle must outlive every writer for the whole
    // process lifetime, and the OS closes it on exit.
    let handle = HANDLE(file.into_raw_handle());
    for target in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe { SetStdHandle(target, handle) }
            .map_err(|error| HostError::Desktop(format!("failed to redirect output: {error}")))?;
    }
    Ok(())
}
