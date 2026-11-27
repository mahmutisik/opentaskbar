//! The wizard window: a borderless Win32 window with Mica and rounded corners, filled by a transparent WebView2 page.
//!
//! JS sends JSON messages with `window.ipc.postMessage`; they are queued and handled from the window procedure (never
//! inside WebView2's callback), and answers go back through `window.ot.receive(...)`. The dock runs on the same thread
//! once the install step started it, so every later choice is applied to it immediately.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::num::NonZeroIsize;
use std::path::PathBuf;

use ot_core::language::{AppLanguage, Catalog};
use ot_core::log;
use ot_core::settings::DockSettings;
use ot_core::store::{SettingsStore, app_data_dir};
use ot_dock::theme::{Tone, tone};
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

/// Settings written by a completed wizard carry this version; a newer wizard can ask again for new choices.
pub const ONBOARDING_VERSION: u32 = 1;

const CLASS_NAME: &str = "OpenTaskBarRustWizard";
const WIDTH_DIP: i32 = 820;
const HEIGHT_DIP: i32 = 560;
const WM_IPC: u32 = wm::WM_APP + 1;
const TIMER_SHOW: usize = 1;

// DWM attributes (Windows 11).
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMWCP_ROUND: i32 = 2;
const DWMSBT_MAINWINDOW: i32 = 2;

const HTML: &str = include_str!("../../../ui/wizard/index.html");
const CSS: &str = include_str!("../../../ui/wizard/wizard.css");
const STRINGS: &str = include_str!("../../../ui/wizard/strings.js");
const SCRIPT: &str = include_str!("../../../ui/wizard/wizard.js");

/// Widgets offered on the widget screen, in catalog order (AI usage widgets need accounts; they stay in Settings).
const OFFERED_WIDGETS: &[&str] = &[
    "Clock",
    "Weather",
    "Media",
    "SystemStats",
    "NetSpeed",
    "Battery",
    "BluetoothBattery",
    "Storage",
    "Audio",
    "QuickSettings",
    "Focus",
    "DayProgress",
    "Stopwatch",
    "Countdown",
];

pub struct WizardOptions {
    pub store: SettingsStore,
    /// Starting point: defaults on first run, the current settings when run again.
    pub settings: DockSettings,
    /// Development: no copy, no startup entry, nothing saved.
    pub dry_run: bool,
    /// Screenshot tests: the page walks through every screen by itself.
    pub demo: bool,
}

pub enum WizardOutcome {
    /// Setup finished after the install step: the dock runs on this thread; the caller runs the message loop.
    DockRunning(DockSettings),
    /// Setup skipped (or finished without installing): settings saved, the caller starts the dock.
    Skipped(DockSettings),
    /// Closed before the install step: nothing saved, the wizard runs again next time.
    Cancelled,
}

struct State {
    hwnd: HWND,
    webview: Option<WebView>,
    _context: WebContext,
    store: SettingsStore,
    settings: DockSettings,
    dry_run: bool,
    demo: bool,
    installed: bool,
    shown: bool,
    outcome: Option<WizardOutcome>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static INBOX: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
    /// Set while a message is handled: install pumps messages (Explorer, WebView2), which must not re-enter.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|slot| slot.try_borrow_mut().ok()?.as_mut().map(f))
}

