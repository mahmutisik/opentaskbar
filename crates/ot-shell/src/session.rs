//! The taskbar replacement session (`TaskbarReplacementSession` in OpenTaskbar): hides Explorer's taskbar,
//! registers the dock as an AppBar that reserves its strip of the work area, keeps both in sync with Explorer
//! (restarts, display and DPI changes, full-screen apps) and restores everything on stop.
//!
//! Threading: a session lives on the dock's UI thread and is driven by its window procedure. Explorer can send
//! ABN_* notifications while an `SHAppBarMessage` call is still on the stack; those re-entrant messages must not
//! touch the session (it is already borrowed), so the window procedure hands them to `defer_reentrant`, which
//! queues a reconcile on a timer, exactly where OpenTaskbar defers them too.
//!
//! Differences from the C# session, all fixes:
//! - AppBar registration is tracked explicitly, so `stop` and `stop_for_restart` really remove the reservation
//!   (C# checked the phase after switching to Stopping, so the cleanup never ran; upstream issue #141).

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ot_core::log;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    KillTimer, RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SetTimer, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_TIMER,
};

use crate::Rect;
use crate::appbar::{
    self, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABN_FULLSCREENAPP, ABN_POSCHANGED, ABN_STATECHANGE,
};
use crate::appbar::{ABS_ALWAYSONTOP, ABS_AUTOHIDE};
use crate::fullscreen;
use crate::journal;
use crate::restore::{self, TaskbarStateResult};
use crate::watchdog;
use crate::win::{self, Hmonitor, Hwnd};

/// AppBar callback message (WM_APP + 0x52, as OpenTaskbar).
pub const CALLBACK_MESSAGE: u32 = 0x8000 + 0x52;
/// Posted by the dock when Explorer shows (wparam 0) or recreates (wparam 1) one of its taskbars.
pub const TASKBAR_SHOWN_MESSAGE: u32 = 0x8000 + 0x53;
/// Explorer missing: retries back off and stop after this many; TaskbarCreated or a new taskbar window wakes the
/// session again.
const DEGRADED_RETRY_LIMIT: u32 = 45;
/// Minimum headroom above the shelf for magnified icons, in DIPs (`DockMetrics.Headroom`).
pub const MIN_HEADROOM_DIPS: f64 = 24.0;
/// Default shelf height in DIPs (`DockMetrics.ShelfHeight`).
pub const DEFAULT_SHELF_DIPS: f64 = 66.0;

const TIMER_FULL_SCREEN: usize = 0x4F01;
const TIMER_HIDE_RETRY: usize = 0x4F02;
const TIMER_RECONCILE: usize = 0x4F03;
const TIMER_EDGE_SYNC: usize = 0x4F04;
const HWND_TOPMOST: Hwnd = -1;
const HWND_BOTTOM: Hwnd = 1;

/// The `TaskbarCreated` message Explorer broadcasts after it (re)starts.
pub fn taskbar_created_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        let name: Vec<u16> = "TaskbarCreated".encode_utf16().chain(Some(0)).collect();
        // SAFETY: null-terminated name.
        unsafe { RegisterWindowMessageW(name.as_ptr()) }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Left,
    Right,
}

impl Edge {
    fn appbar_edge(self) -> u32 {
        match self {
            Self::Bottom => ABE_BOTTOM,
            Self::Left => ABE_LEFT,
            Self::Right => ABE_RIGHT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Uninitialized,
    Hiding,
    Registered,
    Active,
    Degraded,
    Stopping,
    Restoring,
    StoppedForRestart,
    Failed,
}

/// What the session reports to the dock UI. Handlers must not call back into the session.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// A localization key and whether it is an error.
    OperationReported(&'static str, bool),
    /// The dock content area in DIPs (bar width/length and shelf thickness).
    LayoutChanged {
        width: f64,
        height: f64,
    },
    FullScreenChanged(bool),
}

pub struct Session {
    dock: Hwnd,
    phase: Phase,
    full_screen_suppressed: bool,
    app_bar_registered: bool,
    in_transient: bool,
    previous_taskbar_state: u32,
    native_bottom_dips: f64,
    native_at_bottom: bool,
    shelf_dips: f64,
    headroom_dips: f64,
    edge: Edge,
    auto_hide: bool,
    target_monitor: Hmonitor,
    reservation: Option<(i32, i32, i32, i32)>,
    side_reservation: Option<(i32, i32, u32, i32)>,
    last_bottom_reserve_synced: bool,
    last_window_pos: Option<(i32, i32, i32, i32, bool)>,
    last_layout: Option<(f64, f64)>,
    /// Skip work-area updates while the user drags a size slider.
    pub defer_reservation: bool,
    reconcile_active: bool,
    hide_retry_pending: bool,
    hide_retry_attempts: u32,
    /// Consecutive checks that found every taskbar already hidden.
    hide_retry_clean: u32,
    /// Settle mode (start, Explorer restart): Explorer's late initialization re-shows the taskbar several times.
    hide_retry_settle: bool,
    degraded_attempts: u32,
    full_screen_scan: Option<(Instant, bool)>,
    events: Box<dyn FnMut(SessionEvent)>,
}

thread_local! {
    /// Reconcile reason queued by a re-entrant message or by `queue_reconcile`; consumed by the timer.
    static DEFERRED_REASON: RefCell<Option<String>> = const { RefCell::new(None) };
    static RECONCILE_TIMER_ARMED: Cell<bool> = const { Cell::new(false) };
}

fn set_timer(dock: Hwnd, id: usize, ms: u32) {
    // SAFETY: the timer belongs to our own window; WM_TIMER is routed to `Session::handle_message`.
    unsafe { SetTimer(win::hwnd(dock), id, ms, None) };
}

fn kill_timer(dock: Hwnd, id: usize) {
    // SAFETY: killing a timer that may not exist is harmless.
    unsafe { KillTimer(win::hwnd(dock), id) };
}

/// Records a reconcile for later when the session is busy (re-entrant ABN_* or display messages).
/// Call from the window procedure when the session cannot be borrowed. Returns true for messages it handled.
pub fn defer_reentrant(dock: Hwnd, message: u32, wparam: usize) -> bool {
    let reason = if message == CALLBACK_MESSAGE {
        match wparam as u32 {
            ABN_POSCHANGED => "ABN_POSCHANGED/deferred",
            ABN_FULLSCREENAPP => "ABN_FULLSCREENAPP/deferred",
            ABN_STATECHANGE => "ABN_STATECHANGE/deferred",
            _ => return false,
        }
    } else if message == WM_DISPLAYCHANGE {
        "WM_DISPLAYCHANGE/deferred"
    } else if message == WM_DPICHANGED {
        "WM_DPICHANGED/deferred"
    } else if message == taskbar_created_message() {
        "TaskbarCreated/deferred"
    } else if message == TASKBAR_SHOWN_MESSAGE {
        "TaskbarShown/deferred"
    } else {
        return false;
    };
    queue_reason(dock, reason, 120);
    message == CALLBACK_MESSAGE
}

fn queue_reason(dock: Hwnd, reason: &str, delay_ms: u32) {
    DEFERRED_REASON.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(reason.to_owned());
        }
    });
    if !RECONCILE_TIMER_ARMED.with(Cell::get) {
        RECONCILE_TIMER_ARMED.with(|armed| armed.set(true));
        set_timer(dock, TIMER_RECONCILE, delay_ms);
    }
}

