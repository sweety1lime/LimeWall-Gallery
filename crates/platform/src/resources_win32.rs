//! Windows backend for [`crate::resources::StackSampler`]: sums the CPU time of
//! this process and its descendants (WebView2 children) and turns successive
//! readings into a percentage of total machine capacity.

use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

pub(crate) struct Win32StackSampler {
    /// Busy CPU time (ns) and the wall-clock instant of the previous reading.
    last: Option<(u128, Instant)>,
    cores: u32,
}

impl Win32StackSampler {
    pub(crate) fn new() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        Self { last: None, cores }
    }

    pub(crate) fn sample(&mut self) -> Option<f32> {
        let busy = stack_busy_ns()?;
        let now = Instant::now();
        let percent = match self.last {
            Some((prev_busy, prev_at)) => {
                let busy_delta = busy.saturating_sub(prev_busy);
                let wall_delta = now.duration_since(prev_at).as_nanos();
                Some(crate::resources::cpu_percent(
                    busy_delta, wall_delta, self.cores,
                ))
            }
            None => None,
        };
        self.last = Some((busy, now));
        percent
    }
}

/// Total busy CPU time (ns) of this process and every descendant process.
fn stack_busy_ns() -> Option<u128> {
    let me = unsafe { GetCurrentProcessId() };
    let edges = process_edges()?;
    let descendants = crate::resources::descendants(me, &edges);

    // This process via the pseudo-handle — no OpenProcess / access rights.
    let mut total = process_busy_ns(unsafe { GetCurrentProcess() }).unwrap_or(0);
    for pid in descendants {
        let Some(handle) = open_query(pid) else {
            continue; // access denied / already gone — skip, don't fail
        };
        total = total.saturating_add(process_busy_ns(handle).unwrap_or(0));
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
    Some(total)
}

/// `(pid, parent_pid)` for every process, via a Toolhelp snapshot.
fn process_edges() -> Option<Vec<(u32, u32)>> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut edges = Vec::new();
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                edges.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        Some(edges)
    }
}

fn open_query(pid: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok() }
}

/// Kernel + user CPU time of one process, in nanoseconds.
fn process_busy_ns(handle: HANDLE) -> Option<u128> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
    }
    // FILETIME CPU times are in 100 ns units.
    let hundred_ns = filetime_units(&kernel).saturating_add(filetime_units(&user));
    Some(hundred_ns.saturating_mul(100))
}

fn filetime_units(ft: &FILETIME) -> u128 {
    ((ft.dwHighDateTime as u128) << 32) | ft.dwLowDateTime as u128
}