struct Parent(HWND);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(handle));
        // SAFETY: the window outlives the WebView (it is destroyed only after the WebView is dropped).
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Shows the wizard and runs its message loop until it ends.
pub fn run(options: WizardOptions) -> WizardOutcome {
    // SAFETY: WebView2 needs a single-threaded apartment on this thread (repeat calls are harmless).
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let Some(hwnd) = create_window() else {
        log::error("Wizard window could not be created.");
        return WizardOutcome::Cancelled;
    };
    set_dark(hwnd, tone(options.settings.theme) == Tone::Dark);

    let mut context = WebContext::new(Some(app_data_dir().join("WebView2")));
    let target = hwnd as isize;
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(page())
        .with_transparent(true)
        .with_devtools(cfg!(debug_assertions))
        .with_default_context_menus(cfg!(debug_assertions))
        .with_browser_accelerator_keys(false)
        .with_additional_browser_args(browser_args())
        .with_ipc_handler(move |request| {
            INBOX.with(|inbox| inbox.borrow_mut().push_back(request.into_body()));
            // SAFETY: posting to our own window.
            unsafe { wm::PostMessageW(target as HWND, WM_IPC, 0, 0) };
        })
        .build(&Parent(hwnd));
    let webview = match webview {
        Ok(webview) => webview,
        Err(error) => {
            log::error(format!("Wizard WebView2 failed: {error}"));
            // SAFETY: our own window.
            unsafe { wm::DestroyWindow(hwnd) };
            return WizardOutcome::Cancelled;
        }
    };
    log::info(format!(
        "Wizard shown{}{}.",
        if options.dry_run { " (dry run)" } else { "" },
        if options.demo { " (demo)" } else { "" }
    ));
    STATE.with(|slot| {
        *slot.borrow_mut() = Some(State {
            hwnd,
            webview: Some(webview),
            _context: context,
            store: options.store,
            settings: options.settings,
            dry_run: options.dry_run,
            demo: options.demo,
            installed: false,
            shown: false,
            outcome: None,
        })
    });
    // The page shows the window once it has painted; this is the fallback.
    // SAFETY: timer on our own window.
    unsafe { wm::SetTimer(hwnd, TIMER_SHOW, 4000, None) };

    let mut msg: wm::MSG = unsafe { std::mem::zeroed() };
    loop {
        if with_state(|s| s.outcome.is_some()).unwrap_or(true) {
            break;
        }
        // SAFETY: standard message loop; the dock (once started) shares it.
        let got = unsafe { wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            // WM_QUIT: end the wizard like the close button.
            close_requested();
            if got == 0 {
                // SAFETY: put WM_QUIT back for the caller's loop.
                unsafe { wm::PostQuitMessage(msg.wParam as i32) };
            }
            break;
        }
        unsafe {
            wm::TranslateMessage(&msg);
            wm::DispatchMessageW(&msg);
        }
    }

    let state = STATE.with(|slot| slot.borrow_mut().take());
    INBOX.with(|inbox| inbox.borrow_mut().clear());
    let Some(mut state) = state else {
        return WizardOutcome::Cancelled;
    };
    state.webview.take();
    // SAFETY: our own window; the WebView is gone.
    unsafe { wm::DestroyWindow(state.hwnd) };
    state.outcome.unwrap_or(WizardOutcome::Cancelled)
}

/// WebView2 browser arguments: wry's defaults plus memory savers. `OT_WIZARD_BROWSER_ARGS` replaces them (tests).
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

fn browser_args() -> String {
    std::env::var("OT_WIZARD_BROWSER_ARGS").unwrap_or_else(|_| BROWSER_ARGS.to_owned())
}

/// The page with its stylesheet and scripts inlined. `OT_WIZARD_DIR` loads them from disk instead (development).
fn page() -> String {
    let read = |name: &str, embedded: &'static str| -> String {
        std::env::var_os("OT_WIZARD_DIR")
            .and_then(|dir| std::fs::read_to_string(PathBuf::from(dir).join(name)).ok())
            .unwrap_or_else(|| embedded.to_owned())
    };
    read("index.html", HTML)
        .replace("/*@CSS*/", &read("wizard.css", CSS))
        .replace("/*@STRINGS*/", &read("strings.js", STRINGS))
        .replace("/*@SCRIPT*/", &read("wizard.js", SCRIPT))
}

fn create_window() -> Option<HWND> {
    let class = wide(CLASS_NAME);
    let title = wide("OpenTaskBar");
    // SAFETY: registering a class and creating a top-level window owned by this thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let mut wc: wm::WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = size_of::<wm::WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        wc.hCursor = wm::LoadCursorW(std::ptr::null_mut(), wm::IDC_ARROW);
        wc.hIcon = wm::LoadIconW(instance, 1 as _);
        // Black is transparent where the frame is extended: Mica shows through the transparent page.
        wc.hbrBackground = GetStockObject(BLACK_BRUSH) as HBRUSH;
        wm::RegisterClassExW(&wc);
        let hwnd = wm::CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            // No caption: with the frame extended over the window DWM would draw its own caption buttons.
            // The thick frame keeps the shadow, rounded corners and Mica; hit testing below disables resizing.
            wm::WS_POPUP | wm::WS_THICKFRAME | wm::WS_MINIMIZEBOX | wm::WS_SYSMENU,
            0,
            0,
            WIDTH_DIP,
            HEIGHT_DIP,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
        set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
        set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW);

        // Size for this monitor's DPI and center on its work area.
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let width = WIDTH_DIP * dpi / 96;
        let height = HEIGHT_DIP * dpi / 96;
        let mut work: RECT = std::mem::zeroed();
        wm::SystemParametersInfoW(wm::SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        // SWP_FRAMECHANGED applies WM_NCCALCSIZE: the whole window becomes client area.
        wm::SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            width,
            height,
            wm::SWP_NOZORDER | wm::SWP_NOACTIVATE | wm::SWP_FRAMECHANGED,
        );
        Some(hwnd)
    }
}

