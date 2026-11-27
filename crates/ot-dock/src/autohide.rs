//! Auto-hide (`DockAutoHideController` in OpenTaskbar): the decision, the window-overlap test and the shell-window
//! checks. Timings and offsets are OpenTaskbar's (`DockMetrics`). The dock owns the timers and the slide animation.
//!
//! Difference from OpenTaskbar, invisible on screen: Smart mode re-checks window overlap on window events from the
//! tracker (foreground changes, moves and resizes of the foreground window, windows shown, hidden, cloaked) instead
//! of enumerating windows every 200 ms while the dock is revealed (#144). The 200 ms pointer check stays, and only
//! runs while a hide could actually follow.

use std::cell::Cell;
use std::ffi::c_void;

use ot_core::settings::{AutoHideMode, DockEdge};
use ot_shell::Rect;
use ot_shell::win::{self, Hwnd};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};

/// Pointer away this long before the dock hides (`DockMetrics.AutoHideDelayMs`).
pub const HIDE_DELAY_MS: u32 = 400;
pub const REVEAL_MS: u32 = 220;
pub const HIDE_MS: u32 = 220;
/// Minimum slide distance (`DockMetrics.HideMinimumOffset`).
pub const HIDE_MINIMUM_OFFSET: f64 = 72.0;
/// Edge strip that reveals the hidden dock (`DockMetrics.EdgeTriggerHeight`).
pub const EDGE_TRIGGER: f64 = 5.0;
/// Pointer re-check while revealed (`_revealedGuardTimer`).
pub const GUARD_MS: u32 = 200;

#[derive(Clone, Copy, Debug, Default)]
pub struct AutoHide {
    pub mode: AutoHideMode,
    pub hidden: bool,
    pub over_dock: bool,
    pub at_edge: bool,
    pub overlap: bool,
    pub shell_menu_open: bool,
    /// The last application window in front, judged while the Start menu or the dock itself holds the foreground.
    pub last_application: Hwnd,
}

impl AutoHide {
    pub fn should_hide(&self, interaction_locked: bool) -> bool {
        match self.mode {
            AutoHideMode::Disabled => false,
            _ if self.over_dock || self.at_edge || self.shell_menu_open || interaction_locked => {
                false
            }
            AutoHideMode::Always => true,
            AutoHideMode::Smart => self.overlap,
        }
    }

    /// A hide could follow once the pointer leaves: only then is the pointer worth polling.
    pub fn hide_pending_on_pointer(&self) -> bool {
        !self.hidden
            && match self.mode {
                AutoHideMode::Disabled => false,
                AutoHideMode::Always => true,
                AutoHideMode::Smart => self.overlap,
            }
    }
}

/// Edge strip thickness in pixels (`Math.Max(4, ceil(EdgeTriggerHeight * scale) + 2)`).
pub fn edge_pixels(scale: f64) -> i32 {
    ((EDGE_TRIGGER * scale).ceil() as i32 + 2).max(4)
}

/// Slide distance in DIPs for a bar `thickness` DIPs thick, signed for the edge (x for side docks, y for bottom).
pub fn hide_offset(edge: DockEdge, thickness: f64) -> f64 {
    let offset = HIDE_MINIMUM_OFFSET.max(thickness + 16.0);
    if edge == DockEdge::Left {
        -offset
    } else {
        offset
    }
}

/// Pointer over the dock: its bounds grown by the headroom on the side away from the edge and 6 px elsewhere.
pub fn pointer_over_dock(cursor: (i32, i32), dock: Rect, edge: DockEdge, headroom_px: i32) -> bool {
    let left = dock.left
        - if edge == DockEdge::Right {
            headroom_px
        } else {
            6
        };
    let right = dock.right
        + if edge == DockEdge::Left {
            headroom_px
        } else {
            6
        };
    let top = dock.top
        - if edge == DockEdge::Bottom {
            headroom_px
        } else {
            6
        };
    let bottom = dock.bottom + 6;
    cursor.0 >= left && cursor.0 <= right && cursor.1 >= top && cursor.1 <= bottom
}

/// Pointer in the edge strip of the dock's monitor.
pub fn pointer_at_edge(cursor: (i32, i32), monitor: Rect, edge: DockEdge, edge_px: i32) -> bool {
    let inside = cursor.0 >= monitor.left
        && cursor.0 <= monitor.right
        && cursor.1 >= monitor.top
        && cursor.1 <= monitor.bottom;
    inside
        && match edge {
            DockEdge::Left => cursor.0 <= monitor.left + edge_px,
            DockEdge::Right => cursor.0 >= monitor.right - edge_px,
            DockEdge::Bottom => cursor.1 >= monitor.bottom - edge_px,
        }
}

pub fn cursor() -> Option<(i32, i32)> {
    let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    (unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) } != 0)
        .then_some((point.x, point.y))
}

