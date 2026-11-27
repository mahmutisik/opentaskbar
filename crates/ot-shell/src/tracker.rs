//! Running application windows (`NativeWindowTracker` and `TaskbarWindowRules` in OpenTaskbar).
//!
//! Which windows count, and when the list is refreshed, follow the C# tracker exactly: WinEvent hooks for
//! create/destroy/show/hide/title/cloak/location/alert/foreground, a 60 ms coalescing window for ordinary
//! events, an immediate scan for events that change visibility, and the shell's own taskbar-button rules.
//!
//! Differences, all invisible to the dock:
//! - Hooks and scans run on a dedicated tracker thread with its own message loop. The UI thread never runs a
//!   scan or a hook callback; results arrive through the `sink` callback.
//! - Per-window facts that cannot change for a given HWND and process (executable path, class, AppUserModelID,
//!   shell-window classification) are cached and re-validated by process ID, so a scan reads only what can
//!   change (visibility, style, cloak, title) and a recycled HWND never inherits another process's identity
//!   (upstream #141).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use ot_core::log;
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::win::{self, Hwnd};

const EVENT_SYSTEM_ALERT: u32 = 0x0002;
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_HIDE: u32 = 0x8003;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;
const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;
/// WINEVENT_OUTOFCONTEXT (0) | WINEVENT_SKIPOWNPROCESS (2).
const WINEVENT_OUTOFCONTEXT_SKIPOWNPROCESS: u32 = 0x0002;

const WM_SCAN: u32 = wm::WM_APP + 1;
const WM_RESYNC: u32 = wm::WM_APP + 2;
const REFRESH_DELAY_MS: u32 = 60;
const LOCATION_DELAY_MS: u32 = 100;

// ---- Taskbar button rules (TaskbarWindowRules) ----

pub const WS_EX_APPWINDOW: isize = 0x0004_0000;
pub const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
pub const WS_EX_NOACTIVATE: isize = 0x0800_0000;
pub const WS_EX_WINDOWEDGE: isize = 0x0000_0100;
pub const DWM_CLOAKED_SHELL: u32 = 0x2;

/// The shell's eligibility check: an owned window needs WS_EX_APPWINDOW, tool and non-activatable windows are
/// hidden unless they opt in, and `ITaskbarList::DeleteTab` overrides everything.
pub fn has_taskbar_button(ex_style: isize, has_owner: bool, deleted_from_taskbar: bool) -> bool {
    let opted_in = ex_style & WS_EX_APPWINDOW != 0;
    !deleted_from_taskbar
        && (!has_owner || opted_in)
        && (opted_in || ex_style & WS_EX_TOOLWINDOW == 0)
        && (opted_in || ex_style & WS_EX_NOACTIVATE == 0)
}

/// Cloaked windows are hidden, except the shell cloak used for windows on other virtual desktops.
pub fn is_hidden_cloak(cloaked: u32) -> bool {
    cloaked & !DWM_CLOAKED_SHELL != 0
}

/// Immersive shell frames without WS_EX_WINDOWEDGE are internal surfaces, not app windows.
pub fn is_hidden_immersive_frame(class_name: &str, ex_style: isize) -> bool {
    ex_style & WS_EX_WINDOWEDGE == 0
        && matches!(
            class_name,
            "Windows.UI.Core.CoreWindow"
                | "ApplicationFrameWindow"
                | "StartMenuSizingFrame"
                | "Shell_LightDismissOverlay"
        )
}

// ---- Public types ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedWindow {
    pub handle: Hwnd,
    pub title: String,
    pub executable_path: Option<String>,
    pub has_notification: bool,
    pub app_user_model_id: Option<String>,
}

#[derive(Clone, Debug)]
pub enum TrackerEvent {
    WindowsChanged(Arc<Vec<TrackedWindow>>),
    ForegroundChanged(Hwnd),
    /// Explorer created (`recreated`: a new primary taskbar, i.e. a new Explorer) or showed one of its taskbar
    /// windows while the dock keeps them hidden.
    TaskbarShown {
        recreated: bool,
    },
}

/// Scan timing, for the performance log.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanStats {
    pub scans: u64,
    pub last_micros: u64,
    pub max_micros: u64,
    pub total_micros: u64,
}