fn set_attribute(hwnd: HWND, attribute: u32, value: i32) {
    // SAFETY: a 4-byte attribute on our own window; unsupported attributes (older Windows) just fail.
    unsafe { DwmSetWindowAttribute(hwnd, attribute as _, (&value as *const i32).cast(), 4) };
}

fn set_dark(hwnd: HWND, dark: bool) {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(dark));
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        wm::WM_NCCALCSIZE if wparam != 0 => 0,
        wm::WM_NCHITTEST => wm::HTCLIENT as LRESULT,
        WM_IPC => {
            drain_inbox();
            0
        }
        wm::WM_TIMER if wparam == TIMER_SHOW => {
            show_window();
            0
        }
        wm::WM_CLOSE => {
            close_requested();
            // SAFETY: wakes the wizard loop so it sees the outcome.
            unsafe { wm::PostMessageW(hwnd, wm::WM_NULL, 0, 0) };
            0
        }
        wm::WM_DPICHANGED => {
            // SAFETY: lparam points at the suggested rectangle for the new DPI.
            let rect = unsafe { &*(lparam as *const RECT) };
            unsafe {
                wm::SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    wm::SWP_NOZORDER | wm::SWP_NOACTIVATE,
                )
            };
            0
        }
        wm::WM_SETTINGCHANGE => {
            // Windows switched light/dark: matters when the theme follows Windows.
            if with_state(|s| s.settings.theme == ot_core::settings::AppTheme::System)
                .unwrap_or(false)
            {
                apply_theme();
            }
            // SAFETY: default handling.
            unsafe { wm::DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { wm::DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn drain_inbox() {
    if BUSY.with(Cell::get) {
        return; // the outer handler drains the rest when it is done
    }
    while let Some(message) = INBOX.with(|inbox| inbox.borrow_mut().pop_front()) {
        let Ok(value) = serde_json::from_str::<Value>(&message) else {
            log::warn(format!("Wizard: unreadable message {message:?}"));
            continue;
        };
        let kind = value["type"].as_str().unwrap_or_default();
        if kind == "drag" {
            // The move loop is modal and pumps messages: run it outside the busy section.
            drag();
            continue;
        }
        BUSY.with(|busy| busy.set(true));
        handle(kind, &value);
        BUSY.with(|busy| busy.set(false));
    }
}

fn handle(kind: &str, message: &Value) {
    match kind {
        "ready" => {
            send("init", init_payload());
            show_window();
        }
        "set" => {
            if let Some(patch) = message["patch"].as_object() {
                apply_patch(patch);
            }
        }
        "import" => import_settings(),
        "install" => install(message["startWithWindows"].as_bool().unwrap_or(true)),
        "finish" => finish(false),
        "skip" => finish(true),
        "minimize" => {
            if let Some(hwnd) = with_state(|s| s.hwnd) {
                // SAFETY: our own window.
                unsafe { wm::ShowWindow(hwnd, wm::SW_MINIMIZE) };
            }
        }
        "close" => close_requested(),
        "log" => log::info(format!(
            "Wizard page: {}",
            message["text"].as_str().unwrap_or_default()
        )),
        other => log::warn(format!("Wizard: unknown message {other:?}")),
    }
}

/// Sends `{ type, data }` to the page.
fn send(kind: &str, data: Value) {
    let script = format!(
        "window.ot && window.ot.receive({})",
        json!({ "type": kind, "data": data })
    );
    with_state(|s| {
        if let Some(webview) = &s.webview
            && let Err(error) = webview.evaluate_script(&script)
        {
            log::warn(format!("Wizard: message {kind} not delivered: {error}"));
        }
    });
}

fn show_window() {
    let Some((hwnd, first)) = with_state(|s| (s.hwnd, !std::mem::replace(&mut s.shown, true)))
    else {
        return;
    };
    if first {
        // SAFETY: our own window.
        unsafe {
            wm::KillTimer(hwnd, TIMER_SHOW);
            // Not ShowWindow: its first call follows the launcher's STARTUPINFO (a hidden launch would hide us).
            wm::SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                wm::SWP_NOMOVE | wm::SWP_NOSIZE | wm::SWP_NOZORDER | wm::SWP_SHOWWINDOW,
            );
            wm::SetForegroundWindow(hwnd);
        }
        ot_core::startup::mark("wizard visible");
    }
}

fn drag() {
    let Some(hwnd) = with_state(|s| s.hwnd) else {
        return;
    };
    // SAFETY: the standard caption drag: release the page's capture and start the move loop on our window.
    unsafe {
        ReleaseCapture();
        wm::SendMessageW(hwnd, wm::WM_NCLBUTTONDOWN, wm::HTCAPTION as WPARAM, 0);
    }
}

fn settings_value(settings: &DockSettings) -> Value {
    serde_json::to_value(settings).unwrap_or(Value::Null)
}

fn widget_list(language: &AppLanguage) -> Value {
    OFFERED_WIDGETS
        .iter()
        .map(|id| {
            let name_key = format!("Widget{id}");
            let description_key = format!("Widget{id}Description");
            json!({
                "id": id,
                "name": Catalog::try_get(&name_key, language).unwrap_or(id),
                "description": Catalog::try_get(&description_key, language).unwrap_or_default(),
            })
        })
        .collect()
}

fn csharp_settings_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(r"OpenTaskbar\settings.json")
}

fn init_payload() -> Value {
    with_state(|s| {
        json!({
            "settings": settings_value(&s.settings),
            "widgets": widget_list(&s.settings.language),
            "languages": AppLanguage::bundled().map(|l| l.culture_name().to_owned()).collect::<Vec<_>>(),
            "systemTheme": if tone(ot_core::settings::AppTheme::System) == Tone::Dark { "dark" } else { "light" },
            "canImport": csharp_settings_path().exists(),
            "dryRun": s.dry_run,
            "demo": s.demo,
            "installed": s.installed,
            "version": env!("CARGO_PKG_VERSION"),
        })
    })
    .unwrap_or(Value::Null)
}

/// Merges `{ "PascalCaseKey": value }` into the settings, normalized the way a settings file is read.
fn apply_patch(patch: &Map<String, Value>) {
    let Some((previous, mut value, store)) = with_state(|s| {
        let store = SettingsStore::new(s.store.path().to_path_buf(), s.settings.language.clone());
        (s.settings.clone(), settings_value(&s.settings), store)
    }) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for (key, item) in patch {
        object.insert(key.clone(), item.clone());
    }
    let settings = match store.parse(&value.to_string()) {
        Ok(settings) => settings,
        Err(error) => {
            log::warn(format!(
                "Wizard: rejected change {patch:?}: {}",
                error.key()
            ));
            return;
        }
    };
    let language_changed = settings.language != previous.language;
    let theme_changed = settings.theme != previous.theme;
    with_state(|s| s.settings = settings.clone());
    if theme_changed {
        apply_theme();
    }
    if language_changed {
        send("widgets", widget_list(&settings.language));
    }
    if ot_dock::is_running() && dock_visible_change(&previous, &settings) {
        ot_dock::apply_settings(settings);
    }
}

/// Only changes the dock shows are worth a restyle (language, widget picks and startup are saved at the end).
fn dock_visible_change(a: &DockSettings, b: &DockSettings) -> bool {
    a.theme != b.theme
        || a.dock_background != b.dock_background
        || a.dock_opacity != b.dock_opacity
        || a.dock_corner_radius != b.dock_corner_radius
        || a.size != b.size
        || a.edge != b.edge
        || a.layout != b.layout
        || a.content_alignment != b.content_alignment
        || a.show_search_button != b.show_search_button
        || a.show_task_view_button != b.show_task_view_button
        || a.show_desktop_button != b.show_desktop_button
        || a.show_recycle_bin != b.show_recycle_bin
        || a.auto_hide != b.auto_hide
}

fn apply_theme() {
    let Some((hwnd, theme)) = with_state(|s| (s.hwnd, s.settings.theme)) else {
        return;
    };
    let dark = tone(theme) == Tone::Dark;
    set_dark(hwnd, dark);
    send(
        "systemTheme",
        json!(if tone(ot_core::settings::AppTheme::System) == Tone::Dark {
            "dark"
        } else {
            "light"
        }),
    );
}

/// Takes the look and choices from an OpenTaskbar (C#) installation, read-only.
fn import_settings() {
    let path = csharp_settings_path();
    let Some((language, onboarding)) =
        with_state(|s| (s.settings.language.clone(), s.settings.onboarding_version))
    else {
        return;
    };
    let parsed = std::fs::read_to_string(&path)
        .map_err(|_| "ErrorSettingsRead")
        .and_then(|text| {
            SettingsStore::new(path.clone(), language)
                .parse(&text)
                .map_err(|e| e.key())
        });
    match parsed {
        Ok(mut settings) => {
            settings.onboarding_version = onboarding;
            log::info(format!("Wizard: imported settings from {}", path.display()));
            with_state(|s| s.settings = settings.clone());
            apply_theme();
            send("settings", settings_value(&settings));
            send("widgets", widget_list(&settings.language));
            if ot_dock::is_running() {
                ot_dock::apply_settings(settings);
            }
        }
        Err(key) => {
            log::warn(format!("Wizard: import failed: {key}"));
            send("imported", json!({ "ok": false, "error": key }));
        }
    }
}

/// Copies the program, registers startup and shows the dock (hiding Explorer's taskbar).
fn install(start_with_windows: bool) {
    let Some((dry_run, settings, hwnd)) = with_state(|s| (s.dry_run, s.settings.clone(), s.hwnd))
    else {
        return;
    };
    let language = settings.language.clone();
    let mut result = crate::install::install(dry_run).and_then(|exe| {
        crate::install::set_start_with_windows(&exe, start_with_windows, dry_run)
            .map(|()| exe)
            .map_err(|key| Catalog::get(&key, &language).into_owned())
    });
    if result.is_ok() {
        // No state borrow is held here: starting the dock pumps messages for ~1.5 s while Explorer moves windows.
        if let Err(key) = ot_dock::start(settings) {
            result = Err(Catalog::get(key, &language).into_owned());
        }
    }
    match &result {
        Ok(exe) => {
            log::info(format!("Wizard: dock installed ({}).", exe.display()));
            with_state(|s| s.installed = true);
            // Keep the wizard in front of the new dock.
            // SAFETY: our own window.
            unsafe { wm::SetForegroundWindow(hwnd) };
        }
        Err(message) => log::error(format!("Wizard: install failed: {message}")),
    }
    send(
        "installed",
        match result {
            Ok(_) => json!({ "ok": true }),
            Err(message) => json!({ "ok": false, "error": message }),
        },
    );
}

/// Saves the settings with the onboarding version and ends the wizard.
fn finish(skipped: bool) {
    let Some((mut settings, dry_run, installed, path, language)) = with_state(|s| {
        (
            s.settings.clone(),
            s.dry_run,
            s.installed,
            s.store.path().to_path_buf(),
            s.settings.language.clone(),
        )
    }) else {
        return;
    };
    settings.onboarding_version = ONBOARDING_VERSION;
    if dry_run {
        log::info(format!(
            "Wizard (dry run): would save {}",
            serde_json::to_string(&settings).unwrap_or_default()
        ));
    } else {
        match SettingsStore::new(path, language).save(&settings) {
            Ok(saved) => settings = saved,
            Err(error) => log::report_error(error.key(), &settings.language, None),
        }
    }
    log::info(if skipped {
        "Wizard skipped."
    } else {
        "Wizard finished."
    });
    // The dock the install step started keeps its own copy of the settings and saves it on later changes: it must
    // carry the completed setup too, or the next start would run the wizard again.
    if installed && ot_dock::is_running() {
        ot_dock::set_onboarding_version(settings.onboarding_version);
    }
    let outcome = if installed && ot_dock::is_running() {
        WizardOutcome::DockRunning(settings)
    } else {
        WizardOutcome::Skipped(settings)
    };
    with_state(|s| {
        s.outcome = Some(outcome);
        // SAFETY: wakes the wizard loop.
        unsafe { wm::PostMessageW(s.hwnd, wm::WM_NULL, 0, 0) };
    });
}

/// The close button or Alt+F4: before the install step nothing is kept; after it the dock stays with the choices
/// made so far.
fn close_requested() {
    let installed = with_state(|s| s.installed).unwrap_or(false);
    if installed {
        finish(false);
    } else {
        log::info("Wizard closed before setup.");
        with_state(|s| s.outcome = Some(WizardOutcome::Cancelled));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_inlines_assets() {
        let html = page();
        assert!(!html.contains("/*@CSS*/"));
        assert!(!html.contains("/*@SCRIPT*/"));
        assert!(!html.contains("/*@STRINGS*/"));
    }

    #[test]
    fn offered_widgets_are_supported() {
        for id in OFFERED_WIDGETS {
            assert!(ot_core::widgets::is_supported(id), "{id}");
        }
    }
}