fn take_reason() -> Option<String> {
    DEFERRED_REASON.with(|slot| slot.borrow_mut().take())
}

/// Delay before hide check number `attempt + 1`.
fn hide_retry_delay(attempt: u32, settle: bool) -> u32 {
    match (settle, attempt) {
        (false, 0..=3) => 250,
        (false, 4..=9) => 500,
        (false, _) => 1000,
        (true, 0..=9) => 500,
        (true, 10..=24) => 1000,
        (true, _) => 1500,
    }
}

/// Bottom reservation from the measured native taskbar, independent of a lagging `rcWork`
/// (`ComputeBottomReservation`): (native pixels, extra pixels, expected work-area bottom).
#[allow(clippy::too_many_arguments)] // mirrors the C# signature for easy comparison
pub fn compute_bottom_reservation(
    monitor_bottom: i32,
    shelf_pixels: i32,
    native_bottom_dips: f64,
    dpi: u32,
    auto_hide: bool,
    release_native_strip: bool,
    native_at_bottom: bool,
    previous_was_auto_hide: bool,
) -> (i32, i32, i32) {
    let native_owns_strip =
        !auto_hide && !release_native_strip && native_at_bottom && !previous_was_auto_hide;
    let native = if native_owns_strip {
        ((native_bottom_dips * f64::from(dpi) / 96.0).ceil() as i32).min(shelf_pixels)
    } else {
        0
    };
    let extra = if auto_hide {
        0
    } else {
        (shelf_pixels - native).max(0)
    };
    (native, extra, monitor_bottom - (native + extra))
}

impl Session {
    /// A session for the dock window `dock`. Nothing changes until `start`.
    pub fn new(dock: Hwnd, auto_hide: bool, events: impl FnMut(SessionEvent) + 'static) -> Self {
        Self {
            dock,
            phase: Phase::Uninitialized,
            full_screen_suppressed: false,
            app_bar_registered: false,
            in_transient: false,
            previous_taskbar_state: 0,
            native_bottom_dips: 0.0,
            native_at_bottom: true,
            shelf_dips: DEFAULT_SHELF_DIPS,
            headroom_dips: MIN_HEADROOM_DIPS,
            edge: Edge::Bottom,
            auto_hide,
            target_monitor: 0,
            reservation: None,
            side_reservation: None,
            last_bottom_reserve_synced: false,
            last_window_pos: None,
            last_layout: None,
            defer_reservation: false,
            reconcile_active: false,
            hide_retry_pending: false,
            hide_retry_attempts: 0,
            hide_retry_clean: 0,
            hide_retry_settle: false,
            degraded_attempts: 0,
            full_screen_scan: None,
            events: Box::new(events),
        }
    }

    fn is_running(&self) -> bool {
        matches!(
            self.phase,
            Phase::Hiding | Phase::Registered | Phase::Active | Phase::Degraded
        )
    }

    /// True while the session owns (or is releasing) Explorer's taskbar state.
    pub fn is_active(&self) -> bool {
        self.is_running() || matches!(self.phase, Phase::Stopping | Phase::Restoring)
    }

    pub fn is_degraded(&self) -> bool {
        self.phase == Phase::Degraded
    }

    pub fn full_screen_suppressed(&self) -> bool {
        self.full_screen_suppressed
    }

    fn can_operate(&self) -> bool {
        self.is_running()
    }

    fn is_vertical(&self) -> bool {
        self.edge != Edge::Bottom
    }

    fn release_native_strip(&self) -> bool {
        !self.is_vertical() && self.native_at_bottom && self.shelf_dips < self.native_bottom_dips
    }

    /// Explorer keeps reserving the work area for its hidden taskbar; auto-hide releases that strip for the dock's
    /// hide modes, side docks and shelves lower than the native taskbar.
    fn replacement_state(&self) -> u32 {
        (self.previous_taskbar_state & ABS_ALWAYSONTOP)
            | if self.auto_hide
                || self.is_vertical()
                || !self.native_at_bottom
                || self.release_native_strip()
            {
                ABS_AUTOHIDE
            } else {
                0
            }
    }

