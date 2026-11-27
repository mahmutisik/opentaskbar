//! Notification-area hosting (OpenTaskbar's `TrayHost` on ManagedShell's `TrayService` / `NotificationArea`):
//! while Explorer's taskbar is hidden, background apps' tray icons and their own menus stay reachable.
//!
//! The host owns an invisible topmost `Shell_TrayWnd` (with a `TrayNotifyWnd` child). `Shell_NotifyIcon` and
//! `SHAppBarMessage` find the first `Shell_TrayWnd` in Z order, so their `WM_COPYDATA` reaches this window:
//! - `dwData` 1 (icon added, changed, removed): kept here and answered, not forwarded;
//! - `dwData` 3 (`Shell_NotifyIconGetRect`): answered from the icon's place in the dock;
//! - `dwData` 0 (app bars, including the dock's own): forwarded to Explorer's taskbar; the shared-memory messages
//!   are re-allocated for Explorer's process first, as OpenTaskbar does (#86, #92);
//! - other messages ManagedShell forwards (`WM_COPYDATA`, `WM_ACTIVATEAPP`, `WM_COMMAND`, `WM_USER` and above) go
//!   to Explorer's taskbar too, with a timeout so a hung Explorer cannot freeze the dock.
//!
//! Starting broadcasts `TaskbarCreated`, so running apps register their icons again with this window; stopping
//! destroys the window and broadcasts it again, so they return to Explorer. The broadcasts carry a marker in
//! `wParam` and a sequence in `lParam`, so the dock can tell its own broadcast from an Explorer restart.
//!
//! Unlike ManagedShell, the host does not read Explorer's existing icons (Windows 11 has no tray toolbar to read,
//! and doing it means switching `EnableAutoTray` off and on in the user's registry) and starts no shell service
//! objects (Explorer stays the shell).

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use ot_core::log;
use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{SHAllocShared, SHFreeShared, SHLockShared, SHUnlockShared};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::win::{self, Hwnd};

/// `wParam` of our own `TaskbarCreated` broadcasts: 1 from the host, 2 from a restore (watchdog, `--restore`).
pub const SELF_BROADCAST: usize = 1;
pub const RESTORE_BROADCAST: usize = 2;

static LAST_SELF_SEQUENCE: AtomicIsize = AtomicIsize::new(0);

/// True for the `TaskbarCreated` this process broadcast itself (it must not be handled as an Explorer restart).
pub fn is_own_broadcast(wparam: usize, lparam: isize) -> bool {
    wparam == SELF_BROADCAST && lparam != 0 && lparam == LAST_SELF_SEQUENCE.load(Ordering::SeqCst)
}

/// Asks every app to register its tray icons with whichever `Shell_TrayWnd` is first now. Fire and forget
/// (`SendNotifyMessage`): Explorer or a hung app can never block the caller.
pub fn broadcast_taskbar_created(restore: bool) {
    let message = crate::session::taskbar_created_message();
    if message == 0 {
        return;
    }
    let sequence = log::next_sequence() as isize;
    if !restore {
        LAST_SELF_SEQUENCE.store(sequence, Ordering::SeqCst);
    }
    log::event(
        "TaskbarBroadcast",
        &[
            ("source", &if restore { "restore" } else { "self" }),
            ("seq", &sequence),
        ],
    );
    // SAFETY: broadcast of a registered message with plain integers.
    unsafe {
        wm::SendNotifyMessageW(
            wm::HWND_BROADCAST,
            message,
            if restore {
                RESTORE_BROADCAST
            } else {
                SELF_BROADCAST
            },
            sequence,
        )
    };
}

// ---------- Wire formats (fixed layouts: handles are 32-bit so 32- and 64-bit senders agree) ----------

/// `SHELLTRAYDATA`: signature, `NIM_*`, then `NOTIFYICONDATA` with 32-bit handles (956 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct ShellTrayData {
    signature: u32,
    message: u32,
    nid: NotifyIconData32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback_message: u32,
    icon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: u32,
}

/// `WINNOTIFYICONIDENTIFIER` (`Shell_NotifyIconGetRect`).
#[repr(C)]
#[derive(Clone, Copy)]
struct IconIdentifier {
    magic: u32,
    message: u32,
    cb_size: u32,
    padding: u32,
    hwnd: u32,
    uid: u32,
    guid: [u8; 16],
}

