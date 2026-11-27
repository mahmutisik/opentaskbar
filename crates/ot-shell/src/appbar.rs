//! The single choke point for `SHAppBarMessage` (`AppBarClient` in OpenTaskbar).
//!
//! `SHAppBarMessage` has no timeout and blocks while Explorer processes it, so each call first probes Explorer's
//! taskbar with `SendMessageTimeout(WM_NULL, 500 ms)`: a hung or restarting Explorer is skipped instead of
//! freezing the UI thread. While a call is on the stack, `notifications_suspended()` is true so ABN_* callbacks
//! that Windows dispatches during the wait are deferred instead of re-entering.

use std::sync::atomic::{AtomicU32, Ordering};

use ot_core::log;
use windows_sys::Win32::UI::Shell::{APPBARDATA, SHAppBarMessage};
use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_NULL};

use crate::Rect;
use crate::win::{self, Hwnd};

pub const ABM_NEW: u32 = 0x0;
pub const ABM_REMOVE: u32 = 0x1;
pub const ABM_QUERYPOS: u32 = 0x2;
pub const ABM_SETPOS: u32 = 0x3;
pub const ABM_GETSTATE: u32 = 0x4;
pub const ABM_GETTASKBARPOS: u32 = 0x5;
pub const ABM_SETSTATE: u32 = 0xA;

pub const ABE_LEFT: u32 = 0;
pub const ABE_RIGHT: u32 = 2;
pub const ABE_BOTTOM: u32 = 3;

pub const ABS_AUTOHIDE: u32 = 0x1;
pub const ABS_ALWAYSONTOP: u32 = 0x2;
/// The taskbar state bits OpenTaskbar saves and restores.
pub const SUPPORTED_STATE: u32 = ABS_AUTOHIDE | ABS_ALWAYSONTOP;

pub const ABN_STATECHANGE: u32 = 0;
pub const ABN_POSCHANGED: u32 = 1;
pub const ABN_FULLSCREENAPP: u32 = 2;

const EXPLORER_PROBE_TIMEOUT_MS: u32 = 500;

static NOTIFICATION_GATE: AtomicU32 = AtomicU32::new(0);

/// True while an AppBar call is in flight; shell callbacks must be deferred, not handled inline.
pub fn notifications_suspended() -> bool {
    NOTIFICATION_GATE.load(Ordering::Acquire) != 0
}

/// AppBar message data. `hwnd` is the window the message is about (the dock or Explorer's taskbar).
#[derive(Clone, Copy, Debug, Default)]
pub struct AppBarData {
    pub hwnd: Hwnd,
    pub callback_message: u32,
    pub edge: u32,
    pub rect: Rect,
    pub lparam: isize,
}

impl AppBarData {
    pub fn for_window(hwnd: Hwnd) -> Self {
        Self {
            hwnd,
            ..Self::default()
        }
    }
}

/// Sends an AppBar message to Explorer after probing that it responds. Returns 0 when skipped.
pub fn send(message: u32, data: &mut AppBarData, reason: &str) -> usize {
    let Some(taskbar) = win::find_native_taskbar() else {
        log::event(
            "AppBarSkipped",
            &[
                ("reason", &reason),
                ("message", &message),
                ("cause", &"no-taskbar"),
            ],
        );
        return 0;
    };
    let mut reply = 0usize;
    // SAFETY: WM_NULL with SMTO_ABORTIFHUNG and a bounded timeout; valid out pointer.
    let probe = unsafe {
        SendMessageTimeoutW(
            win::hwnd(taskbar.handle),
            WM_NULL,
            0,
            0,
            SMTO_ABORTIFHUNG,
            EXPLORER_PROBE_TIMEOUT_MS,
            &mut reply,
        )
    };
    if probe == 0 {
        log::event(
            "AppBarSkipped",
            &[
                ("reason", &reason),
                ("message", &message),
                ("cause", &"explorer-hung"),
                ("pid", &taskbar.process_id),
            ],
        );
        return 0;
    }

    let mut raw = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: win::hwnd(data.hwnd),
        uCallbackMessage: data.callback_message,
        uEdge: data.edge,
        rc: data.rect.into(),
        lParam: data.lparam,
    };
    let sequence = log::next_sequence();
    NOTIFICATION_GATE.fetch_add(1, Ordering::AcqRel);
    // SAFETY: `raw` is a fully initialized APPBARDATA with cbSize set.
    let result = unsafe { SHAppBarMessage(message, &mut raw) };
    NOTIFICATION_GATE.fetch_sub(1, Ordering::AcqRel);
    data.rect = raw.rc.into();
    data.edge = raw.uEdge;
    log::event(
        "AppBarMessage",
        &[
            ("seq", &sequence),
            ("reason", &reason),
            ("message", &message),
            ("result", &result),
            ("pid", &taskbar.process_id),
        ],
    );
    result
}

/// Explorer's taskbar state bits (ABS_AUTOHIDE, ABS_ALWAYSONTOP).
pub fn get_state(reason: &str) -> u32 {
    let mut data = AppBarData::for_window(0);
    send(ABM_GETSTATE, &mut data, reason) as u32
}

/// Asks Explorer to apply `state` to its taskbar window `taskbar`.
pub fn set_state(taskbar: Hwnd, state: u32, reason: &str) -> usize {
    let mut data = AppBarData {
        lparam: state as isize,
        ..AppBarData::for_window(taskbar)
    };
    send(ABM_SETSTATE, &mut data, reason)
}

/// Explorer's taskbar rectangle and edge.
pub fn taskbar_pos(window: Hwnd, reason: &str) -> Option<(Rect, u32)> {
    let mut data = AppBarData::for_window(window);
    (send(ABM_GETTASKBARPOS, &mut data, reason) != 0).then_some((data.rect, data.edge))
}

pub fn register(window: Hwnd, callback_message: u32, reason: &str) -> bool {
    let mut data = AppBarData {
        callback_message,
        ..AppBarData::for_window(window)
    };
    send(ABM_NEW, &mut data, reason) != 0
}

pub fn unregister(window: Hwnd, reason: &str) -> bool {
    let mut data = AppBarData::for_window(window);
    send(ABM_REMOVE, &mut data, reason) != 0
}

/// The official two-step reservation: QUERYPOS, let the caller pin the result to its edge, then SETPOS.
/// Returns whether SETPOS succeeded and the rectangle the shell actually assigned.
pub fn set_position(
    window: Hwnd,
    edge: u32,
    requested: Rect,
    adjust_after_query: Option<&dyn Fn(&mut Rect)>,
    reason: &str,
) -> (bool, Rect) {
    let mut data = AppBarData {
        edge,
        rect: requested,
        ..AppBarData::for_window(window)
    };
    if send(ABM_QUERYPOS, &mut data, &format!("{reason}/QUERYPOS")) == 0 {
        log::event(
            "AppBarQueryPosFailed",
            &[("reason", &reason), ("edge", &edge)],
        );
    }
    if let Some(adjust) = adjust_after_query {
        adjust(&mut data.rect);
    }
    let committed = send(ABM_SETPOS, &mut data, &format!("{reason}/SETPOS")) != 0;
    (committed, data.rect)
}