    fn emit(&mut self, event: SessionEvent) {
        (self.events)(event);
    }

    fn mark_registered(&mut self) {
        self.app_bar_registered = true;
        if self.is_running() && self.phase != Phase::Degraded {
            self.phase = Phase::Registered;
        }
    }

    fn mark_unregistered(&mut self) {
        self.app_bar_registered = false;
        if self.is_running() && self.phase != Phase::Degraded {
            self.phase = Phase::Hiding;
        }
    }

    // ---- Lifecycle ----

    /// Saves Explorer's taskbar state, arms the watchdog, hides the taskbar and reserves the dock's strip.
    pub fn start(&mut self) -> Result<(), &'static str> {
        if self.is_active() {
            return Ok(());
        }
        self.degraded_attempts = 0;
        restore::mark_ui_thread();
        let pending = if journal::has_pending_recovery() {
            Some(journal::read_state().map_err(journal::JournalError::key)?)
        } else {
            None
        };
        self.previous_taskbar_state = pending.unwrap_or_else(|| appbar::get_state("GetState"));
        self.read_native_taskbar_height();
        if pending.is_none() {
            journal::write_state(self.previous_taskbar_state)
                .map_err(journal::JournalError::key)?;
        }
        // Arm recovery as soon as the journal exists: a kill at any later point has an out-of-process restorer.
        watchdog::arm();