struct Shared {
    snapshot: Mutex<Arc<Vec<TrackedWindow>>>,
    scans: AtomicU64,
    last_micros: AtomicU64,
    max_micros: AtomicU64,
    total_micros: AtomicU64,
}

/// Owns the tracker thread. Dropping it unhooks and stops the thread.
pub struct WindowTracker {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl WindowTracker {
    /// Starts tracking. `excluded` is the dock's own window; `sink` runs on the tracker thread.
    pub fn start(
        excluded: Hwnd,
        sink: impl Fn(TrackerEvent) + Send + 'static,
    ) -> Result<Self, &'static str> {
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Arc::new(Vec::new())),
            scans: AtomicU64::new(0),
            last_micros: AtomicU64::new(0),
            max_micros: AtomicU64::new(0),
            total_micros: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<u32, &'static str>>(1);
        let thread_shared = shared.clone();
        let join = std::thread::Builder::new()
            .name("ot-window-tracker".into())
            .spawn(move || run_thread(excluded, Box::new(sink), thread_shared, ready_tx))
            .map_err(|_| "ErrorWindowEvents")?;
        let thread_id = ready_rx.recv().map_err(|_| "ErrorWindowEvents")??;
        Ok(Self {
            thread_id,
            join: Some(join),
            shared,
        })
    }

    pub fn snapshot(&self) -> Arc<Vec<TrackedWindow>> {
        self.shared
            .snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Drops cached foreground state after a secure-desktop transition (lock screen, UAC).
    pub fn resync_foreground(&self) {
        post(self.thread_id, WM_RESYNC);
    }

    pub fn stats(&self) -> ScanStats {
        ScanStats {
            scans: self.shared.scans.load(Ordering::Relaxed),
            last_micros: self.shared.last_micros.load(Ordering::Relaxed),
            max_micros: self.shared.max_micros.load(Ordering::Relaxed),
            total_micros: self.shared.total_micros.load(Ordering::Relaxed),
        }
    }
}

impl Drop for WindowTracker {
    fn drop(&mut self) {
        post(self.thread_id, wm::WM_QUIT);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn post(thread_id: u32, message: u32) {
    // SAFETY: posting a message to our own tracker thread.
    unsafe { wm::PostThreadMessageW(thread_id, message, 0, 0) };
}

// ---- Tracker thread ----

/// Facts about a window that cannot change while the same process owns the HWND.
#[derive(Clone)]
struct Identity {
    process_id: u32,
    class_name: String,
    executable_path: Option<String>,
    app_user_model_id: Option<String>,
    /// SystemApps executables and Explorer's non-folder windows are never app buttons.
    shell_window: bool,
}

struct State {
    excluded: Hwnd,
    sink: Box<dyn Fn(TrackerEvent) + Send>,
    shared: Arc<Shared>,
    thread_id: u32,
    identities: HashMap<Hwnd, Identity>,
    /// HWND -> PID: a recycled handle must not inherit a notification badge.
    notifications: HashMap<Hwnd, u32>,
    /// Suppresses redundant title events from helper windows; eligibility is still re-evaluated every scan.
    quiet: HashSet<Hwnd>,
    snapshot_handles: HashSet<Hwnd>,
    snapshot: Arc<Vec<TrackedWindow>>,
    last_foreground: Option<Hwnd>,
    pending_location_foreground: Hwnd,
    scan_posted: bool,
    urgent: bool,
    refresh_timer: usize,
    location_timer: usize,
    system_apps_prefix: String,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn run_thread(
    excluded: Hwnd,
    sink: Box<dyn Fn(TrackerEvent) + Send>,
    shared: Arc<Shared>,
    ready: mpsc::SyncSender<Result<u32, &'static str>>,
) {
    // SAFETY: COM for this thread (AppUserModelID property store, virtual desktop manager).
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    // SAFETY: plain Win32 call.
    let thread_id = unsafe { GetCurrentThreadId() };
    // Make sure this thread has a message queue before anyone posts to it.
    let mut msg: wm::MSG = unsafe { std::mem::zeroed() };
    // SAFETY: PM_NOREMOVE peek only creates the queue.
    unsafe { wm::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, wm::PM_NOREMOVE) };

    let hook = |min: u32, max: u32| -> HWINEVENTHOOK {
        // SAFETY: out-of-context hook on this thread; `win_event_proc` only touches this thread's state.
        unsafe {
            SetWinEventHook(
                min,
                max,
                std::ptr::null_mut(),
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT_SKIPOWNPROCESS,
            )
        }
    };
    let windows_hook = hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE);
    if windows_hook.is_null() {
        let _ = ready.send(Err("ErrorWindowEvents"));
        return;
    }
    let name_hook = hook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE);
    if name_hook.is_null() {
        // SAFETY: hook installed above.
        unsafe { UnhookWinEvent(windows_hook) };
        let _ = ready.send(Err("ErrorWindowTitleHook"));
        return;
    }
    let mut hooks = vec![windows_hook, name_hook];
    for (min, max, what) in [
        (
            EVENT_OBJECT_CLOAKED,
            EVENT_OBJECT_UNCLOAKED,
            "Cloak-change hook unavailable; UWP visibility changes may be delayed.",
        ),
        (
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            "Location-change hook unavailable.",
        ),
        (
            EVENT_SYSTEM_ALERT,
            EVENT_SYSTEM_FOREGROUND,
            "Foreground-event hook unavailable.",
        ),
    ] {
        let handle = hook(min, max);
        if handle.is_null() {
            log::warn(what);
        } else {
            hooks.push(handle);
        }
    }

