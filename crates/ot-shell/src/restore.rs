//! Putting Explorer's taskbar back (`RestoreCoordinator` and the static restore half of
//! `TaskbarReplacementSession` in OpenTaskbar). Usable from any thread and from another process (the watchdog,
//! `--restore`), because it depends only on the journal on disk.

use std::cell::Cell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ot_core::log;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW,
};

use crate::appbar::{self, SUPPORTED_STATE};
use crate::journal::{self, JournalError};
use crate::win;

const RESTORE_ATTEMPTS: usize = 3;

thread_local!(static IS_UI_THREAD: Cell<bool> = const { Cell::new(false) });

/// Marks the calling thread as the dock's UI thread: restore verification never sleeps on it.
pub fn mark_ui_thread() {
    IS_UI_THREAD.with(|flag| flag.set(true));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarStateResult {
    Applied,
    MissingTaskbar,
    Failed,
}

/// Publishes `state` to Explorer's validated taskbar only. Without `Shell_TrayWnd` there is nobody to own the
/// state, so callers enter a degraded mode instead of targeting their own window.
pub fn set_taskbar_state(state: u32) -> TaskbarStateResult {
    let taskbar = win::find_native_taskbar();
    // Explorer can notify us about our own update; an identical state must not publish another one.
    let current = appbar::get_state("SetState/current");
    // Comparing only the auto-hide bit would silently lose ABS_ALWAYSONTOP on restore.
    if current & SUPPORTED_STATE == state & SUPPORTED_STATE {
        return if taskbar.is_some() {
            TaskbarStateResult::Applied
        } else {
            TaskbarStateResult::MissingTaskbar
        };
    }
    let Some(taskbar) = taskbar else {
        log::event(
            "SetTaskbarStateSkipped",
            &[("cause", &"no-taskbar"), ("state", &state)],
        );
        return TaskbarStateResult::MissingTaskbar;
    };
    let reply = appbar::set_state(taskbar.handle, state, "SetTaskbarState");
    let applied = appbar::get_state("SetState/verify") & SUPPORTED_STATE == state & SUPPORTED_STATE;
    log::event(
        "SetTaskbarState",
        &[
            ("ok", &applied),
            ("state", &state),
            ("reply", &reply),
            ("pid", &taskbar.process_id),
        ],
    );
    if applied {
        TaskbarStateResult::Applied
    } else {
        TaskbarStateResult::Failed
    }
}

/// Hides the primary and every secondary taskbar; true only when all of them are hidden (#124).
pub fn hide_native_taskbar() -> bool {
    let Some(taskbar) = win::find_native_taskbar() else {
        return false;
    };
    if win::is_visible(taskbar.handle) {
        win::show(taskbar.handle, SW_HIDE);
    }
    let secondaries = win::find_secondary_taskbars();
    for secondary in &secondaries {
        if win::is_visible(secondary.handle) {
            win::show(secondary.handle, SW_HIDE);
        }
    }
    !win::is_visible(taskbar.handle) && secondaries.iter().all(|s| !win::is_visible(s.handle))
}

/// Whether Explorer shows any of its taskbars (primary or secondary) right now.
pub fn native_taskbar_shown() -> bool {
    win::find_native_taskbar().is_some_and(|t| win::is_visible(t.handle))
        || win::find_secondary_taskbars()
            .iter()
            .any(|s| win::is_visible(s.handle))
}

/// Re-hides secondary taskbars Explorer recreated, without touching the primary or any session state.
pub fn enforce_secondary_taskbars_hidden() {
    for secondary in win::find_secondary_taskbars() {
        if win::is_visible(secondary.handle) {
            win::show(secondary.handle, SW_HIDE);
        }
    }
}

fn show_native_taskbar() -> bool {
    let Some(taskbar) = win::find_native_taskbar() else {
        return false;
    };
    let flags =
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED;
    win::show(taskbar.handle, SW_SHOW);
    win::set_pos(taskbar.handle, 0, 0, 0, 0, 0, flags);
    for secondary in win::find_secondary_taskbars() {
        win::show(secondary.handle, SW_SHOW);
        win::set_pos(secondary.handle, 0, 0, 0, 0, 0, flags);
    }
    true
}

/// Primary and every secondary taskbar visible. A primary-only check could delete the journal too early.
pub fn is_native_taskbar_visible() -> bool {
    let Some(taskbar) = win::find_native_taskbar() else {
        return false;
    };
    win::is_visible(taskbar.handle)
        && win::find_secondary_taskbars()
            .iter()
            .all(|s| win::is_visible(s.handle))
}

fn try_restore_once(state: u32) -> bool {
    set_taskbar_state(state) == TaskbarStateResult::Applied
        && show_native_taskbar()
        && is_native_taskbar_visible()
}

/// Re-asserts state and visibility and requires two consecutive clean observations. Never sleeps on the UI
/// thread; background threads give Explorer 40 ms between passes to settle a late auto-hide or secondary tray.
pub fn restore_with_verification(state: u32, reason: &str) -> Result<(), &'static str> {
    let sequence = log::next_sequence();
    let background = !IS_UI_THREAD.with(Cell::get);
    let mut consecutive = 0;
    for attempt in 0..RESTORE_ATTEMPTS {
        consecutive = if try_restore_once(state) {
            consecutive + 1
        } else {
            0
        };
        if consecutive >= 2 {
            log::event(
                "TaskbarRestore",
                &[
                    ("reason", &reason),
                    ("ok", &true),
                    ("seq", &sequence),
                    ("attempts", &(attempt + 1)),
                ],
            );
            return Ok(());
        }
        if background && attempt + 1 < RESTORE_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    log::event(
        "TaskbarRestore",
        &[
            ("reason", &reason),
            ("ok", &false),
            ("seq", &sequence),
            ("attempts", &RESTORE_ATTEMPTS),
            ("state", &state),
        ],
    );
    Err("ErrorTaskbarRestoreFailed")
}

/// Restores the taskbar from the journal, even with no session in this process. The journal is deleted only
/// after the taskbar is verified visible, so a transient Explorer restart cannot lose the recovery state.
pub fn force_restore_native_taskbar() -> Result<(), &'static str> {
    let mut state = 0;
    if journal::has_pending_recovery() {
        let text = std::fs::read_to_string(journal::taskbar_journal_path()).ok();
        state = journal::fallback_state(text.as_deref());
        // A journal left behind by a failed clean exit must not overwrite a state the user has since changed.
        // After a hard kill the taskbar is hidden, so the recorded state still applies then.
        if is_native_taskbar_visible() {
            let current = appbar::get_state("ForceRestore/GetState") & SUPPORTED_STATE;
            if current != state & SUPPORTED_STATE {
                log::info(
                    "Ignoring a stale taskbar journal: taskbar is already visible with a different state.",
                );
                state = current;
            }
        }
    }
    restore_with_verification(state, "ForceRestore")?;
    restore_flyouts_from_journal();
    journal::delete_state().map_err(JournalError::key)
}

