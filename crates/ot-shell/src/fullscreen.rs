//! Full-screen application detection (part of `TaskbarReplacementSession` in OpenTaskbar): the dock hides behind
//! a full-screen app on its monitor and comes back when it leaves.

use crate::Rect;
use crate::win::{self, Hmonitor, Hwnd};

const WS_CAPTION: isize = 0x00C0_0000;
const WS_EX_TRANSPARENT: isize = 0x0000_0020;
const WS_EX_APPWINDOW: isize = 0x0004_0000;
const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
const WS_EX_NOACTIVATE: isize = 0x0800_0000;

/// True when a window rectangle covers the monitor like a full-screen app, and not like a maximized window
/// (maximized windows keep WS_CAPTION or overhang the monitor with their resize border).
pub fn is_full_screen_bounds(
    window: Rect,
    style: isize,
    zoomed: bool,
    monitor: Rect,
    work: Rect,
) -> bool {
    let covers = window.left <= monitor.left + 2
        && window.top <= monitor.top + 2
        && window.right >= monitor.right - 2
        && window.bottom >= monitor.bottom - 2;
    if !covers {
        return false;
    }
    let work_near_monitor = (work.left - monitor.left).abs() <= 2
        && (work.top - monitor.top).abs() <= 2
        && (work.right - monitor.right).abs() <= 2
        && (work.bottom - monitor.bottom).abs() <= 2;
    let overhang = window.left < monitor.left - 2
        || window.top < monitor.top - 2
        || window.right > monitor.right + 2
        || window.bottom > monitor.bottom + 2;
    !(work_near_monitor && zoomed && (style & WS_CAPTION == WS_CAPTION || overhang))
}

/// Monitor-sized shell surfaces that are never full-screen apps.
pub fn is_excluded_class(class_name: &str, include_core_window: bool) -> bool {
    matches!(
        class_name,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "LogonUI" | "LockAppWindow"
    ) || (include_core_window && class_name == "Windows.UI.Core.CoreWindow")
}

/// Only a real application window that owns a taskbar button counts as a full-screen app.
fn has_taskbar_button(handle: Hwnd) -> bool {
    let ex = win::ex_style(handle);
    let app_window = ex & WS_EX_APPWINDOW != 0;
    if win::owner(handle) == 0 {
        (ex & WS_EX_NOACTIVATE == 0 || app_window) && ex & WS_EX_TOOLWINDOW == 0
    } else {
        app_window
    }
}

fn is_lock_screen(handle: Hwnd) -> bool {
    let pid = win::process_id(handle);
    pid != 0 && win::process_name(pid).is_some_and(|name| name == "LogonUI" || name == "LockApp")
}

fn is_full_screen(handle: Hwnd, dock_monitor: Hmonitor) -> bool {
    if handle == 0 || !win::is_visible(handle) || win::is_iconic(handle) {
        return false;
    }
    let root = win::root_owner(handle);
    let target = if root != 0 { root } else { handle };
    // Cloaked UWP/WinUI windows and click-through overlays can report full-monitor bounds (#95).
    if win::is_cloaked(target) || win::ex_style(target) & WS_EX_TRANSPARENT != 0 {
        return false;
    }
    let target_monitor = win::monitor_of(target);
    if target_monitor != dock_monitor {
        return false;
    }
    let (Some((monitor, work)), Some(rect)) =
        (win::monitor_info(target_monitor), win::window_rect(target))
    else {
        return false;
    };
    if !is_full_screen_bounds(
        rect,
        win::style(target),
        win::is_zoomed(target),
        monitor,
        work,
    ) {
        return false;
    }
    if is_excluded_class(&win::class_name(target), false) {
        return false;
    }
    // The lock screen covers the monitor but is not an app; treating it as one desynchronizes auto-hide.
    !is_lock_screen(target)
}

/// Any top-level application window (not ours) full screen on the dock's monitor. Checks every window, not just
/// the foreground one: a browser's Esc bubble can briefly own focus while its video stays full screen.
pub fn any_full_screen_on(dock_monitor: Hmonitor) -> bool {
    let own_pid = std::process::id();
    let mut found = false;
    win::enum_windows(|handle| {
        if win::process_id(handle) == own_pid
            || !is_full_screen(handle, dock_monitor)
            || !has_taskbar_button(handle)
        {
            return true;
        }
        if is_excluded_class(&win::class_name(handle), true) {
            return true;
        }
        found = true;
        false
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect::new(0, 0, 1920, 1080);
    const WORK: Rect = Rect::new(0, 0, 1920, 1032);
    const WORK_FULL: Rect = MONITOR;

    #[test]
    fn full_screen_video_counts() {
        assert!(is_full_screen_bounds(MONITOR, 0, false, MONITOR, WORK));
        assert!(is_full_screen_bounds(MONITOR, 0, true, MONITOR, WORK_FULL));
    }

    #[test]
    fn maximized_windows_do_not_count() {
        // Maximized with caption while the dock released the work area.
        assert!(!is_full_screen_bounds(
            MONITOR, WS_CAPTION, true, MONITOR, WORK_FULL
        ));
        // Custom-chrome maximized window overhanging by its resize border.
        assert!(!is_full_screen_bounds(
            Rect::new(-8, -8, 1928, 1088),
            0,
            true,
            MONITOR,
            WORK_FULL
        ));
        // Not covering the monitor.
        assert!(!is_full_screen_bounds(WORK, 0, false, MONITOR, WORK));
    }

    #[test]
    fn shell_surfaces_are_excluded() {
        assert!(is_excluded_class("WorkerW", false));
        assert!(!is_excluded_class("Windows.UI.Core.CoreWindow", false));
        assert!(is_excluded_class("Windows.UI.Core.CoreWindow", true));
        assert!(!is_excluded_class("Chrome_WidgetWin_1", true));
    }
}