    let windows_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    STATE.with(|slot| {
        *slot.borrow_mut() = Some(State {
            excluded,
            sink,
            shared,
            thread_id,
            identities: HashMap::new(),
            notifications: HashMap::new(),
            quiet: HashSet::new(),
            snapshot_handles: HashSet::new(),
            snapshot: Arc::new(Vec::new()),
            last_foreground: None,
            pending_location_foreground: 0,
            scan_posted: false,
            urgent: false,
            refresh_timer: 0,
            location_timer: 0,
            system_apps_prefix: format!("{}\\SystemApps\\", windows_dir.trim_end_matches('\\'))
                .to_lowercase(),
        });
    });
    with_state(|s| {
        s.scan();
        s.publish_foreground(normalized_foreground(), false);
    });
    let _ = ready.send(Ok(thread_id));

    // SAFETY: standard message loop for this thread.
    while unsafe { wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        match msg.message {
            WM_SCAN => with_state(State::on_scan_message),
            WM_RESYNC => with_state(|s| {
                s.stop_timers();
                s.pending_location_foreground = 0;
                s.scan();
                s.publish_foreground(normalized_foreground(), false);
            }),
            wm::WM_TIMER => with_state(|s| s.on_timer(msg.wParam)),
            _ => unsafe {
                wm::TranslateMessage(&msg);
                wm::DispatchMessageW(&msg);
            },
        }
    }

    for handle in hooks {
        // SAFETY: hooks installed on this thread.
        unsafe { UnhookWinEvent(handle) };
    }
    with_state(State::stop_timers);
    STATE.with(|slot| slot.borrow_mut().take());
    if com {
        // SAFETY: balances CoInitializeEx on this thread.
        unsafe { CoUninitialize() };
    }
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|slot| {
        // A hook callback can run while a scan is mid-call (COM pumps messages); skip rather than re-enter.
        if let Ok(mut state) = slot.try_borrow_mut()
            && let Some(state) = state.as_mut()
        {
            f(state);
        }
    });
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: windows_sys::Win32::Foundation::HWND,
    object_id: i32,
    child_id: i32,
    _thread: u32,
    _time: u32,
) {
    let handle = hwnd as Hwnd;
    // A re-entrant callback (during a scan's COM call) is recorded as a pending urgent scan instead.
    // A panic must not unwind out of a system callback (that aborts the whole dock): it is logged by the panic
    // hook and this one event is lost.
    let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        STATE.with(|slot| match slot.try_borrow_mut() {
            Ok(mut state) => {
                if let Some(state) = state.as_mut() {
                    state.on_event(event, handle, object_id, child_id);
                }
                true
            }
            Err(_) => false,
        })
    }))
    .unwrap_or(true);
    if !handled {
        // SAFETY: post to this thread's own queue; processed after the current scan.
        unsafe { wm::PostThreadMessageW(GetCurrentThreadId(), WM_SCAN, 1, 0) };
    }
}

fn normalize_root(handle: Hwnd) -> Hwnd {
    if handle == 0 {
        return 0;
    }
    let root = win::root_owner(handle);
    if root != 0 { root } else { handle }
}