thread_local! {
    /// (process id, is the Start menu) of the last process asked about.
    static START_MENU_PROCESS: Cell<(u32, bool)> = const { Cell::new((0, false)) };
}

/// A window of the Start menu host (`StartMenuState.IsStartMenuWindow`).
pub fn is_start_menu_window(handle: Hwnd) -> bool {
    if handle == 0 {
        return false;
    }
    let pid = win::process_id(handle);
    if pid == 0 {
        return false;
    }
    let (cached_pid, cached) = START_MENU_PROCESS.with(Cell::get);
    if cached_pid == pid {
        return cached;
    }
    let is_start = win::process_name(pid)
        .is_some_and(|name| name.eq_ignore_ascii_case("StartMenuExperienceHost"));
    START_MENU_PROCESS.with(|c| c.set((pid, is_start)));
    is_start
}

/// The full-screen Alt+Tab / Task View switcher (`ShellOverlayWindows.IsTaskSwitcher`).
pub fn is_task_switcher(handle: Hwnd) -> bool {
    handle != 0
        && matches!(
            win::class_name(handle).as_str(),
            "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame" | "TaskSwitcherWnd"
        )
}

pub fn is_shell_overlay(handle: Hwnd) -> bool {
    is_start_menu_window(handle) || is_task_switcher(handle)
}

/// The Start menu is open: it holds the foreground and is on screen (`IsStartMenuOpenNow`, without the
/// window enumeration fallback; the tracker reports every foreground change).
pub fn start_menu_open() -> bool {
    let foreground = win::foreground();
    is_start_menu_window(foreground) && win::is_visible(foreground) && !win::is_cloaked(foreground)
}

fn frame_bounds(handle: Hwnd) -> Option<Rect> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a RECT-sized attribute into a RECT.
    let hr = unsafe {
        DwmGetWindowAttribute(
            handle as _,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut rect as *mut RECT).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    };
    if hr == 0 {
        Some(Rect::new(rect.left, rect.top, rect.right, rect.bottom))
    } else {
        win::window_rect(handle)
    }
}

/// An application window covers the dock's resting bounds (`EvaluateWindowOverlap`).
pub fn window_covers_dock(handle: Hwnd, dock_window: Hwnd, dock: Rect) -> bool {
    if handle == 0 {
        return false;
    }
    let root = win::root_owner(handle);
    let target = if root != 0 { root } else { handle };
    if target == dock_window
        || !win::is_window(target)
        || !win::is_visible(target)
        || win::is_iconic(target)
        || win::is_cloaked(target)
        || win::process_id(target) == std::process::id()
    {
        return false;
    }
    if matches!(
        win::class_name(target).as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return false;
    }
    if win::monitor_of(dock_window) != win::monitor_of(target) {
        return false;
    }
    if win::is_zoomed(target) {
        return true;
    }
    frame_bounds(target).is_some_and(|w| {
        w.left < dock.right && w.right > dock.left && w.bottom > dock.top && w.top < dock.bottom
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_matches_opentaskbar() {
        let mut a = AutoHide {
            mode: AutoHideMode::Smart,
            ..Default::default()
        };
        assert!(!a.should_hide(false));
        a.overlap = true;
        assert!(a.should_hide(false));
        a.over_dock = true;
        assert!(!a.should_hide(false));
        a.over_dock = false;
        a.shell_menu_open = true;
        assert!(!a.should_hide(false));
        a.shell_menu_open = false;
        assert!(!a.should_hide(true));
        a.mode = AutoHideMode::Always;
        a.overlap = false;
        assert!(a.should_hide(false));
        a.at_edge = true;
        assert!(!a.should_hide(false));
        a.mode = AutoHideMode::Disabled;
        a.at_edge = false;
        assert!(!a.should_hide(false));
    }

    #[test]
    fn pointer_zones() {
        let dock = Rect::new(800, 1030, 1120, 1077);
        assert!(pointer_over_dock((900, 1010), dock, DockEdge::Bottom, 24));
        assert!(!pointer_over_dock((900, 1000), dock, DockEdge::Bottom, 24));
        assert!(pointer_over_dock((1125, 1050), dock, DockEdge::Bottom, 24));
        let monitor = Rect::new(0, 0, 1920, 1080);
        assert_eq!(edge_pixels(1.0), 7);
        assert!(pointer_at_edge((10, 1075), monitor, DockEdge::Bottom, 7));
        assert!(!pointer_at_edge((10, 1060), monitor, DockEdge::Bottom, 7));
        assert!(pointer_at_edge((3, 500), monitor, DockEdge::Left, 7));
        assert_eq!(hide_offset(DockEdge::Bottom, 44.0), 72.0);
        assert_eq!(hide_offset(DockEdge::Left, 70.0), -86.0);
    }
}
