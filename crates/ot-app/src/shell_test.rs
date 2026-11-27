//! `--shell-test[=seconds]`: a placeholder dock window (a plain dark bar) driven by the real taskbar session, to
//! exercise hiding, AppBar reservation and restore before the real dock exists (Phase 3).
//! `--crash-after=seconds` panics inside the message loop to test the panic restore path; killing the process
//! (`taskkill /F`) tests the watchdog.

use std::cell::RefCell;

use ot_core::log;
use ot_shell::session::{self, Session, SessionEvent};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

const TIMER_END: usize = 1;
const TIMER_CRASH: usize = 2;

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Runs the test for `seconds`; returns the stop result.
pub fn run(seconds: u32, crash_after: Option<u32>) -> Result<(), &'static str> {
    let dock = create_window().ok_or("ErrorDockWindow")?;
    let session = Session::new(dock as isize, false, |event| match event {
        SessionEvent::OperationReported(key, error) => {
            log::warn(format!("Session reported {key} (error={error})"))
        }
        SessionEvent::LayoutChanged { width, height } => {
            log::info(format!("Layout {width:.0}x{height:.0} DIP"))
        }
        SessionEvent::FullScreenChanged(on) => log::info(format!("Full screen: {on}")),
    });
    SESSION.with(|slot| *slot.borrow_mut() = Some(session));

    let started = SESSION.with(|slot| slot.borrow_mut().as_mut().map(Session::start));
    if let Some(Err(key)) = started {
        log::error(format!("Session start failed: {key}"));
        SESSION.with(|slot| slot.borrow_mut().take()); // drop restores
        // SAFETY: our own window.
        unsafe { wm::DestroyWindow(dock) };
        return Err(key);
    }
    log::info(format!("Shell test running for {seconds}s."));
    // SAFETY: timers on our own window.
    unsafe {
        wm::ShowWindow(dock, wm::SW_SHOWNOACTIVATE);
        wm::SetTimer(dock, TIMER_END, seconds * 1000, None);
        if let Some(crash) = crash_after {
            wm::SetTimer(dock, TIMER_CRASH, crash * 1000, None);
        }
    }

    let mut msg: wm::MSG = unsafe { std::mem::zeroed() };
    // SAFETY: standard message loop for this thread.
    while unsafe { wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            wm::TranslateMessage(&msg);
            wm::DispatchMessageW(&msg);
        }
    }
    let result = SESSION
        .with(|slot| slot.borrow_mut().as_mut().map(Session::stop))
        .unwrap_or(Ok(()));
    SESSION.with(|slot| slot.borrow_mut().take());
    result
}

fn create_window() -> Option<HWND> {
    let class: Vec<u16> = "OpenTaskBarRust.ShellTest"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: class registration and window creation with valid, null-terminated strings.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = wm::WNDCLASSEXW {
            cbSize: size_of::<wm::WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_ARROW),
            ..std::mem::zeroed()
        };
        wm::RegisterClassExW(&wc);
        let hwnd = wm::CreateWindowExW(
            wm::WS_EX_TOOLWINDOW | wm::WS_EX_TOPMOST | wm::WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            wm::WS_POPUP,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        (!hwnd.is_null()).then_some(hwnd)
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_TIMER if wparam == TIMER_END => {
            // SAFETY: our own window.
            unsafe { wm::DestroyWindow(hwnd) };
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_CRASH => {
            panic!("--crash-after: deliberate panic to test the restore path")
        }
        wm::WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { wm::PostQuitMessage(0) };
            return 0;
        }
        wm::WM_PAINT => {
            paint(hwnd);
            return 0;
        }
        _ => {}
    }
    let handled = SESSION.with(|slot| match slot.try_borrow_mut() {
        Ok(mut session) => session
            .as_mut()
            .is_some_and(|s| s.handle_message(message, wparam, lparam)),
        // Re-entrant: the session is mid-call (e.g. inside SHAppBarMessage). Defer instead of nesting.
        Err(_) => session::defer_reentrant(hwnd as isize, message, wparam),
    });
    if handled {
        return 0;
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn paint(hwnd: HWND) {
    // SAFETY: standard WM_PAINT handling on our own window; the brush is deleted after use.
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rect: RECT = std::mem::zeroed();
        wm::GetClientRect(hwnd, &mut rect);
        let brush = CreateSolidBrush(0x0030_2a28);
        FillRect(dc, &rect, brush);
        DeleteObject(brush);
        EndPaint(hwnd, &ps);
    }
}