fn normalized_foreground() -> Hwnd {
    normalize_root(win::foreground())
}

impl State {
    fn on_event(&mut self, event: u32, handle: Hwnd, object_id: i32, child_id: i32) {
        match event {
            EVENT_OBJECT_LOCATIONCHANGE => {
                if object_id != 0 || child_id != 0 || handle == 0 {
                    return;
                }
                let foreground = win::foreground();
                if foreground == 0 {
                    return;
                }
                let root = normalize_root(foreground);
                if handle == foreground || handle == root {
                    self.pending_location_foreground = root;
                    if self.location_timer == 0 {
                        self.location_timer = thread_timer(LOCATION_DELAY_MS);
                    }
                }
            }
            EVENT_SYSTEM_ALERT => {
                let target = normalize_root(handle);
                if target != 0
                    && target != self.excluded
                    && win::is_visible(target)
                    && win::owner(target) == 0
                {
                    self.notifications.insert(target, win::process_id(target));
                    self.queue_refresh(false);
                }
            }
            EVENT_SYSTEM_FOREGROUND => {
                let target = normalize_root(handle);
                self.notifications.remove(&handle);
                self.notifications.remove(&target);
                kill_thread_timer(&mut self.location_timer);
                self.queue_refresh(true);
                self.publish_foreground(target, false);
            }
            EVENT_OBJECT_CREATE
            | EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_SHOW
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_NAMECHANGE
            | EVENT_OBJECT_CLOAKED
            | EVENT_OBJECT_UNCLOAKED => {
                if object_id != 0 || child_id != 0 {
                    return;
                }
                if (event == EVENT_OBJECT_SHOW || event == EVENT_OBJECT_CREATE) && handle != 0 {
                    // Explorer's own taskbars (this process's tray host is skipped by the hook flags).
                    let class = win::class_name(handle);
                    if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
                        (self.sink)(TrackerEvent::TaskbarShown {
                            recreated: event == EVENT_OBJECT_CREATE && class == "Shell_TrayWnd",
                        });
                        return;
                    }
                }
                if event == EVENT_OBJECT_CREATE || event == EVENT_OBJECT_DESTROY {
                    self.quiet.remove(&handle);
                } else if event == EVENT_OBJECT_NAMECHANGE && self.quiet.contains(&handle) {
                    return;
                }
                let urgent = matches!(
                    event,
                    EVENT_OBJECT_SHOW
                        | EVENT_OBJECT_DESTROY
                        | EVENT_OBJECT_CLOAKED
                        | EVENT_OBJECT_UNCLOAKED
                ) || (event == EVENT_OBJECT_NAMECHANGE
                    && !self.snapshot_handles.contains(&handle));
                self.queue_refresh(urgent);
            }
            _ => {}
        }
    }

    /// Coalesces bursts: one posted message per burst; urgent events scan as soon as the burst is drained,
    /// ordinary ones within 60 ms of the first event (later events never postpone it).
    fn queue_refresh(&mut self, urgent: bool) {
        self.urgent |= urgent;
        if !self.scan_posted {
            self.scan_posted = true;
            post(self.thread_id, WM_SCAN);
        }
    }

    fn on_scan_message(&mut self) {
        self.scan_posted = false;
        if std::mem::take(&mut self.urgent) {
            self.flush();
        } else if self.refresh_timer == 0 {
            self.refresh_timer = thread_timer(REFRESH_DELAY_MS);
        }
    }

    fn flush(&mut self) {
        kill_thread_timer(&mut self.refresh_timer);
        self.scan();
        self.publish_foreground(normalized_foreground(), false);
    }

    fn on_timer(&mut self, id: usize) {
        if id != 0 && id == self.refresh_timer {
            self.flush();
        } else if id != 0 && id == self.location_timer {
            kill_thread_timer(&mut self.location_timer);
            let foreground = win::foreground();
            let target = if foreground != 0 {
                normalize_root(foreground)
            } else {
                self.pending_location_foreground
            };
            self.pending_location_foreground = 0;
            if target != 0 && win::is_window(target) {
                self.publish_foreground(target, true);
            }
        }
    }

    fn stop_timers(&mut self) {
        kill_thread_timer(&mut self.refresh_timer);
        kill_thread_timer(&mut self.location_timer);
    }

    /// Publishes a foreground change, deduplicated unless forced (location changes re-trigger auto-hide checks).
    fn publish_foreground(&mut self, target: Hwnd, force: bool) {
        if !force && self.last_foreground == Some(target) {
            return;
        }
        self.last_foreground = Some(target);
        (self.sink)(TrackerEvent::ForegroundChanged(target));
    }

    fn scan(&mut self) {
        let started = Instant::now();
        let own_pid = std::process::id();
        let mut pending = Vec::with_capacity(self.snapshot.len() + 4);
        let mut seen = HashSet::with_capacity(self.identities.len());
        let mut quiet_found = Vec::new();
        let snapshot = self.snapshot.clone();
        let previous: HashMap<Hwnd, &TrackedWindow> =
            snapshot.iter().map(|w| (w.handle, w)).collect();
        let mut title_buffer = vec![0u16; 1024];
        let mut handles = Vec::with_capacity(256);
        win::enum_windows(|handle| {
            handles.push(handle);
            true
        });

        for handle in handles {
            if handle == self.excluded || !win::is_visible(handle) {
                continue;
            }
            let pid = win::process_id(handle);
            if pid == 0 || pid == own_pid {
                continue;
            }
            let ex_style = win::ex_style(handle);
            if !has_taskbar_button(
                ex_style,
                win::owner(handle) != 0,
                deleted_from_taskbar(handle),
            ) {
                continue;
            }
            if let Some(cloaked) = cloak_reason(handle)
                && is_hidden_cloak(cloaked)
            {
                continue;
            }
            seen.insert(handle);
            let identity = self.identity(handle, pid);
            if is_hidden_immersive_frame(&identity.class_name, ex_style) || identity.shell_window {
                quiet_found.push(handle);
                continue;
            }
            let Some(title) = window_title(handle, &mut title_buffer) else {
                continue;
            };
            let has_notification = self.notifications.get(&handle).is_some_and(|&p| p == pid);
            let previous = previous.get(&handle);
            let title = match previous {
                Some(p) if p.title == title => p.title.clone(),
                _ => title,
            };
            pending.push(TrackedWindow {
                handle,
                title,
                executable_path: identity.executable_path.clone(),
                has_notification,
                app_user_model_id: identity.app_user_model_id.clone(),
            });
        }

        // Forget identities of windows that are gone, and quiet/notification entries of dead handles.
        self.identities.retain(|handle, _| seen.contains(handle));
        self.quiet.extend(quiet_found);
        self.quiet.retain(|&handle| win::is_window(handle));
        self.notifications
            .retain(|&handle, &mut pid| win::is_window(handle) && win::process_id(handle) == pid);

        let micros = started.elapsed().as_micros() as u64;
        let shared = &self.shared;
        shared.scans.fetch_add(1, Ordering::Relaxed);
        shared.last_micros.store(micros, Ordering::Relaxed);
        shared.max_micros.fetch_max(micros, Ordering::Relaxed);
        shared.total_micros.fetch_add(micros, Ordering::Relaxed);

        if *self.snapshot == pending {
            return;
        }
        let next = Arc::new(pending);
        self.snapshot_handles = next.iter().map(|w| w.handle).collect();
        self.snapshot = next.clone();
        if let Ok(mut shared) = self.shared.snapshot.lock() {
            *shared = next.clone();
        }
        (self.sink)(TrackerEvent::WindowsChanged(next));
    }

    /// Cached identity, re-read when the HWND now belongs to another process (or the path was not readable yet).
    fn identity(&mut self, handle: Hwnd, pid: u32) -> Identity {
        if let Some(identity) = self.identities.get(&handle)
            && identity.process_id == pid
            && identity.executable_path.is_some()
        {
            return identity.clone();
        }
        let class_name = win::class_name(handle);
        let executable_path = executable_path(pid);
        let lower = executable_path.as_deref().map(str::to_lowercase);
        let system_app = lower
            .as_deref()
            .is_some_and(|p| p.starts_with(&self.system_apps_prefix));
        let explorer_shell = lower
            .as_deref()
            .is_some_and(|p| p.ends_with("\\explorer.exe"))
            && class_name != "CabinetWClass"
            && class_name != "ExploreWClass";
        let reused = self.identities.get(&handle).filter(|i| i.process_id == pid);
        let app_user_model_id = match reused {
            Some(previous) => previous.app_user_model_id.clone(),
            None => window_app_id(handle),
        };
        let identity = Identity {
            process_id: pid,
            class_name,
            executable_path,
            app_user_model_id,
            shell_window: system_app || explorer_shell,
        };
        self.identities.insert(handle, identity.clone());
        identity
    }
}