        let result = self.start_hidden();
        if result.is_err() {
            let _ = self.stop();
            if self.is_active() {
                self.phase = Phase::Failed;
            }
        }
        result
    }

    fn start_hidden(&mut self) -> Result<(), &'static str> {
        self.phase = Phase::Hiding;
        // Hide the taskbar window before switching it to auto-hide: Explorer then slides an invisible window,
        // so its auto-hide animation (the taskbar dropping out and back) is never on screen.
        restore::hide_native_taskbar();
        match restore::set_taskbar_state(self.replacement_state()) {
            TaskbarStateResult::MissingTaskbar => {
                self.enter_degraded("Start: Shell_TrayWnd missing", 500)
            }
            TaskbarStateResult::Failed => return Err("ErrorTaskbarEdgeTrigger"),
            TaskbarStateResult::Applied => {}
        }
        // Another shell (or a recovery in progress) can be showing the taskbar at this moment. Unlike the C#
        // session, which gives up with ErrorTaskbarHide, keep going and let the hide-retry loop win.
        if !restore::hide_native_taskbar() && !self.is_degraded() {
            log::warn("Start: the taskbar did not hide on the first try; retrying.");
        }
        if appbar::register(self.dock, CALLBACK_MESSAGE, "Start") {
            self.mark_registered();
        } else if !self.is_degraded() {
            return Err("ErrorAppBarRegister");
        }
        self.reservation = None;
        self.last_window_pos = None;
        if !self.is_degraded() {
            self.phase = Phase::Active;
        }
        log::event(
            "SessionStart",
            &[("ok", &true), ("degraded", &self.is_degraded())],
        );
        // Explorer can show the taskbar again for a few seconds after it hides (late initialization); watch until
        // it stays hidden.
        self.settle_native_taskbar("Start");
        // Browsers enter full screen by resizing the foreground window without a foreground change; poll.
        set_timer(self.dock, TIMER_FULL_SCREEN, 500);
        self.position_dock_window(true)
    }

    fn stop_timers(&mut self) {
        for id in [
            TIMER_FULL_SCREEN,
            TIMER_HIDE_RETRY,
            TIMER_RECONCILE,
            TIMER_EDGE_SYNC,
        ] {
            kill_timer(self.dock, id);
        }
        RECONCILE_TIMER_ARMED.with(|armed| armed.set(false));
        take_reason();
        self.hide_retry_pending = false;
    }

    /// Removes the reservation, restores Explorer's taskbar (verified) and deletes the journals. The watchdog is
    /// dismissed only after everything is back, so a failure here still leaves an out-of-process restorer.
    pub fn stop(&mut self) -> Result<(), &'static str> {
        if !self.is_active() && !journal::has_pending_recovery() {
            return Ok(());
        }
        self.phase = Phase::Stopping;
        self.stop_timers();
        if self.full_screen_suppressed {
            self.set_full_screen_suppression(false);
        }
        if self.app_bar_registered {
            self.zero_and_remove_app_bar(None);
        }
        self.phase = Phase::Restoring;
        let mut result = restore::restore_with_verification(self.previous_taskbar_state, "Stop");
        if result.is_ok() {
            restore::restore_flyouts_from_journal();
            result = journal::delete_state().map_err(journal::JournalError::key);
        }
        if result.is_ok() {
            watchdog::notify_clean_exit();
        }
        self.phase = if result.is_ok() {
            Phase::Uninitialized
        } else {
            Phase::Failed
        };
        result
    }

    /// Ends the session but keeps the taskbar hidden and the journal on disk, for an immediate re-registration
    /// (e.g. a dock size change) without flashing Explorer's taskbar.
    pub fn stop_for_restart(&mut self) {
        self.phase = Phase::Stopping;
        self.stop_timers();
        if self.full_screen_suppressed {
            self.set_full_screen_suppression(false);
        }
        if self.app_bar_registered {
            self.zero_and_remove_app_bar(None);
        }
        self.phase = Phase::StoppedForRestart;
        log::event("SessionStoppedForRestart", &[]);
    }

    // ---- Reconcile ----

    /// Explorer (re)started and broadcast `TaskbarCreated`.
    pub fn handle_taskbar_created(&mut self) {
        if !self.is_running() {
            return;
        }
        self.degraded_attempts = 0;
        log::event("TaskbarCreated", &[("seq", &log::next_sequence())]);
        self.reconcile("TaskbarCreated");
    }

    /// The only path that repairs taskbar state, registration, hiding and placement together. Bounded: at most
    /// one queued reason is consumed inline; anything later is retried on a timer instead of recursing.
    fn reconcile(&mut self, reason: &str) {
        if !self.can_operate() {
            log::event("ReconcileSkipped", &[("reason", &reason)]);
            return;
        }
        if self.reconcile_active {
            queue_reason(self.dock, reason, 120);
            return;
        }
        self.reconcile_active = true;
        let mut reason = reason.to_owned();
        for _ in 0..2 {
            let deferred = take_reason();
            self.reconcile_once(&reason);
            match deferred {
                Some(next) => reason = next,
                None => break,
            }
        }
        self.reconcile_active = false;
        if DEFERRED_REASON.with(|slot| slot.borrow().is_some()) {
            queue_reason(self.dock, &reason, 300);
        }
    }

    fn reconcile_once(&mut self, reason: &str) {
        let sequence = log::next_sequence();
        log::event(
            "Reconcile",
            &[
                ("seq", &sequence),
                ("reason", &reason),
                ("phase", &format!("{:?}", self.phase)),
            ],
        );
        if reason.starts_with("TaskbarCreated") {
            // A stale reservation from the previous registration can survive an Explorer restart.
            self.zero_and_remove_app_bar(None);
            self.reservation = None;
            self.side_reservation = None;
            self.last_window_pos = None;
        }
        let Some(lease) = win::find_native_taskbar() else {
            self.enter_degraded(reason, 500);
            log::event(
                "Reconcile",
                &[("seq", &sequence), ("ok", &false), ("cause", &"no-taskbar")],
            );
            return;
        };
        self.read_native_taskbar_height();
        match restore::set_taskbar_state(self.replacement_state()) {
            TaskbarStateResult::MissingTaskbar => {
                self.enter_degraded(reason, 500);
                log::event(
                    "Reconcile",
                    &[("seq", &sequence), ("ok", &false), ("cause", &"no-taskbar")],
                );
                return;
            }
            TaskbarStateResult::Failed => {
                self.emit(SessionEvent::OperationReported(
                    "ErrorTaskbarAutoHideToggle",
                    true,
                ));
                log::event(
                    "Reconcile",
                    &[
                        ("seq", &sequence),
                        ("ok", &false),
                        ("cause", &"setstate-failed"),
                    ],
                );
                return;
            }
            TaskbarStateResult::Applied => {}
        }
        if !self.app_bar_registered && appbar::register(self.dock, CALLBACK_MESSAGE, reason) {
            self.mark_registered();
        }
        if reason.starts_with("TaskbarCreated") || reason.starts_with("TaskbarRecreated") {
            self.settle_native_taskbar(reason);
        } else if !restore::hide_native_taskbar() {
            self.retry_hide_native_taskbar(reason);
        }
        self.degraded_attempts = 0;
        if self.phase == Phase::Degraded {
            self.phase = if self.app_bar_registered {
                Phase::Active
            } else {
                Phase::Hiding
            };
        }
        self.reposition();
        log::event(
            "Reconcile",
            &[
                ("seq", &sequence),
                ("ok", &true),
                ("verified", &(self.app_bar_registered && lease.is_valid())),
            ],
        );
    }

    fn enter_degraded(&mut self, reason: &str, retry_ms: u32) {
        if !self.is_running() {
            return;
        }
        if self.phase != Phase::Degraded {
            self.phase = Phase::Degraded;
            log::event("TaskbarDegraded", &[("reason", &reason)]);
        }
        // Back off while Explorer stays away (500 ms, then 1 s, then 2 s; about a minute in all), then wait for
        // TaskbarCreated or a new taskbar window instead of waking twice a second forever.
        self.degraded_attempts += 1;
        if self.degraded_attempts > DEGRADED_RETRY_LIMIT {
            if self.degraded_attempts == DEGRADED_RETRY_LIMIT + 1 {
                log::event("TaskbarDegradedIdle", &[("reason", &reason)]);
            }
            return;
        }
        let delay = match self.degraded_attempts {
            0..=10 => retry_ms,
            11..=25 => retry_ms.max(1000),
            _ => retry_ms.max(2000),
        };
        queue_reason(self.dock, reason, delay);
    }

    /// Window messages for the session. Returns true when the message is fully handled.
    pub fn handle_message(&mut self, message: u32, wparam: usize, lparam: isize) -> bool {
        if message == WM_TIMER {
            return self.on_timer(wparam);
        }
        if message == TASKBAR_SHOWN_MESSAGE {
            if self.can_operate() {
                if wparam == 1 {
                    // A new primary taskbar: Explorer restarted (its TaskbarCreated broadcast follows; this also
                    // covers a missed one). Reconcile everything.
                    self.degraded_attempts = 0;
                    queue_reason(self.dock, "TaskbarRecreated", 250);
                } else if !self.full_screen_suppressed {
                    self.retry_hide_native_taskbar("TaskbarShown");
                }
            }
            return true;
        }
        if message == taskbar_created_message() {
            // The tray host's own broadcast (collecting icons) is not an Explorer restart.
            if !crate::tray::is_own_broadcast(wparam, lparam) {
                self.handle_taskbar_created();
            }
            return false;
        }
        if !self.can_operate() {
            return false;
        }
        if message == CALLBACK_MESSAGE {
            let code = wparam as u32;
            let reason = match code {
                ABN_POSCHANGED => "ABN_POSCHANGED",
                ABN_FULLSCREENAPP => "ABN_FULLSCREENAPP",
                ABN_STATECHANGE => "ABN_STATECHANGE",
                _ => return false,
            };
            if appbar::notifications_suspended() || self.in_transient {
                queue_reason(self.dock, &format!("{reason}/deferred"), 120);
                return true;
            }
            match code {
                ABN_POSCHANGED => {
                    self.last_window_pos = None;
                    self.reconcile(reason);
                }
                ABN_FULLSCREENAPP => {
                    // Re-scan instead of trusting one app's lParam: another full-screen window may remain.
                    self.full_screen_scan = None;
                    let full_screen = self.any_full_screen();
                    self.set_full_screen_suppression(full_screen);
                    self.reposition();
                }
                _ => self.reconcile(reason),
            }
            return true;
        }
        if message == WM_DISPLAYCHANGE || message == WM_DPICHANGED {
            self.last_window_pos = None;
            self.reservation = None;
            self.side_reservation = None;
            self.reconcile(if message == WM_DISPLAYCHANGE {
                "WM_DISPLAYCHANGE"
            } else {
                "WM_DPICHANGED"
            });
        }
        false
    }

    fn on_timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_FULL_SCREEN => {
                if self.can_operate() {
                    let full_screen = self.any_full_screen();
                    self.set_full_screen_suppression(full_screen);
                }
            }
            TIMER_HIDE_RETRY => self.hide_retry_tick(),
            TIMER_RECONCILE => {
                kill_timer(self.dock, TIMER_RECONCILE);
                RECONCILE_TIMER_ARMED.with(|armed| armed.set(false));
                if let Some(reason) = take_reason() {
                    self.reconcile(&reason);
                }
            }
            TIMER_EDGE_SYNC => {
                kill_timer(self.dock, TIMER_EDGE_SYNC);
                if self.can_operate() {
                    self.reservation = None;
                    self.side_reservation = None;
                    self.enforce_native_taskbar_hidden();
                    self.reposition();
                }
            }
            _ => return false,
        }
        true
    }

    // ---- Settings that move the dock ----

    pub fn edge(&self) -> Edge {
        self.edge
    }

    pub fn set_edge(&mut self, edge: Edge) {
        if self.edge == edge {
            return;
        }
        if !self.can_operate() {
            self.edge = edge;
            return;
        }
        let was_registered = self.app_bar_registered;
        if was_registered {
            // Drop the old edge's reservation while `edge` still describes it.
            self.zero_and_remove_app_bar(Some(self.edge));
        }
        self.edge = edge;
        self.reservation = None;
        self.side_reservation = None;
        self.last_window_pos = None;
        self.last_layout = None;
        if was_registered && appbar::register(self.dock, CALLBACK_MESSAGE, "Edge") {
            self.mark_registered();
        }
        if restore::set_taskbar_state(self.replacement_state())
            == TaskbarStateResult::MissingTaskbar
        {
            self.enter_degraded("Edge: Shell_TrayWnd missing", 500);
        }
        self.enforce_native_taskbar_hidden();
        self.reposition();
        // Explorer releases its bottom strip asynchronously after ABS_AUTOHIDE; re-apply once it settled.
        set_timer(self.dock, TIMER_EDGE_SYNC, 120);
    }

    /// The monitor the dock sits on; 0 follows the dock window's nearest monitor.
    pub fn set_target_monitor(&mut self, monitor: Hmonitor) {
        if self.target_monitor == monitor {
            return;
        }
        self.target_monitor = monitor;
        self.reservation = None;
        self.side_reservation = None;
        self.last_window_pos = None;
        self.last_layout = None;
        if !self.can_operate() {
            return;
        }
        if self.app_bar_registered {
            // Release the old monitor's strip before re-registering.
            self.zero_and_remove_app_bar(None);
            if appbar::register(self.dock, CALLBACK_MESSAGE, "TargetMonitor") {
                self.mark_registered();
            }
        }
        self.reposition();
    }

    pub fn set_auto_hide(&mut self, auto_hide: bool) {
        self.auto_hide = auto_hide;
        if !self.can_operate() {
            return;
        }
        match restore::set_taskbar_state(self.replacement_state()) {
            TaskbarStateResult::MissingTaskbar => {
                self.enter_degraded("UpdateAutoHide: Shell_TrayWnd missing", 500);
                return;
            }
            TaskbarStateResult::Failed => {
                self.emit(SessionEvent::OperationReported(
                    "ErrorTaskbarAutoHideToggle",
                    true,
                ));
                return;
            }
            TaskbarStateResult::Applied => {}
        }
        self.reservation = None;
        self.side_reservation = None;
        self.reposition();
    }

    /// Shelf thickness in DIPs for the selected dock size; the window adds headroom above it.
    pub fn set_shelf_height(&mut self, dips: f64) {
        if self.shelf_dips == dips {
            return;
        }
        let was_releasing = self.release_native_strip();
        self.shelf_dips = dips;
        if self.can_operate() {
            if was_releasing != self.release_native_strip() {
                restore::set_taskbar_state(self.replacement_state());
                restore::hide_native_taskbar();
                self.reservation = None;
            }
            self.reposition();
        }
    }

    /// Headroom above the shelf in DIPs (driven by the user's top gap).
    pub fn set_headroom(&mut self, dips: f64) {
        if (self.headroom_dips - dips).abs() < 0.01 {
            return;
        }
        self.headroom_dips = dips;
        if self.can_operate() {
            self.reposition();
        }
    }

    pub fn refresh_position(&mut self) {
        self.reposition();
    }

    /// Commits a pending work-area update after a geometry change; re-registers only if Explorer kept the old one.
    pub fn ensure_reservation_synced(&mut self) {
        if !self.can_operate() {
            return;
        }
        self.reposition();
        if !self.is_vertical() && !self.last_bottom_reserve_synced {
            self.reapply_reservation();
        }
    }

    /// Re-registers the AppBar in place: Windows 11 recalculates the work area reliably only for a fresh AppBar.
    pub fn reapply_reservation(&mut self) {
        if !self.can_operate() {
            return;
        }
        if self.app_bar_registered {
            self.zero_and_remove_app_bar(None);
        }
        if appbar::register(self.dock, CALLBACK_MESSAGE, "ReapplyReservation") {
            self.mark_registered();
        } else {
            log::warn("ReapplyReservation: ABM_NEW failed; keeping the existing window placement.");
        }
        self.reservation = None;
        self.side_reservation = None;
        self.last_window_pos = None;
        self.reposition();
    }

    /// Foreground changed: re-scan for a full-screen app on the dock's monitor.
    pub fn update_foreground(&mut self) {
        if !self.can_operate() {
            return;
        }
        let full_screen = self.any_full_screen();
        self.set_full_screen_suppression(full_screen);
    }

    // ---- Placement ----

    fn dock_monitor(&self) -> Hmonitor {
        if self.target_monitor != 0 {
            self.target_monitor
        } else {
            win::monitor_of(self.dock)
        }
    }

    /// Repositions the dock; failures are reported as events, not returned.
    fn reposition(&mut self) {
        let _ = self.position_dock_window(false);
    }

    fn position_dock_window(&mut self, fail_on_error: bool) -> Result<(), &'static str> {
        if !self.can_operate() || self.in_transient {
            return Ok(());
        }
        self.in_transient = true;
        let result = self.position_dock();
        self.in_transient = false;
        match result {
            Err(key) if !fail_on_error => {
                log::warn(format!("Dock positioning skipped: {key}"));
                self.emit(SessionEvent::OperationReported(key, true));
                Ok(())
            }
            other => other,
        }
    }

    fn position_dock(&mut self) -> Result<(), &'static str> {
        let monitor = self.dock_monitor();
        let (bounds, work) = win::monitor_info(monitor).ok_or("ErrorDisplayDimensions")?;
        // Mixed-DPI setups: the target monitor's DPI, not the window's previous one (#95).
        let mut dpi = win::monitor_dpi(monitor);
        if dpi == 0 {
            dpi = win::window_dpi(self.dock);
        }
        if dpi == 0 {
            return Err("ErrorDisplayDimensions");
        }
        if self.is_vertical() {
            return self.position_side_dock(bounds, work, dpi);
        }
        let scale = f64::from(dpi) / 96.0;
        let shelf = (self.shelf_dips * scale).ceil() as i32;
        let headroom = (self.headroom_dips.max(MIN_HEADROOM_DIPS) * scale).ceil() as i32;
        self.reserve_shelf(monitor, bounds, work, shelf, dpi);
        let total = shelf + headroom;
        let layout = (f64::from(bounds.width()) / scale, f64::from(shelf) / scale);
        if self.last_layout != Some(layout) {
            self.last_layout = Some(layout);
            self.emit(SessionEvent::LayoutChanged {
                width: layout.0,
                height: layout.1,
            });
        }
        self.update_foreground();
        let desired = (
            bounds.left,
            bounds.bottom - total,
            bounds.width(),
            total,
            self.full_screen_suppressed,
        );
        self.place(desired)
    }

    fn place(&mut self, desired: (i32, i32, i32, i32, bool)) -> Result<(), &'static str> {
        if self.last_window_pos == Some(desired) {
            return Ok(());
        }
        self.last_window_pos = Some(desired);
        let (x, y, cx, cy, suppressed) = desired;
        let after = if suppressed {
            HWND_BOTTOM
        } else {
            HWND_TOPMOST
        };
        if win::set_pos(
            self.dock,
            after,
            x,
            y,
            cx,
            cy,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        ) {
            Ok(())
        } else {
            self.last_window_pos = None;
            Err("ErrorDockPosition")
        }
    }

    fn read_native_taskbar_height(&mut self) {
        if let Some(taskbar) = win::find_native_taskbar()
            && let Some((rect, edge)) = appbar::taskbar_pos(self.dock, "GetTaskbarPos")
        {
            let dpi = win::window_dpi(taskbar.handle);
            if dpi != 0 && rect.height() > 0 && rect.width() > 0 {
                self.native_at_bottom = edge == ABE_BOTTOM;
                self.native_bottom_dips = if self.native_at_bottom {
                    f64::from(rect.height()) * 96.0 / f64::from(dpi)
                } else {
                    0.0
                };
                log::info(format!(
                    "Native taskbar measured: {:.1} DIP, bottom={}.",
                    self.native_bottom_dips, self.native_at_bottom
                ));
                return;
            }
        }
        // A hidden taskbar (recovering from a killed session, Explorer restarting) cannot be measured.
        self.native_at_bottom = true;
        self.native_bottom_dips = 48.0;
    }

    fn reserve_shelf(
        &mut self,
        monitor_handle: Hmonitor,
        monitor: Rect,
        work: Rect,
        shelf: i32,
        dpi: u32,
    ) {
        if !self.app_bar_registered || self.defer_reservation {
            return;
        }
        let (native, extra, expected_bottom) = compute_bottom_reservation(
            monitor.bottom,
            shelf,
            self.native_bottom_dips,
            dpi,
            self.auto_hide,
            self.release_native_strip(),
            self.native_at_bottom,
            self.previous_taskbar_state & ABS_AUTOHIDE != 0,
        );
        let request = (monitor.left, monitor.bottom, native, extra);
        if self.reservation == Some(request) {
            return;
        }
        self.reservation = Some(request);
        let bottom = monitor.bottom - native;
        let requested = Rect::new(monitor.left, bottom - extra, monitor.right, bottom);
        // Pin the bottom edge so a transient screen-bottom QUERYPOS reply cannot drop stacking.
        let pin = |rect: &mut Rect| {
            let resolved_bottom = rect.bottom.min(bottom);
            *rect = Rect::new(
                monitor.left,
                resolved_bottom - extra,
                monitor.right,
                resolved_bottom,
            );
        };
        let (committed, resolved) =
            appbar::set_position(self.dock, ABE_BOTTOM, requested, Some(&pin), "ReserveShelf");
        if !committed {
            log::warn(format!(
                "Could not reserve the {extra}px taskbar shelf; the work area may keep its old height."
            ));
            self.reservation = None;
            return;
        }
        let work_after =
            win::monitor_info(monitor_handle).map_or(work.bottom, |(_, work)| work.bottom);
        self.last_bottom_reserve_synced = work_after == expected_bottom;
        log::info(format!(
            "ReserveShelf: shelf={shelf}px native={native}px extra={extra}px setPos=({}..{}) workBottom={work_after} (expected={expected_bottom}).",
            resolved.top, resolved.bottom
        ));
    }

    /// Left/right dock: a full-height column whose shelf is reserved unless auto-hide is on.
    fn position_side_dock(
        &mut self,
        monitor: Rect,
        work: Rect,
        dpi: u32,
    ) -> Result<(), &'static str> {
        let scale = f64::from(dpi) / 96.0;
        let thickness = ((self.shelf_dips * scale).round() as i32).max(1);
        let headroom = (self.headroom_dips.max(MIN_HEADROOM_DIPS) * scale).round() as i32;
        let total = thickness + headroom;
        let edge = self.edge.appbar_edge();
        let reserved = if self.auto_hide { 0 } else { thickness };
        let actual = if edge == ABE_LEFT {
            (work.left - monitor.left).max(0)
        } else {
            (monitor.right - work.right).max(0)
        };
        let request = (
            if edge == ABE_LEFT {
                monitor.left
            } else {
                monitor.right
            },
            monitor.top,
            edge,
            reserved,
        );
        // Re-apply when the request changed or Windows has not moved the work area yet.
        if self.app_bar_registered
            && !self.defer_reservation
            && (self.side_reservation != Some(request) || actual != reserved)
        {
            self.side_reservation = Some(request);
            let requested = if edge == ABE_LEFT {
                Rect::new(
                    monitor.left,
                    monitor.top,
                    monitor.left + reserved,
                    monitor.bottom,
                )
            } else {
                Rect::new(
                    monitor.right - reserved,
                    monitor.top,
                    monitor.right,
                    monitor.bottom,
                )
            };
            let pin = move |rect: &mut Rect| {
                if edge == ABE_LEFT {
                    rect.right = rect.left + reserved;
                } else {
                    rect.left = rect.right - reserved;
                }
            };
            let (committed, _) =
                appbar::set_position(self.dock, edge, requested, Some(&pin), "PositionSideDock");
            if !committed && self.side_reservation == Some(request) {
                self.side_reservation = None;
            }
        }
        let layout = (
            f64::from(thickness) / scale,
            f64::from(monitor.height()) / scale,
        );
        if self.last_layout != Some(layout) {
            self.last_layout = Some(layout);
            self.emit(SessionEvent::LayoutChanged {
                width: layout.0,
                height: layout.1,
            });
        }
        self.update_foreground();
        let left = if edge == ABE_LEFT {
            monitor.left
        } else {
            monitor.right - total
        };
        self.place((
            left,
            monitor.top,
            total,
            monitor.height(),
            self.full_screen_suppressed,
        ))
    }

    /// Collapses the reservation to zero thickness before ABM_REMOVE, so Explorer restores the work area and
    /// resizes maximized windows immediately.
    fn zero_and_remove_app_bar(&mut self, edge_override: Option<Edge>) {
        if !self.app_bar_registered {
            return;
        }
        let edge = edge_override.unwrap_or(self.edge).appbar_edge();
        let was_transient = self.in_transient;
        self.in_transient = true;
        let zero = self.zero_thickness_rect(edge);
        let (collapsed, _) =
            appbar::set_position(self.dock, edge, zero, None, "ZeroAndRemove/SetPos");
        let removed = appbar::unregister(self.dock, "ZeroAndRemove");
        log::event(
            "AppBarRemove",
            &[("collapsed", &collapsed), ("removed", &removed)],
        );
        self.in_transient = was_transient;
        self.mark_unregistered();
        self.reservation = None;
        self.side_reservation = None;
    }

    /// Side AppBars must span the monitor's height; a degenerate rect is rejected and leaves the strip.
    fn zero_thickness_rect(&self, edge: u32) -> Rect {
        let Some((b, _)) = win::monitor_info(self.dock_monitor()) else {
            return Rect::default();
        };
        match edge {
            ABE_LEFT => Rect::new(b.left, b.top, b.left, b.bottom),
            ABE_RIGHT => Rect::new(b.right, b.top, b.right, b.bottom),
            _ => Rect::new(b.left, b.bottom, b.right, b.bottom),
        }
    }

    // ---- Native taskbar visibility ----

    fn enforce_native_taskbar_hidden(&mut self) {
        if !self.can_operate() {
            return;
        }
        match restore::set_taskbar_state(self.replacement_state()) {
            TaskbarStateResult::MissingTaskbar => {
                self.enter_degraded("EnforceHidden: Shell_TrayWnd missing", 500);
                return;
            }
            TaskbarStateResult::Failed => {
                self.emit(SessionEvent::OperationReported(
                    "TaskbarAutoHideReenabled",
                    false,
                ));
                return;
            }
            TaskbarStateResult::Applied => {}
        }
        if !restore::hide_native_taskbar() {
            self.retry_hide_native_taskbar("EnforceHidden");
        }
    }

    /// Explorer briefly re-shows an auto-hidden taskbar at the screen edge and may recreate a secondary tray a
    /// beat later: hide again until two checks in a row find everything hidden (250 ms, then 500 ms, then 1 s
    /// apart; at most 20 checks).
    fn retry_hide_native_taskbar(&mut self, reason: &str) {
        self.begin_hide_retry(reason, false);
    }

    /// After a start or an Explorer restart Explorer keeps re-showing the taskbar for a few seconds: hide until ten
    /// checks in a row find everything hidden (500 ms, then 1 s, then 1.5 s apart; at most 40 checks).
    fn settle_native_taskbar(&mut self, reason: &str) {
        self.begin_hide_retry(reason, true);
    }

    fn begin_hide_retry(&mut self, reason: &str, settle: bool) {
        if self.hide_retry_pending {
            if settle && !self.hide_retry_settle {
                // A running quick retry becomes a settle without starting over.
                self.hide_retry_settle = true;
                self.hide_retry_clean = 0;
            }
            return;
        }
        self.hide_retry_pending = true;
        self.hide_retry_settle = settle;
        self.hide_retry_attempts = 0;
        self.hide_retry_clean = 0;
        log::event(
            "NativeTaskbarHideRetry",
            &[("reason", &reason), ("settle", &settle)],
        );
        set_timer(self.dock, TIMER_HIDE_RETRY, hide_retry_delay(0, settle));
    }

    fn hide_retry_tick(&mut self) {
        self.hide_retry_attempts += 1;
        if !self.can_operate() {
            kill_timer(self.dock, TIMER_HIDE_RETRY);
            self.hide_retry_pending = false;
            return;
        }
        let shown = restore::native_taskbar_shown();
        let hidden = restore::hide_native_taskbar();
        if shown || !hidden {
            self.hide_retry_clean = 0;
        } else {
            self.hide_retry_clean += 1;
        }
        let (needed, limit) = if self.hide_retry_settle {
            (10, 40)
        } else {
            (2, 20)
        };
        if self.hide_retry_clean >= needed || self.hide_retry_attempts >= limit {
            kill_timer(self.dock, TIMER_HIDE_RETRY);
            self.hide_retry_pending = false;
            log::event(
                "NativeTaskbarHide",
                &[
                    ("ok", &hidden),
                    ("attempts", &self.hide_retry_attempts),
                    ("clean", &self.hide_retry_clean),
                    ("settle", &self.hide_retry_settle),
                ],
            );
            if !hidden {
                self.emit(SessionEvent::OperationReported("ErrorTaskbarRehide", true));
            }
            return;
        }
        set_timer(
            self.dock,
            TIMER_HIDE_RETRY,
            hide_retry_delay(self.hide_retry_attempts, self.hide_retry_settle),
        );
    }

    // ---- Full screen ----

    /// Full-screen scan, coalesced: placement and the 500 ms poll both ask, at most one scan per 200 ms.
    fn any_full_screen(&mut self) -> bool {
        if let Some((at, result)) = self.full_screen_scan
            && at.elapsed() < Duration::from_millis(200)
        {
            return result;
        }
        let result = fullscreen::any_full_screen_on(self.dock_monitor());
        self.full_screen_scan = Some((Instant::now(), result));
        result
    }

    fn set_full_screen_suppression(&mut self, suppressed: bool) {
        if self.full_screen_suppressed == suppressed {
            return;
        }
        log::info(format!(
            "FullScreenSuppression changed: suppressed={suppressed}."
        ));
        self.full_screen_suppressed = suppressed;
        // Leaving full screen must re-apply HWND_TOPMOST after an HWND_BOTTOM demotion.
        self.last_window_pos = None;
        win::show(
            self.dock,
            if suppressed {
                SW_HIDE
            } else {
                SW_SHOWNOACTIVATE
            },
        );
        if !suppressed {
            self.reposition();
        }
        self.emit(SessionEvent::FullScreenChanged(suppressed));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.phase == Phase::StoppedForRestart {
            return; // the next session takes over the hidden taskbar and its journal
        }
        if let Err(error) = self.stop() {
            log::error(format!("Session drop: restore failed: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_reservation_matches_csharp() {
        // Native 48 DIP taskbar at 100%, shelf 66 px: Explorer keeps 48, the dock adds 18.
        assert_eq!(
            compute_bottom_reservation(1080, 66, 48.0, 96, false, false, true, false),
            (48, 18, 1014)
        );
        // Auto-hide: nothing reserved.
        assert_eq!(
            compute_bottom_reservation(1080, 66, 48.0, 96, true, false, true, false),
            (0, 0, 1080)
        );
        // Native strip released (shelf lower than the taskbar): the dock reserves its whole shelf.
        assert_eq!(
            compute_bottom_reservation(1080, 40, 48.0, 96, false, true, true, false),
            (0, 40, 1040)
        );
        // 150%: 48 DIP = 72 px, capped at the shelf.
        assert_eq!(
            compute_bottom_reservation(1080, 99, 48.0, 144, false, false, true, false),
            (72, 27, 981)
        );
        // Taskbar was auto-hide before the dock: it owns no strip.
        assert_eq!(
            compute_bottom_reservation(1080, 66, 48.0, 96, false, false, true, true),
            (0, 66, 1014)
        );
    }
}