/// `APPBARDATAV2` (40 bytes) and `APPBARMSGDATAV3` (64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct AppBarData2 {
    cb_size: u32,
    hwnd: u32,
    callback_message: u32,
    edge: u32,
    rc: [i32; 4],
    lparam: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AppBarMessageData3 {
    abd: AppBarData2,
    message: u32,
    padding1: u32,
    shared_memory: i64,
    source_process_id: u32,
    padding3: u32,
}

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_STATE: u32 = 0x8;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;
/// `ABM_QUERYPOS`, `ABM_SETPOS`, `ABM_GETTASKBARPOS`, `ABM_GETAUTOHIDEBAREX`: their data travels in shared memory.
const SHARED_MEMORY_MESSAGES: [u32; 4] = [2, 3, 5, 11];
/// ManagedShell posts this one instead of sending it.
const POSTED_MESSAGE: u32 = wm::WM_USER + 372;
const FORWARD_TIMEOUT_MS: u32 = 1000;
const ZORDER_CHECK_MS: u32 = 100;
const TIMER_ZORDER: usize = 1;

/// The shell's own icons (volume, network, power, …): the dock has its own widgets for these.
pub const SYSTEM_ICON_GUIDS: [&str; 9] = [
    "7820ae73-23e3-4229-82c1-e41cb67d5b9c", // volume
    "7820ae74-23e3-4229-82c1-e41cb67d5b9c", // network
    "7820ae75-23e3-4229-82c1-e41cb67d5b9c", // power
    "7820ae76-23e3-4229-82c1-e41cb67d5b9c", // health
    "7820ae77-23e3-4229-82c1-e41cb67d5b9c", // location
    "7820ae78-23e3-4229-82c1-e41cb67d5b9c", // hardware
    "7820ae81-23e3-4229-82c1-e41cb67d5b9c", // update
    "7820ae82-23e3-4229-82c1-e41cb67d5b9c", // microphone
    "7820ae83-23e3-4229-82c1-e41cb67d5b9c", // meet now
];

// ---------- Icons ----------

/// A copied icon handle owned by the host (the app's own may be destroyed at any time).
pub struct OwnedIcon(pub isize);

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: a copy this host created.
            unsafe { wm::DestroyIcon(self.0 as wm::HICON) };
        }
    }
}

pub struct TrayIcon {
    pub hwnd: Hwnd,
    pub uid: u32,
    /// Lowercase GUID text, empty when the icon has none.
    pub guid: String,
    pub callback_message: u32,
    pub version: u32,
    pub icon: Option<std::rc::Rc<OwnedIcon>>,
    pub tip: String,
    pub hidden: bool,
    pub executable_path: Option<String>,
    /// Where the dock shows the icon (screen pixels), answered to `Shell_NotifyIconGetRect`.
    pub placement: Option<(i32, i32, i32, i32)>,
    last_left: Option<Instant>,
    last_right: Option<Instant>,
    last_middle: Option<Instant>,
}

impl TrayIcon {
    fn matches(&self, hwnd: Hwnd, uid: u32, guid: &str) -> bool {
        (self.hwnd == hwnd && self.uid == uid) || (!guid.is_empty() && self.guid == guid)
    }