fn thread_timer(ms: u32) -> usize {
    // SAFETY: a thread timer (no window); WM_TIMER arrives in this thread's loop with the returned id.
    unsafe { wm::SetTimer(std::ptr::null_mut(), 0, ms, None) }
}

fn kill_thread_timer(id: &mut usize) {
    if *id != 0 {
        // SAFETY: killing our own thread timer.
        unsafe { wm::KillTimer(std::ptr::null_mut(), *id) };
        *id = 0;
    }
}

fn deleted_from_taskbar(handle: Hwnd) -> bool {
    static NAME: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    let name = NAME.get_or_init(|| "ITaskList_Deleted".encode_utf16().chain(Some(0)).collect());
    // SAFETY: null-terminated property name.
    !unsafe { wm::GetPropW(win::hwnd(handle), name.as_ptr()) }.is_null()
}

fn cloak_reason(handle: Hwnd) -> Option<u32> {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    let mut cloaked = 0u32;
    // SAFETY: 4-byte attribute into a 4-byte buffer.
    let hr = unsafe {
        DwmGetWindowAttribute(
            win::hwnd(handle),
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            4,
        )
    };
    (hr == 0).then_some(cloaked)
}

/// Trimmed window title, or `None` when empty.
fn window_title(handle: Hwnd, buffer: &mut [u16]) -> Option<String> {
    // SAFETY: plain Win32 calls; the buffer length is passed.
    let length = unsafe { wm::GetWindowTextLengthW(win::hwnd(handle)) };
    if length <= 0 {
        return None;
    }
    let capacity = (length as usize + 1).min(buffer.len());
    let read =
        unsafe { wm::GetWindowTextW(win::hwnd(handle), buffer.as_mut_ptr(), capacity as i32) };
    if read <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buffer[..read as usize]);
    let trimmed = title.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Full executable path of a process.
