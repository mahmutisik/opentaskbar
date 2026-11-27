//! Out-of-process recovery: a second instance of the same executable, started with `--watchdog=<pid>:<token>`,
//! waits for the dock process and restores the taskbar if it ends without signalling a clean exit (End Task,
//! `taskkill /F`, a crash the process could not handle itself).
//!
//! OpenTaskbar ships this as a separate C executable because its main exe is a 170 MB .NET process; the Rust exe
//! is small, so the watchdog is simply a mode of the same binary and can never get out of date with it.
//!
//! Like OpenTaskbar's native watchdog (and unlike its managed fallback), there is no heartbeat timeout: a dock
//! that is busy for a while must never have the taskbar restored underneath it while it is still alive.

use std::ffi::c_void;
use std::hash::{BuildHasher, RandomState};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;

use ot_core::log;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};

const ARGUMENT_PREFIX: &str = "--watchdog=";
const CLEAN_EXIT_PREFIX: &str = r"Local\OpenTaskBar-Rust.CleanExit.";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Owned Win32 handle.
struct Handle(HANDLE);

// SAFETY: event and process handles may be used from any thread.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn clean_exit_event(parent_pid: u32, token: &str) -> Option<Handle> {
    let name: Vec<u16> = format!("{CLEAN_EXIT_PREFIX}{parent_pid}.{token}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: auto-reset, initially unsignalled named event; `name` is null-terminated.
    let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    (!handle.is_null()).then_some(Handle(handle))
}

static ARMED: Mutex<Option<Handle>> = Mutex::new(None);

pub fn is_watchdog_invocation(args: &[String]) -> bool {
    args.iter().any(|arg| arg.starts_with(ARGUMENT_PREFIX))
}

pub fn parse_arguments(args: &[String]) -> Option<(u32, String)> {
    let value = args
        .iter()
        .find_map(|arg| arg.strip_prefix(ARGUMENT_PREFIX))?;
    let (pid, token) = value.split_once(':')?;
    let pid: u32 = pid.parse().ok().filter(|pid| *pid > 0)?;
    (!token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| (pid, token.to_owned()))
}

/// Starts the watchdog for this process once. Call right after the taskbar journal is written and before
/// the taskbar is hidden, so a kill at any later point has an out-of-process restorer.
pub fn arm() {
    let Ok(mut armed) = ARMED.lock() else { return };
    if armed.is_some() {
        return;
    }
    let pid = std::process::id();
    let token = format!("{:016x}", RandomState::new().hash_one(pid));
    let Some(event) = clean_exit_event(pid, &token) else {
        log::warn("Taskbar watchdog could not start: the clean-exit event could not be created.");
        return;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            log::warn(format!("Taskbar watchdog could not start: {error}"));
            return;
        }
    };
    let argument = format!("{ARGUMENT_PREFIX}{pid}:{token}");
    // Leave the parent's job object when the job allows it: a launcher or terminal that kills its whole job
    // (kill-on-close) would otherwise take the watchdog down together with the dock, leaving the taskbar hidden.
    // Jobs that forbid breakaway make that spawn fail; then the watchdog starts inside the job as before.
    let spawned = Command::new(&exe)
        .arg(&argument)
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .map(|child| (child, true))
        .or_else(|_| {
            Command::new(&exe)
                .arg(&argument)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map(|child| (child, false))
        });
    match spawned {
        Ok((child, outside_job)) => {
            log::event(
                "WatchdogStarted",
                &[("pid", &child.id()), ("outsideJob", &outside_job)],
            );
            *armed = Some(event);
        }
        Err(error) => log::warn(format!("Taskbar watchdog could not start: {error}")),
    }
}

/// Tells the watchdog this exit is intentional. Call only after the taskbar and its journals are restored.
pub fn notify_clean_exit() {
    if let Ok(mut armed) = ARMED.lock()
        && let Some(event) = armed.take()
    {
        // SAFETY: valid event handle owned by `event`.
        unsafe { SetEvent(event.0) };
    }
}

/// Watchdog process entry point. Returns when the parent exited cleanly or after the recovery ran.
pub fn run(args: &[String]) {
    let Some((parent_pid, token)) = parse_arguments(args) else {
        return;
    };
    let Some(clean_exit) = clean_exit_event(parent_pid, &token) else {
        return;
    };
    // SAFETY: plain Win32 call; the handle is owned below.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if process.is_null() {
        let _ = crate::restore::restore("watchdog-parent-gone");
        crate::tray::broadcast_taskbar_created(true);
        return;
    }
    let process = Handle(process);
    let handles: [*mut c_void; 2] = [clean_exit.0, process.0];
    // SAFETY: two valid handles; wait for either.
    let signaled = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
    if signaled == WAIT_OBJECT_0 {
        return; // clean exit
    }
    // The parent ended; it may have signalled just before exiting.
    // SAFETY: valid handle, zero timeout.
    if unsafe { WaitForSingleObject(clean_exit.0, 0) } == WAIT_OBJECT_0 {
        return;
    }
    log::warn(format!(
        "Dock process {parent_pid} ended without a clean exit; restoring the taskbar."
    ));
    if let Err(error) = crate::restore::restore("watchdog-crash") {
        log::error(format!("Watchdog restore failed: {error}"));
    }
    // Tray icons were registered with the dead dock's tray window: ask the apps to register them with Explorer.
    crate::tray::broadcast_taskbar_created(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse_strictly() {
        let args = |s: &str| vec![s.to_owned()];
        assert_eq!(
            parse_arguments(&args("--watchdog=123:abc0")),
            Some((123, "abc0".to_owned()))
        );
        assert_eq!(parse_arguments(&args("--watchdog=0:abc")), None);
        assert_eq!(parse_arguments(&args("--watchdog=12:")), None);
        assert_eq!(parse_arguments(&args("--watchdog=12:a\\b")), None);
        assert_eq!(parse_arguments(&args("--watchdog=x:abc")), None);
        assert!(is_watchdog_invocation(&args("--watchdog=1:a")));
    }
}
