//! Virtual desktops from documented sources (`VirtualDesktops` in OpenTaskbar): `IVirtualDesktopManager` for
//! window membership and Explorer's registry values for the desktop list.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

use crate::win::Hwnd;

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";

thread_local! {
    /// Per COM apartment: `None` = not tried yet, `Some(None)` = unavailable.
    static MANAGER: RefCell<Option<Option<IVirtualDesktopManager>>> = const { RefCell::new(None) };
}

/// True when the window is on the current desktop, or when that cannot be determined. The calling thread must
/// have COM initialized.
pub fn is_on_current_desktop(window: Hwnd) -> bool {
    MANAGER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let manager = slot.get_or_insert_with(|| {
            // SAFETY: COM is initialized on this thread by the caller.
            let created: windows::core::Result<IVirtualDesktopManager> =
                unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) };
            if let Err(error) = &created {
                ot_core::log::warn(format!("IVirtualDesktopManager unavailable: {error}"));
            }
            created.ok()
        });
        match manager {
            // SAFETY: valid interface; any HWND value is accepted and errors are handled.
            Some(manager) => {
                unsafe { manager.IsWindowOnCurrentVirtualDesktop(HWND(window as *mut _)) }
                    .map(|on| on.as_bool())
                    .unwrap_or(true)
            }
            None => true,
        }
    })
}

/// 1-based index of the current desktop and the desktop count; (1, 1) with a single desktop.
pub fn read() -> (usize, usize) {
    let Some(ids) = read_binary(KEY, "VirtualDesktopIDs").filter(|ids| ids.len() >= 16) else {
        return (1, 1);
    };
    let count = ids.len() / 16;
    let current = read_binary(KEY, "CurrentVirtualDesktop").or_else(read_session_current);
    let Some(current) = current.filter(|c| c.len() == 16) else {
        return (1, count);
    };
    let index = ids
        .as_chunks::<16>()
        .0
        .iter()
        .position(|id| id.as_slice() == current.as_slice())
        .map_or(1, |i| i + 1);
    (index, count)
}

/// Windows 10 keeps the current desktop per logon session.
fn read_session_current() -> Option<Vec<u8>> {
    let mut session = 0u32;
    // SAFETY: valid out pointer.
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
        return None;
    }
    read_binary(
        &format!(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}\VirtualDesktops"
        ),
        "CurrentVirtualDesktop",
    )
}

fn read_binary(key: &str, value: &str) -> Option<Vec<u8>> {
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut size = 0u32;
    // SAFETY: first call queries the size; the second fills a buffer of that size.
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        ) != 0
        {
            return None;
        }
        buffer.truncate(size as usize);
        Some(buffer)
    }
}