    /// An app icon worth showing: visible, owned by a live window, not one of the shell's system icons.
    pub fn is_app_icon(&self) -> bool {
        !self.hidden
            && self.hwnd != 0
            && win::is_window(self.hwnd)
            && !SYSTEM_ICON_GUIDS.contains(&self.guid.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

struct Host {
    tray: Hwnd,
    notify: Hwnd,
    icons: Vec<TrayIcon>,
    changed: Box<dyn Fn()>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

pub fn is_running() -> bool {
    HOST.with(|h| h.try_borrow().map(|h| h.is_some()).unwrap_or(true))
}

/// Starts hosting on the calling (UI) thread; `changed` runs after icons were added, changed or removed (it is
/// called from the window procedure, so it should only post a message). Returns false when the windows could not
/// be created; the dock then simply has no live tray icons.
pub fn start(changed: impl Fn() + 'static) -> bool {
    if is_running() {
        return true;
    }
    let Some((tray, notify)) = create_windows() else {
        log::warn("Tray host failed to start: windows could not be created.");
        return false;
    };
    HOST.with(|h| {
        *h.borrow_mut() = Some(Host {
            tray,
            notify,
            icons: Vec::new(),
            changed: Box::new(changed),
        })
    });
    // `TrayService.Resume`: Explorer's taskbar to the bottom, ours topmost, and kept first.
    if let Some(explorer) = win::find_native_taskbar() {
        // SAFETY: Z-order changes without activation.
        unsafe {
            wm::SetWindowPos(
                explorer.handle as HWND,
                wm::HWND_BOTTOM,
                0,
                0,
                0,
                0,
                wm::SWP_NOMOVE | wm::SWP_NOSIZE | wm::SWP_NOACTIVATE,
            )
        };
    }
    raise(tray);
    // SAFETY: timer on our own window.
    unsafe { wm::SetTimer(tray as HWND, TIMER_ZORDER, ZORDER_CHECK_MS, None) };
    broadcast_taskbar_created(false);
    log::info("Tray host started.");
    true
}

/// Stops hosting: the windows go away and the apps are asked to register their icons with Explorer again.
pub fn stop() {
    let host = HOST.with(|h| h.try_borrow_mut().ok().and_then(|mut h| h.take()));
    let Some(host) = host else {
        return;
    };
    // SAFETY: our own windows.
    unsafe {
        wm::KillTimer(host.tray as HWND, TIMER_ZORDER);
        wm::DestroyWindow(host.notify as HWND);
        wm::DestroyWindow(host.tray as HWND);
    }
    drop(host);
    broadcast_taskbar_created(false);
    log::info("Tray host stopped; icons handed back to Explorer.");
}

/// Runs `f` over the current icons (in registration order).
pub fn with_icons<R>(f: impl FnOnce(&[TrayIcon]) -> R) -> Option<R> {
    HOST.with(|h| {
        h.try_borrow()
            .ok()
            .and_then(|h| h.as_ref().map(|h| f(&h.icons)))
    })
}

/// Records where the dock shows an icon, for `Shell_NotifyIconGetRect` (apps place their flyouts near it).
pub fn set_placement(hwnd: Hwnd, uid: u32, rect: Option<(i32, i32, i32, i32)>) {
    HOST.with(|h| {
        if let Ok(mut h) = h.try_borrow_mut()
            && let Some(host) = h.as_mut()
            && let Some(icon) = host
                .icons
                .iter_mut()
                .find(|i| i.hwnd == hwnd && i.uid == uid)
        {
            icon.placement = rect;
        }
    });
}

/// `NotifyIcon.IconMouseDown` + `IconMouseUp`: the clicks the taskbar sends, so the app opens its window or its own
/// menu. The owner may take the foreground first, so its menu closes when the user clicks elsewhere.
pub fn click(hwnd: Hwnd, uid: u32, button: Button) {
    let mut cursor = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: plain Win32 calls.
    let double_click = unsafe {
        wm::GetCursorPos(&mut cursor);
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime()
    };
    let position = ((cursor.y as u32 & 0xFFFF) << 16) | (cursor.x as u32 & 0xFFFF);
    let sends = HOST.with(|h| {
        let mut h = h.try_borrow_mut().ok()?;
        let host = h.as_mut()?;
        let icon = host
            .icons
            .iter_mut()
            .find(|i| i.hwnd == hwnd && i.uid == uid)?;
        Some(click_messages(
            icon,
            button,
            position as usize,
            double_click,
        ))
    });
    let Some(sends) = sends else {
        return;
    };
    let pid = win::process_id(hwnd);
    if pid != 0 {
        // SAFETY: plain Win32 call; only the owner process gets the right.
        unsafe { wm::AllowSetForegroundWindow(pid) };
    }
    for (message, wparam, lparam) in sends {
        // SAFETY: notifications to the icon's own window; asynchronous across threads.
        unsafe { wm::SendNotifyMessageW(hwnd as HWND, message, wparam, lparam) };
    }
}

/// The callback messages for a click: down (or double-click) then up, plus `NIN_SELECT` / `WM_CONTEXTMENU` for
/// version 3 and later (Explorer does this for version 3 too).
fn click_messages(
    icon: &mut TrayIcon,
    button: Button,
    mouse: usize,
    double_click_ms: u32,
) -> Vec<(u32, usize, isize)> {
    let now = Instant::now();
    let last = match button {
        Button::Left => &mut icon.last_left,
        Button::Right => &mut icon.last_right,
        Button::Middle => &mut icon.last_middle,
    };
    let double =
        last.is_some_and(|t| now.duration_since(t).as_millis() <= u128::from(double_click_ms));
    *last = Some(now);
    let (down, double_message, up) = match button {
        Button::Left => (wm::WM_LBUTTONDOWN, wm::WM_LBUTTONDBLCLK, wm::WM_LBUTTONUP),
        Button::Right => (wm::WM_RBUTTONDOWN, wm::WM_RBUTTONDBLCLK, wm::WM_RBUTTONUP),
        Button::Middle => (wm::WM_MBUTTONDOWN, wm::WM_MBUTTONDBLCLK, wm::WM_MBUTTONUP),
    };
    let mut events = vec![if double { double_message } else { down }, up];
    if icon.version >= 3 {
        match button {
            Button::Left => events.push(0x400), // NIN_SELECT
            Button::Right => events.push(wm::WM_CONTEXTMENU),
            Button::Middle => {}
        }
    }
    events
        .into_iter()
        .map(|event| {
            let (wparam, lparam) = if icon.version > 3 {
                (mouse, (event | ((icon.uid & 0xFFFF) << 16)) as isize)
            } else {
                (icon.uid as usize, event as isize)
            };
            (icon.callback_message, wparam, lparam)
        })
        .collect()
}

// ---------- Windows ----------

fn create_windows() -> Option<(Hwnd, Hwnd)> {
    let tray_class: Vec<u16> = "Shell_TrayWnd".encode_utf16().chain(Some(0)).collect();
    let notify_class: Vec<u16> = "TrayNotifyWnd".encode_utf16().chain(Some(0)).collect();
    let empty: Vec<u16> = vec![0];
    // SAFETY: class registration and window creation with null-terminated strings.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        for class in [&tray_class, &notify_class] {
            let wc = wm::WNDCLASSEXW {
                cbSize: size_of::<wm::WNDCLASSEXW>() as u32,
                style: wm::CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            wm::RegisterClassExW(&wc);
        }
        let width = wm::GetSystemMetrics(wm::SM_CXSCREEN);
        let tray = wm::CreateWindowExW(
            wm::WS_EX_TOPMOST | wm::WS_EX_TOOLWINDOW,
            tray_class.as_ptr(),
            empty.as_ptr(),
            wm::WS_POPUP | wm::WS_CLIPCHILDREN | wm::WS_CLIPSIBLINGS,
            0,
            0,
            width,
            23,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if tray.is_null() {
            return None;
        }
        let notify = wm::CreateWindowExW(
            0,
            notify_class.as_ptr(),
            empty.as_ptr(),
            wm::WS_CHILD | wm::WS_CLIPCHILDREN | wm::WS_CLIPSIBLINGS,
            0,
            0,
            width,
            23,
            tray,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        Some((tray as Hwnd, notify as Hwnd))
    }
}

fn raise(tray: Hwnd) {
    // SAFETY: Z-order change of our own window without activation.
    unsafe {
        wm::SetWindowPos(
            tray as HWND,
            wm::HWND_TOPMOST,
            0,
            0,
            0,
            0,
            wm::SWP_NOMOVE | wm::SWP_NOSIZE | wm::SWP_NOACTIVATE,
        )
    };
}

/// `TrayMonitor_Tick`: if another `Shell_TrayWnd` came first in Z order, ours goes back on top.
fn keep_first(tray: Hwnd) {
    let class: Vec<u16> = "Shell_TrayWnd".encode_utf16().chain(Some(0)).collect();
    // SAFETY: lookup with a null-terminated class name.
    let first = unsafe { wm::FindWindowW(class.as_ptr(), std::ptr::null()) } as Hwnd;
    if first != tray {
        raise(tray);
    }
}

fn guid_text(bytes: &[u8; 16]) -> String {
    if bytes.iter().all(|&b| b == 0) {
        return String::new();
    }
    let d1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let d2 = u16::from_le_bytes([bytes[4], bytes[5]]);
    let d3 = u16::from_le_bytes([bytes[6], bytes[7]]);
    format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn wide_text(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

/// `SysTrayCallback`: true when handled here (the sender gets TRUE), false to forward to Explorer.
fn notify_icon(host: &mut Host, message: u32, nid: &NotifyIconData32) -> bool {
    let hwnd = nid.hwnd as Hwnd;
    let guid = guid_text(&nid.guid);
    if hwnd == 0 && (guid.is_empty() || message == NIM_ADD) {
        return false;
    }
    match message {
        NIM_ADD | NIM_MODIFY => {
            let index = host
                .icons
                .iter()
                .position(|i| i.matches(hwnd, nid.uid, &guid));
            let is_new = index.is_none();
            if is_new && hwnd == 0 {
                return false;
            }
            let index = index.unwrap_or_else(|| {
                host.icons.push(TrayIcon {
                    hwnd,
                    uid: nid.uid,
                    guid: String::new(),
                    callback_message: 0,
                    version: 0,
                    icon: None,
                    tip: String::new(),
                    hidden: false,
                    executable_path: crate::tracker::executable_path(win::process_id(hwnd)),
                    placement: None,
                    last_left: None,
                    last_right: None,
                    last_middle: None,
                });
                host.icons.len() - 1
            });
            let icon = &mut host.icons[index];
            if nid.flags & NIF_STATE != 0 {
                // Only the bits in the mask change; ManagedShell reads `dwState == NIS_HIDDEN` when no mask is set.
                if nid.state_mask & NIS_HIDDEN != 0 {
                    icon.hidden = nid.state & NIS_HIDDEN != 0;
                } else if nid.state_mask == 0 {
                    icon.hidden = nid.state == NIS_HIDDEN;
                }
            }
            if nid.flags & NIF_TIP != 0 {
                let tip = wide_text(&nid.tip);
                if !tip.is_empty() {
                    icon.tip = tip;
                }
            }
            if nid.flags & NIF_ICON != 0 {
                icon.icon = if nid.icon == 0 {
                    None
                } else {
                    // SAFETY: the sender's icon handle is a session-wide user object; the copy is ours.
                    let copy = unsafe { wm::CopyIcon(nid.icon as isize as wm::HICON) } as isize;
                    (copy != 0)
                        .then(|| std::rc::Rc::new(OwnedIcon(copy)))
                        .or_else(|| icon.icon.take())
                };
            }
            if hwnd != 0 {
                icon.hwnd = hwnd;
                icon.uid = nid.uid;
            }
            if nid.flags & NIF_GUID != 0 {
                icon.guid = guid;
            }
            if (1..=4).contains(&nid.version) {
                icon.version = nid.version;
            }
            if nid.flags & NIF_MESSAGE != 0 {
                icon.callback_message = nid.callback_message;
            }
            // A MODIFY for an icon we did not have is kept, but reported as an error (ManagedShell).
            !(is_new && message == NIM_MODIFY)
        }
        NIM_DELETE => match host
            .icons
            .iter()
            .position(|i| i.hwnd == hwnd && i.uid == nid.uid)
        {
            Some(index) => {
                host.icons.remove(index);
                true
            }
            None => false,
        },
        NIM_SETVERSION => {
            if nid.version > 4 {
                return false;
            }
            if let Some(icon) = host
                .icons
                .iter_mut()
                .find(|i| i.matches(hwnd, nid.uid, &guid))
            {
                icon.version = nid.version;
            }
            true
        }
        // NIM_SETFOCUS and anything else: accepted without effect.
        _ => true,
    }
}

/// `IconDataCallback`: the icon's corners for `Shell_NotifyIconGetRect` (1: left/top, 2: right/bottom).
fn icon_rect(host: &Host, id: &IconIdentifier) -> LRESULT {
    let guid = guid_text(&id.guid);
    let Some((l, t, r, b)) = host
        .icons
        .iter()
        .find(|i| i.matches(id.hwnd as Hwnd, id.uid, &guid))
        .and_then(|i| i.placement)
    else {
        return 0;
    };
    let pack =
        |lo: i32, hi: i32| ((hi as i16 as i32) << 16 | (lo as i16 as i32 & 0xFFFF)) as LRESULT;
    match id.message {
        1 => pack(l, t),
        2 => pack(r, b),
        _ => 0,
    }
}

fn forward(message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let explorer = win::find_native_taskbar()?;
    let target = explorer.handle as HWND;
    if message == POSTED_MESSAGE {
        // SAFETY: plain message to Explorer's taskbar.
        unsafe { wm::PostMessageW(target, message, wparam, lparam) };
        return Some(0);
    }
    let mut result: usize = 0;
    // SAFETY: as above; a hung Explorer is abandoned after the timeout.
    let ok = unsafe {
        wm::SendMessageTimeoutW(
            target,
            message,
            wparam,
            lparam,
            wm::SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            &mut result,
        )
    };
    (ok != 0).then_some(result as LRESULT)
}

/// `ForwardSharedAppBarMessage`: the app bar data is copied into shared memory Explorer's process owns, sent
/// there, and Explorer's answer is copied back to the caller's buffer, so Explorer can update work areas live.
fn forward_shared_app_bar(amd: &AppBarMessageData3) -> Option<LRESULT> {
    if !SHARED_MEMORY_MESSAGES.contains(&amd.message) || amd.shared_memory == 0 {
        return None;
    }
    let explorer = win::find_native_taskbar()?;
    let explorer_pid = explorer.process_id;
    let source = amd.shared_memory as isize as HANDLE;
    // SAFETY: shared-memory calls with the sender's handle and pid; every lock is unlocked and the new block freed.
    unsafe {
        let old = SHLockShared(source, amd.source_process_id);
        if old.is_null() {
            log::warn(format!(
                "Tray host: SHLockShared failed for app bar message {} (pid {}).",
                amd.message, amd.source_process_id
            ));
            return None;
        }
        let size = size_of::<AppBarData2>() as u32;
        let shared = SHAllocShared(std::ptr::null(), size, explorer_pid);
        let result = (|| {
            if shared.is_null() {
                return None;
            }
            let new = SHLockShared(shared, explorer_pid);
            if new.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(old as *const u8, new as *mut u8, size as usize);
            SHUnlockShared(new);
            let mut message = *amd;
            message.shared_memory = shared as isize as i64;
            message.source_process_id = explorer_pid;
            let copy = COPYDATASTRUCT {
                dwData: 0,
                cbData: size_of::<AppBarMessageData3>() as u32,
                lpData: (&mut message as *mut AppBarMessageData3).cast(),
            };
            let mut answer: usize = 0;
            wm::SendMessageTimeoutW(
                explorer.handle as HWND,
                wm::WM_COPYDATA,
                amd.abd.hwnd as usize,
                (&copy as *const COPYDATASTRUCT) as LPARAM,
                wm::SMTO_ABORTIFHUNG,
                FORWARD_TIMEOUT_MS,
                &mut answer,
            );
            let updated = SHLockShared(shared, explorer_pid);
            if !updated.is_null() {
                std::ptr::copy_nonoverlapping(updated as *const u8, old as *mut u8, size as usize);
                SHUnlockShared(updated);
            }
            Some(answer as LRESULT)
        })();
        if !shared.is_null() {
            SHFreeShared(shared, explorer_pid);
        }
        SHUnlockShared(old);
        result
    }
}

/// Handles `WM_COPYDATA`; `None` when it should be forwarded as it is.
fn copy_data(data: &COPYDATASTRUCT) -> Option<LRESULT> {
    if data.lpData.is_null() {
        return None;
    }
    match data.dwData {
        0 => {
            if data.cbData as usize != size_of::<AppBarMessageData3>() {
                return None;
            }
            // SAFETY: the size was checked; the system copied the data into this process.
            let amd = unsafe { std::ptr::read_unaligned(data.lpData as *const AppBarMessageData3) };
            if amd.abd.cb_size as usize != size_of::<AppBarData2>() {
                return None;
            }
            forward_shared_app_bar(&amd)
        }
        1 => {
            if (data.cbData as usize) < 8 + 24 {
                return None;
            }
            // Senders with an older, smaller NOTIFYICONDATA: the rest reads as zero.
            let mut tray: ShellTrayData = unsafe { std::mem::zeroed() };
            let length = (data.cbData as usize).min(size_of::<ShellTrayData>());
            // SAFETY: at most `cbData` bytes are copied into a zeroed, larger structure.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.lpData as *const u8,
                    (&mut tray as *mut ShellTrayData).cast::<u8>(),
                    length,
                )
            };
            let handled = HOST.with(|h| {
                let mut h = h.try_borrow_mut().ok()?;
                let host = h.as_mut()?;
                let handled = notify_icon(host, tray.message, &tray.nid);
                (host.changed)();
                Some(handled)
            })?;
            handled.then_some(1)
        }
        3 => {
            if (data.cbData as usize) < size_of::<IconIdentifier>() {
                return None;
            }
            // SAFETY: the size was checked.
            let id = unsafe { std::ptr::read_unaligned(data.lpData as *const IconIdentifier) };
            HOST.with(|h| {
                let h = h.try_borrow().ok()?;
                Some(icon_rect(h.as_ref()?, &id))
            })
        }
        _ => None,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_COPYDATA if lparam != 0 => {
            // SAFETY: WM_COPYDATA's lParam is a COPYDATASTRUCT valid for the duration of the message.
            let data = unsafe { &*(lparam as *const COPYDATASTRUCT) };
            // Data from any process: a parsing panic must not unwind out of the window procedure (that aborts the
            // dock); the panic hook logs it and the message is passed on.
            if let Ok(Some(result)) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| copy_data(data)))
            {
                return result;
            }
        }
        wm::WM_TIMER if wparam == TIMER_ZORDER => {
            keep_first(hwnd as Hwnd);
            return 0;
        }
        wm::WM_WINDOWPOSCHANGED => {
            // `TrayService`: the window must never become visible.
            // SAFETY: lParam is a WINDOWPOS for this message; the style change keeps the window hidden.
            unsafe {
                let pos = &*(lparam as *const wm::WINDOWPOS);
                if pos.flags & wm::SWP_SHOWWINDOW != 0 {
                    let style = wm::GetWindowLongPtrW(hwnd, wm::GWL_STYLE);
                    wm::SetWindowLongPtrW(hwnd, wm::GWL_STYLE, style & !(wm::WS_VISIBLE as isize));
                }
            }
            // SAFETY: default processing.
            return unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        _ => {}
    }
    let forwarded = matches!(
        message,
        wm::WM_COPYDATA | wm::WM_ACTIVATEAPP | wm::WM_COMMAND
    ) || message >= wm::WM_USER;
    if forwarded && let Some(result) = forward(message, wparam, lparam) {
        return result;
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_formats_match_the_shell() {
        assert_eq!(size_of::<NotifyIconData32>(), 956);
        assert_eq!(size_of::<ShellTrayData>(), 964);
        assert_eq!(size_of::<IconIdentifier>(), 40);
        assert_eq!(size_of::<AppBarData2>(), 40);
        assert_eq!(size_of::<AppBarMessageData3>(), 64);
        assert_eq!(std::mem::offset_of!(NotifyIconData32, version), 800);
        assert_eq!(std::mem::offset_of!(NotifyIconData32, guid), 936);
    }

    #[test]
    fn guids_format_like_dotnet() {
        let volume = [
            0x73, 0xae, 0x20, 0x78, 0xe3, 0x23, 0x29, 0x42, 0x82, 0xc1, 0xe4, 0x1c, 0xb6, 0x7d,
            0x5b, 0x9c,
        ];
        assert_eq!(guid_text(&volume), SYSTEM_ICON_GUIDS[0]);
        assert_eq!(guid_text(&[0; 16]), "");
    }

    fn icon(version: u32) -> TrayIcon {
        TrayIcon {
            hwnd: 1,
            uid: 7,
            guid: String::new(),
            callback_message: 0x8001,
            version,
            icon: None,
            tip: String::new(),
            hidden: false,
            executable_path: None,
            placement: None,
            last_left: None,
            last_right: None,
            last_middle: None,
        }
    }

    #[test]
    fn clicks_follow_the_icon_version() {
        let mut old = icon(0);
        assert_eq!(
            click_messages(&mut old, Button::Right, 0x0010_0020, 500),
            vec![(0x8001, 7, 0x204), (0x8001, 7, 0x205)]
        );
        let mut v4 = icon(4);
        let sent = click_messages(&mut v4, Button::Right, 0x0010_0020, 500);
        assert_eq!(
            sent,
            vec![
                (0x8001, 0x0010_0020, 0x204 | (7 << 16)),
                (0x8001, 0x0010_0020, 0x205 | (7 << 16)),
                (0x8001, 0x0010_0020, 0x7B | (7 << 16)),
            ]
        );
        // A second left click within the double-click time is a double click.
        let mut v3 = icon(3);
        click_messages(&mut v3, Button::Left, 0, 500);
        let second = click_messages(&mut v3, Button::Left, 0, 500);
        assert_eq!(second[0].2, 0x203);
        assert_eq!(second[2].2, 0x400);
    }
}