pub fn executable_path(pid: u32) -> Option<String> {
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
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        let mut ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
                != 0;
        if !ok {
            buffer = vec![0u16; 32768];
            size = buffer.len() as u32;
            ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            ) != 0;
        }
        CloseHandle(process);
        ok.then(|| String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

/// The window's Shell AppUserModelID (`PKEY_AppUserModel_ID`), so windows of one app group together.
pub fn window_app_id(handle: Hwnd) -> Option<String> {
    // SAFETY: COM is initialized on the tracker thread; the returned string is freed with CoTaskMemFree.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(HWND(handle as *mut _)).ok()?;
        let mut value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let text = PropVariantToStringAlloc(&value);
        // The value owns its string; it is not freed on drop.
        let _ = PropVariantClear(&mut value);
        let text = text.ok()?;
        let id = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        id.filter(|id| !id.trim().is_empty())
    }
}

// ---- Activation (dock clicks) ----

/// Brings a window to the foreground, restoring it if minimized. Grants foreground rights only to the target app.
pub fn activate(handle: Hwnd) -> bool {
    use windows_sys::Win32::System::Threading::AttachThreadInput;
    if !win::is_window(handle) {
        return false;
    }
    let h = win::hwnd(handle);
    // SAFETY: plain Win32 calls on a validated window; thread input is detached in all paths.
    unsafe {
        if win::is_iconic(handle) {
            wm::ShowWindowAsync(h, wm::SW_RESTORE);
        }
        let pid = win::process_id(handle);
        if pid != 0 {
            wm::AllowSetForegroundWindow(pid);
        }
        // Fast path: a dock click is the last input event, so a plain request usually succeeds.
        if wm::SetForegroundWindow(h) != 0 && is_foreground_root(handle) {
            return true;
        }
        let foreground = win::foreground();
        let foreground_thread = if foreground != 0 {
            wm::GetWindowThreadProcessId(win::hwnd(foreground), std::ptr::null_mut())
        } else {
            0
        };
        let target_thread = wm::GetWindowThreadProcessId(h, std::ptr::null_mut());
        let current = GetCurrentThreadId();
        let attached_foreground = foreground_thread != 0
            && foreground_thread != current
            && AttachThreadInput(current, foreground_thread, 1) != 0;
        let attached_target = target_thread != 0
            && target_thread != current
            && target_thread != foreground_thread
            && AttachThreadInput(current, target_thread, 1) != 0;
        wm::BringWindowToTop(h);
        let ok = wm::SetForegroundWindow(h) != 0;
        if attached_target {
            AttachThreadInput(current, target_thread, 0);
        }
        if attached_foreground {
            AttachThreadInput(current, foreground_thread, 0);
        }
        // SetForegroundWindow can report success without moving the foreground (seen with a minimized target while
        // another app is in front): only a verified result counts, otherwise switch the way the taskbar does.
        if ok && is_foreground_root(handle) {
            return true;
        }
        wm::SwitchToThisWindow(h, 1);
    }
    is_foreground_root(handle)
}

