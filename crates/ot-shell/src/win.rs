//! Small safe wrappers over the Win32 window APIs the shell code uses, plus `HwndLease` and the lookup of
//! Explorer's taskbar windows (`HwndLease` / `NativeTaskbarWindow` in OpenTaskbar).
//!
//! Window handles are stored as `isize` so they can cross threads; Windows recycles them, so every operation on
//! a foreign window re-checks it through a lease.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::Rect;

pub type Hwnd = isize;

pub(crate) fn hwnd(handle: Hwnd) -> HWND {
    handle as HWND
}

pub fn is_window(handle: Hwnd) -> bool {
    // SAFETY: IsWindow accepts any value.
    handle != 0 && unsafe { wm::IsWindow(hwnd(handle)) } != 0
}

pub fn is_visible(handle: Hwnd) -> bool {
    // SAFETY: IsWindowVisible accepts any value.
    handle != 0 && unsafe { wm::IsWindowVisible(hwnd(handle)) } != 0
}

pub fn process_id(handle: Hwnd) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out pointer; any handle value is accepted.
    let thread = unsafe { wm::GetWindowThreadProcessId(hwnd(handle), &mut pid) };
    if thread == 0 { 0 } else { pid }
}

pub fn class_name(handle: Hwnd) -> String {
    let mut buffer = [0u16; 128];
    // SAFETY: the buffer length is passed.
    let length =
        unsafe { wm::GetClassNameW(hwnd(handle), buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

pub fn show(handle: Hwnd, command: wm::SHOW_WINDOW_CMD) -> bool {
    // SAFETY: plain Win32 call.
    unsafe { wm::ShowWindow(hwnd(handle), command) != 0 }
}

pub fn set_pos(
    handle: Hwnd,
    after: Hwnd,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    flags: wm::SET_WINDOW_POS_FLAGS,
) -> bool {
    // SAFETY: plain Win32 call.
    unsafe { wm::SetWindowPos(hwnd(handle), hwnd(after), x, y, cx, cy, flags) != 0 }
}

pub fn window_rect(handle: Hwnd) -> Option<Rect> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: valid out pointer.
    (unsafe { wm::GetWindowRect(hwnd(handle), &mut rect) } != 0).then(|| rect.into())
}

pub fn style(handle: Hwnd) -> isize {
    // SAFETY: plain Win32 call.
    unsafe { wm::GetWindowLongPtrW(hwnd(handle), wm::GWL_STYLE) }
}

pub fn ex_style(handle: Hwnd) -> isize {
    // SAFETY: plain Win32 call.
    unsafe { wm::GetWindowLongPtrW(hwnd(handle), wm::GWL_EXSTYLE) }
}

pub fn is_iconic(handle: Hwnd) -> bool {
    // SAFETY: plain Win32 call.
    unsafe { wm::IsIconic(hwnd(handle)) != 0 }
}

pub fn is_zoomed(handle: Hwnd) -> bool {
    // SAFETY: plain Win32 call.
    unsafe { wm::IsZoomed(hwnd(handle)) != 0 }
}

pub fn root_owner(handle: Hwnd) -> Hwnd {
    // SAFETY: plain Win32 call.
    unsafe { wm::GetAncestor(hwnd(handle), wm::GA_ROOTOWNER) as Hwnd }
}

pub fn owner(handle: Hwnd) -> Hwnd {
    // SAFETY: plain Win32 call.
    unsafe { wm::GetWindow(hwnd(handle), wm::GW_OWNER) as Hwnd }
}

pub fn foreground() -> Hwnd {
    // SAFETY: plain Win32 call.
    unsafe { wm::GetForegroundWindow() as Hwnd }
}

pub fn is_cloaked(handle: Hwnd) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the attribute is a 4-byte value and the out buffer is 4 bytes.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd(handle),
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    hr == 0 && cloaked != 0
}

/// Monitor handle stored as `isize`.
pub type Hmonitor = isize;

pub fn monitor_of(handle: Hwnd) -> Hmonitor {
    // SAFETY: plain Win32 call.
    unsafe { MonitorFromWindow(hwnd(handle), MONITOR_DEFAULTTONEAREST) as Hmonitor }
}

/// Monitor bounds and work area.
pub fn monitor_info(monitor: Hmonitor) -> Option<(Rect, Rect)> {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: cbSize is set; the struct is a valid out buffer.
    (unsafe { GetMonitorInfoW(monitor as _, &mut info) } != 0)
        .then(|| (info.rcMonitor.into(), info.rcWork.into()))
}

pub fn monitor_dpi(monitor: Hmonitor) -> u32 {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: valid out pointers.
    let hr = unsafe { GetDpiForMonitor(monitor as _, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    if hr == 0 { x } else { 0 }
}

pub fn window_dpi(handle: Hwnd) -> u32 {
    // SAFETY: plain Win32 call.
    unsafe { GetDpiForWindow(hwnd(handle)) }
}

/// Calls `f` for every top-level window until it returns `false`.
pub fn enum_windows(mut f: impl FnMut(Hwnd) -> bool) {
    unsafe extern "system" fn callback(handle: HWND, state: LPARAM) -> i32 {
        // SAFETY: `state` is the `&mut dyn FnMut` passed below and outlives EnumWindows.
        let f = unsafe { &mut *(state as *mut &mut dyn FnMut(Hwnd) -> bool) };
        i32::from(f(handle as Hwnd))
    }
    let mut dynamic: &mut dyn FnMut(Hwnd) -> bool = &mut f;
    // SAFETY: the callback only dereferences `dynamic` during this call.
    unsafe {
        wm::EnumWindows(
            Some(callback),
            (&mut dynamic as *mut &mut dyn FnMut(Hwnd) -> bool) as LPARAM,
        )
    };
}

/// A window handle paired with the process that owned it when captured. Valid only while both still match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwndLease {
    pub handle: Hwnd,
    pub process_id: u32,
}

impl HwndLease {
    pub fn capture(handle: Hwnd) -> Option<Self> {
        let process_id = process_id(handle);
        (handle != 0 && process_id != 0).then_some(Self { handle, process_id })
    }

    pub fn is_valid(&self) -> bool {
        is_window(self.handle) && process_id(self.handle) == self.process_id
    }

    pub fn is_foreign(&self) -> bool {
        self.is_valid() && self.process_id != std::process::id()
    }
}

const TRAY_CLASS: &str = "Shell_TrayWnd";
const SECONDARY_TRAY_CLASS: &str = "Shell_SecondaryTrayWnd";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Explorer's primary taskbar, skipping any `Shell_TrayWnd` owned by this process (the tray host owns one).
pub fn find_native_taskbar() -> Option<HwndLease> {
    let class = wide(TRAY_CLASS);
    let mut candidate: HWND = std::ptr::null_mut();
    loop {
        // SAFETY: class is null-terminated; candidate is the previous result or null.
        candidate = unsafe {
            wm::FindWindowExW(
                std::ptr::null_mut(),
                candidate,
                class.as_ptr(),
                std::ptr::null(),
            )
        };
        if candidate.is_null() {
            return None;
        }
        if let Some(lease) = HwndLease::capture(candidate as Hwnd)
            && lease.is_foreign()
        {
            return Some(lease);
        }
    }
}

/// Explorer's secondary-monitor taskbars.
pub fn find_secondary_taskbars() -> Vec<HwndLease> {
    let mut found = Vec::new();
    enum_windows(|handle| {
        if class_name(handle) == SECONDARY_TRAY_CLASS
            && let Some(lease) = HwndLease::capture(handle)
            && lease.is_foreign()
        {
            found.push(lease);
        }
        true
    });
    found
}

/// Executable file name (without extension) of a process, e.g. "LogonUI".
pub fn process_name(pid: u32) -> Option<String> {
    let path = process_path(pid)?;
    let file = path.rsplit('\\').next()?;
    Some(
        file.rsplit_once('.')
            .map_or(file, |(stem, _)| stem)
            .to_owned(),
    )
}

/// `Process.MainWindowHandle` of a process started from `path`: the first visible, unowned top-level window (in
/// Z order) of any such process.
pub fn main_window_for(path: &str) -> Option<Hwnd> {
    struct Search<'a> {
        path: &'a str,
        paths: std::collections::HashMap<u32, Option<String>>,
        found: Option<Hwnd>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is the `Search` below, alive for the whole enumeration.
        let search = unsafe { &mut *(lparam as *mut Search) };
        // SAFETY: plain Win32 queries on a window handed to us by EnumWindows.
        unsafe {
            if wm::IsWindowVisible(hwnd) == 0 || !wm::GetWindow(hwnd, wm::GW_OWNER).is_null() {
                return 1;
            }
            let mut pid = 0u32;
            wm::GetWindowThreadProcessId(hwnd, &mut pid);
            let matches = search
                .paths
                .entry(pid)
                .or_insert_with(|| process_path(pid))
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(search.path));
            if matches {
                search.found = Some(hwnd as Hwnd);
                return 0;
            }
        }
        1
    }
    let mut search = Search {
        path: path.trim(),
        paths: std::collections::HashMap::new(),
        found: None,
    };
    // SAFETY: the callback only runs during this call.
    unsafe { wm::EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.found
}

/// `Process.MainWindowHandle`: the first visible, unowned top-level window (in Z order) of process `pid`.
pub fn main_window_of_process(pid: u32) -> Option<Hwnd> {
    struct Search {
        pid: u32,
        found: Option<Hwnd>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is the `Search` below, alive for the whole enumeration.
        let search = unsafe { &mut *(lparam as *mut Search) };
        // SAFETY: plain Win32 queries on a window handed to us by EnumWindows.
        unsafe {
            let mut pid = 0u32;
            wm::GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == search.pid
                && wm::IsWindowVisible(hwnd) != 0
                && wm::GetWindow(hwnd, wm::GW_OWNER).is_null()
            {
                search.found = Some(hwnd as Hwnd);
                return 0;
            }
        }
        1
    }
    let mut search = Search { pid, found: None };
    // SAFETY: the callback only runs during this call.
    unsafe { wm::EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.found
}

/// Full image path of a process.
pub fn process_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    // SAFETY: the handle is closed below; the buffer length is passed in `size`.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
                != 0;
        CloseHandle(process);
        if !ok {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}