/// Puts Start/Search/Control Center back where Windows had them, from the journal. Handles are PID-checked
/// because Windows recycles HWNDs; the journal is kept if a handle now belongs to another process.
pub fn restore_flyouts_from_journal() {
    let mut owned_by_someone_else = false;
    for position in journal::read_flyouts() {
        if !win::is_window(position.handle) {
            continue;
        }
        if win::process_id(position.handle) != position.process_id {
            owned_by_someone_else = true;
            continue;
        }
        win::set_pos(
            position.handle,
            0,
            position.left,
            position.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    if !owned_by_someone_else {
        journal::delete_flyouts();
    }
}

static GATE: Mutex<()> = Mutex::new(());

/// The single entry point for every "put the taskbar back" path: clean exit, panic, startup heal, watchdog and
/// `--restore`. Attempts are serialized with a bounded 3 s wait and the result is logged with its duration.
pub fn restore(reason: &str) -> Result<(), &'static str> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let guard = loop {
        match GATE.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                log::event(
                    "TaskbarRestore",
                    &[("reason", &reason), ("ok", &false), ("cause", &"busy")],
                );
                return Err("RestoreBusy");
            }
        }
    };
    let started = Instant::now();
    let result = force_restore_native_taskbar();
    let ms = started.elapsed().as_millis();
    log::event(
        "TaskbarRestore",
        &[
            ("reason", &reason),
            ("ok", &result.is_ok()),
            ("ms", &ms),
            ("error", &result.err().unwrap_or("none")),
        ],
    );
    drop(guard);
    result
}

/// Heals a journal left behind by a crashed or killed instance; does nothing when there is none.
pub fn heal_pending(reason: &str) -> Result<(), &'static str> {
    if journal::has_pending_recovery() {
        restore(reason)
    } else {
        Ok(())
    }
}