fn is_foreground_root(handle: Hwnd) -> bool {
    let active = win::foreground();
    active == handle || (active != 0 && normalize_root(active) == normalize_root(handle))
}

/// The foreground application window (root owner), `Some(0)` for an empty foreground, `None` when the
/// foreground belongs to this process.
pub fn foreground_application_window() -> Option<Hwnd> {
    let foreground = win::foreground();
    if foreground == 0 {
        return Some(0);
    }
    if win::process_id(foreground) == std::process::id() {
        return None;
    }
    Some(normalize_root(foreground))
}

/// Minimizes the window when it (or another window of the same executable) is already in front, else activates.
pub fn toggle_activation(
    handle: Hwnd,
    executable_path: Option<&str>,
    foreground_at_press: Option<Hwnd>,
) -> bool {
    if !win::is_window(handle) {
        return false;
    }
    let foreground = foreground_at_press
        .or_else(foreground_application_window)
        .unwrap_or(0);
    if foreground != 0 {
        let root = normalize_root(foreground);
        let same_app = root == handle
            || executable_path.is_some_and(|path| {
                executable_path_of_window(root).is_some_and(|p| p.eq_ignore_ascii_case(path))
            });
        if win::is_window(root) && !win::is_iconic(root) && same_app {
            // SAFETY: plain Win32 call.
            return unsafe { wm::ShowWindowAsync(win::hwnd(root), wm::SW_MINIMIZE) } != 0;
        }
    }
    activate(handle)
}

fn executable_path_of_window(handle: Hwnd) -> Option<String> {
    executable_path(win::process_id(handle))
}

/// Asks a window to close (WM_CLOSE), like the taskbar's Close window command.
pub fn request_close(handle: Hwnd) -> bool {
    // SAFETY: posting WM_CLOSE to a validated window.
    win::is_window(handle)
        && unsafe { wm::PostMessageW(win::hwnd(handle), wm::WM_CLOSE, 0, 0) } != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_button_rules_match_csharp() {
        assert!(has_taskbar_button(0, false, false));
        assert!(
            !has_taskbar_button(0, true, false),
            "owned windows need WS_EX_APPWINDOW"
        );
        assert!(has_taskbar_button(WS_EX_APPWINDOW, true, false));
        assert!(!has_taskbar_button(WS_EX_TOOLWINDOW, false, false));
        assert!(has_taskbar_button(
            WS_EX_TOOLWINDOW | WS_EX_APPWINDOW,
            false,
            false
        ));
        assert!(!has_taskbar_button(WS_EX_NOACTIVATE, false, false));
        assert!(
            !has_taskbar_button(WS_EX_APPWINDOW, false, true),
            "DeleteTab wins"
        );
    }

    #[test]
    fn cloak_and_immersive_rules() {
        assert!(!is_hidden_cloak(0));
        assert!(!is_hidden_cloak(DWM_CLOAKED_SHELL), "other virtual desktop");
        assert!(is_hidden_cloak(0x1));
        assert!(is_hidden_cloak(0x4 | DWM_CLOAKED_SHELL));
        assert!(is_hidden_immersive_frame("ApplicationFrameWindow", 0));
        assert!(!is_hidden_immersive_frame(
            "ApplicationFrameWindow",
            WS_EX_WINDOWEDGE
        ));
        assert!(!is_hidden_immersive_frame("Chrome_WidgetWin_1", 0));
    }
}
