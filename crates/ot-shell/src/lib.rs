//! Explorer taskbar replacement: hiding and restoring the native taskbar, AppBar work-area reservation,
//! recovery journals and the out-of-process watchdog. Ported from OpenTaskbar's `TaskbarReplacementSession`,
//! `AppBarClient`, `RestoreCoordinator`, `NativeTaskbarWindow` and the native watchdog.
//!
//! Safety rule: nothing hides the taskbar before a recovery journal is on disk and the watchdog is armed.
#![cfg(windows)]

pub mod appbar;
pub mod desktops;
pub mod fullscreen;
pub mod journal;
pub mod restore;
pub mod session;
pub mod tracker;
pub mod tray;
pub mod watchdog;
pub mod win;

/// Window rectangle in physical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

impl From<windows_sys::Win32::Foundation::RECT> for Rect {
    fn from(r: windows_sys::Win32::Foundation::RECT) -> Self {
        Self::new(r.left, r.top, r.right, r.bottom)
    }
}

impl From<Rect> for windows_sys::Win32::Foundation::RECT {
    fn from(r: Rect) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}
