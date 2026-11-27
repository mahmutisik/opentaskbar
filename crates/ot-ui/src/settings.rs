//! The Settings window (OpenTaskbar's `SettingsWindow`): a resizable borderless window with Desktop Acrylic
//! (`DWMSBT_TRANSIENTWINDOW`), rounded corners and no native rim, filled by a transparent WebView2 page that draws the
//! WPF window's chrome and pages (`ui/settings`). It runs on the dock's thread and message loop; every edit is applied
//! to the dock at once and saved (`PreferenceEdited` / `SaveSettings`; sliders debounce in the page).
//!
//! The WebView2 processes exist only while the window is open: closing it drops the WebView.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::num::NonZeroIsize;
use std::path::PathBuf;

use ot_core::hotkey::HotkeyBinding;
use ot_core::language::{AppLanguage, Catalog};
use ot_core::log;
use ot_core::settings::{DockSettings, HotkeyChoice, SurfaceBackground};
use ot_core::store::{SettingsStore, app_data_dir};
use ot_core::theme_pack::{self, ThemePack};
use ot_dock::theme::{Tone, tone, windows_accent};
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, EnumDisplayMonitors, GetMonitorInfoW, GetStockObject, HBRUSH, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

const CLASS_NAME: &str = "OpenTaskBarRustSettings";
/// `Width="1020" Height="720" MinWidth="580" MinHeight="480" MaxWidth="1360" MaxHeight="960"`.
const WIDTH_DIP: i32 = 1020;
const HEIGHT_DIP: i32 = 720;
const MIN_DIP: (i32, i32) = (580, 480);
const MAX_DIP: (i32, i32) = (1360, 960);
/// `ResizeBorderThickness="6"`.
const RESIZE_DIP: i32 = 6;
const WM_IPC: u32 = wm::WM_APP + 1;
const TIMER_SHOW: usize = 1;

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMWCP_ROUND: i32 = 2;
const DWMWA_COLOR_NONE: i32 = 0xFFFF_FFFEu32 as i32;
const DWMSBT_NONE: i32 = 1;
const DWMSBT_TRANSIENTWINDOW: i32 = 3;

const HTML: &str = include_str!("../../../ui/settings/index.html");
const WIDGET_ICONS: &str = include_str!("../../../ui/settings/widget-icons.json");
const CSS: &str = include_str!("../../../ui/settings/settings.css");
const SCRIPT: &str = include_str!("../../../ui/settings/settings.js");

struct State {
    hwnd: HWND,
    webview: Option<WebView>,
    _context: WebContext,
    settings: DockSettings,
    shown: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static INBOX: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
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

/// `OpenSettings_Click`: shows the window, or brings the open one forward.
pub fn open() {
    if let Some(hwnd) = with_state(|s| s.hwnd) {
        // SAFETY: our own window.
        unsafe {
            if wm::IsIconic(hwnd) != 0 {
                wm::ShowWindow(hwnd, wm::SW_RESTORE);
            }
            wm::SetForegroundWindow(hwnd);
        }
        return;
    }
    let Some(settings) = ot_dock::current_settings() else {
        log::warn("Settings: the dock is not running.");
        return;
    };
    log::info("Settings open requested.");
    let Some(hwnd) = create_window() else {
        log::error("Settings window could not be created.");
        return;
    };
    apply_backdrop(hwnd, &settings);

    let mut context = WebContext::new(Some(app_data_dir().join("WebView2")));
    let target = hwnd as isize;
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(page())
        .with_transparent(true)
        .with_devtools(cfg!(debug_assertions))
        .with_default_context_menus(cfg!(debug_assertions))
        .with_browser_accelerator_keys(false)
        .with_additional_browser_args(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection",
        )
        // The page's Inter faces (the same files the dock and OpenTaskbar use), as `http://ot.localhost/<file>`.
        .with_custom_protocol("ot".into(), |_id, request| {
            font_response(request.uri().path())
        })
        .with_ipc_handler(move |request| {
            INBOX.with(|inbox| inbox.borrow_mut().push_back(request.into_body()));
            // SAFETY: posting to our own window.
            unsafe { wm::PostMessageW(target as HWND, WM_IPC, 0, 0) };
        })
        .build(&Parent(hwnd));
    let webview = match webview {
        Ok(webview) => webview,
        Err(error) => {
            log::error(format!("Settings WebView2 failed: {error}"));
            // SAFETY: our own window.
            unsafe { wm::DestroyWindow(hwnd) };
            return;
        }
    };
    STATE.with(|slot| {
        *slot.borrow_mut() = Some(State {
            hwnd,
            webview: Some(webview),
            _context: context,
            settings,
            shown: false,
        })
    });
    // The page shows the window once it has painted; this is the fallback.
    // SAFETY: timer on our own window.
    unsafe { wm::SetTimer(hwnd, TIMER_SHOW, 4000, None) };
}

const FONTS: &[(&str, &[u8])] = &[
    (
        "/Inter-Regular.ttf",
        include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
    ),
    (
        "/Inter-Medium.ttf",
        include_bytes!("../../../assets/fonts/Inter-Medium.ttf"),
    ),
    (
        "/Inter-SemiBold.ttf",
        include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"),
    ),
    (
        "/Inter-Bold.ttf",
        include_bytes!("../../../assets/fonts/Inter-Bold.ttf"),
    ),
];

/// `Services/Localization/Flags/<culture>.png` (`CountryFlag`), as `http://ot.localhost/flags/<culture>.png`.
const FLAGS: &[(&str, &[u8])] = &[
    ("de-DE", include_bytes!("../../../assets/flags/de-DE.png")),
    ("en-US", include_bytes!("../../../assets/flags/en-US.png")),
    ("es-ES", include_bytes!("../../../assets/flags/es-ES.png")),
    ("fr-FR", include_bytes!("../../../assets/flags/fr-FR.png")),
    ("it-IT", include_bytes!("../../../assets/flags/it-IT.png")),
    ("ja-JP", include_bytes!("../../../assets/flags/ja-JP.png")),
    ("ko-KR", include_bytes!("../../../assets/flags/ko-KR.png")),
    ("nl-NL", include_bytes!("../../../assets/flags/nl-NL.png")),
    ("pl-PL", include_bytes!("../../../assets/flags/pl-PL.png")),
    ("pt-BR", include_bytes!("../../../assets/flags/pt-BR.png")),
    ("ru-RU", include_bytes!("../../../assets/flags/ru-RU.png")),
    ("tr-TR", include_bytes!("../../../assets/flags/tr-TR.png")),
    ("uk-UA", include_bytes!("../../../assets/flags/uk-UA.png")),
    ("zh-CN", include_bytes!("../../../assets/flags/zh-CN.png")),
];

/// `DockWidgetService.CatalogDescriptors` in declaration order: id, label key, category, icon.
const CATALOG: &[(&str, &str, &str, &str)] = &[
    ("Clock", "WidgetClock", "WidgetCategoryTime", "IconClock"),
    (
        "Weather",
        "WidgetWeather",
        "WidgetCategoryTime",
        "IconCloudSun",
    ),
    (
        "DayProgress",
        "WidgetDayProgress",
        "WidgetCategoryTime",
        "IconCalendar",
    ),
    (
        "Stopwatch",
        "WidgetStopwatch",
        "WidgetCategoryTime",
        "IconTimer",
    ),
    (
        "Countdown",
        "WidgetCountdown",
        "WidgetCategoryTime",
        "IconHourglass",
    ),
    (
        "SystemStats",
        "WidgetSystemStats",
        "WidgetCategorySystem",
        "IconCpu",
    ),
    (
        "NetSpeed",
        "WidgetNetSpeed",
        "WidgetCategorySystem",
        "IconWifi",
    ),
    (
        "Battery",
        "WidgetBattery",
        "WidgetCategorySystem",
        "IconBattery",
    ),
    (
        "BluetoothBattery",
        "WidgetBluetoothBattery",
        "WidgetCategorySystem",
        "IconBluetooth",
    ),
    (
        "Storage",
        "WidgetStorage",
        "WidgetCategorySystem",
        "IconHardDrive",
    ),
    (
        "AiUsageClaude",
        "WidgetAiUsageClaude",
        "WidgetCategoryAi",
        "LogoClaude",
    ),
    (
        "AiUsageCodex",
        "WidgetAiUsageCodex",
        "WidgetCategoryAi",
        "LogoCodex",
    ),
    (
        "AiUsageAntigravity",
        "WidgetAiUsageAntigravity",
        "WidgetCategoryAi",
        "LogoAntigravity",
    ),
    (
        "AiUsageOpenCode",
        "WidgetAiUsageOpenCode",
        "WidgetCategoryAi",
        "LogoOpenCode",
    ),
    (
        "AiUsageCopilot",
        "WidgetAiUsageCopilot",
        "WidgetCategoryAi",
        "LogoCopilot",
    ),
    (
        "AiUsageDeepSeek",
        "WidgetAiUsageDeepSeek",
        "WidgetCategoryAi",
        "LogoDeepSeek",
    ),
    (
        "AiUsageOpenRouter",
        "WidgetAiUsageOpenRouter",
        "WidgetCategoryAi",
        "LogoOpenRouter",
    ),
    (
        "AiUsageCursor",
        "WidgetAiUsageCursor",
        "WidgetCategoryAi",
        "LogoCursor",
    ),
    ("Media", "WidgetMedia", "WidgetCategoryMedia", "IconMusic"),
    ("Audio", "WidgetAudio", "WidgetCategoryMedia", "IconVolume"),
    (
        "QuickSettings",
        "WidgetQuickSettings",
        "WidgetCategoryMedia",
        "IconSliders",
    ),
    (
        "Focus",
        "WidgetFocus",
        "WidgetCategoryProductivity",
        "IconTarget",
    ),
];

/// The catalog for the page: a widget is available when it is ported and its hardware exists (a battery).
fn catalog() -> Value {
    let battery = ot_dock::widgets::battery::present();
    CATALOG
        .iter()
        .map(|&(id, label, category, icon)| {
            let ported = ot_dock::widgets::is_supported(id);
            let hardware = id != "Battery" || battery;
            json!({ "id": id, "label": label, "category": category, "icon": icon,
                    "ported": ported, "available": ported && hardware })
        })
        .collect()
}

fn font_response(path: &str) -> wry::http::Response<std::borrow::Cow<'static, [u8]>> {
    // `http://ot.localhost/preview/<id>.png?<n>`: the widget card drawn by the dock.
    if let Some(id) = path
        .strip_prefix("/preview/")
        .and_then(|f| f.strip_suffix(".png"))
    {
        let scale = with_state(|s| dpi_of(s.hwnd) as f32 / 96.0).unwrap_or(1.0);
        let png = CATALOG
            .iter()
            .find(|(known, ..)| *known == id)
            .and_then(|(known, ..)| ot_dock::widget_preview_png(known, scale));
        let builder = wry::http::Response::builder().header("Access-Control-Allow-Origin", "*");
        return match png {
            Some(bytes) => builder
                .status(200)
                .header("Content-Type", "image/png")
                .header("Cache-Control", "no-store")
                .body(std::borrow::Cow::Owned(bytes)),
            None => builder
                .status(404)
                .body(std::borrow::Cow::Borrowed(&[][..])),
        }
        .unwrap_or_else(|_| wry::http::Response::new(std::borrow::Cow::Borrowed(&[][..])));
    }
    if let Some(culture) = path
        .strip_prefix("/flags/")
        .and_then(|f| f.strip_suffix(".png"))
        && let Some((_, bytes)) = FLAGS.iter().find(|(c, _)| *c == culture)
    {
        return wry::http::Response::builder()
            .status(200)
            .header("Content-Type", "image/png")
            .header("Access-Control-Allow-Origin", "*")
            .body(std::borrow::Cow::Borrowed(*bytes))
            .unwrap_or_else(|_| wry::http::Response::new(std::borrow::Cow::Borrowed(&[][..])));
    }
    let font = FONTS.iter().find(|(name, _)| *name == path);
    let builder = wry::http::Response::builder()
        // The page itself has no origin of this host: fonts need CORS.
        .header("Access-Control-Allow-Origin", "*");
    match font {
        Some((_, bytes)) => builder
            .status(200)
            .header("Content-Type", "font/ttf")
            .body(std::borrow::Cow::Borrowed(*bytes)),
        None => builder
            .status(404)
            .body(std::borrow::Cow::Borrowed(&[][..])),
    }
    .unwrap_or_else(|_| wry::http::Response::new(std::borrow::Cow::Borrowed(&[][..])))
}

/// The page with its stylesheet and script inlined. `OT_SETTINGS_DIR` loads them from disk (development).
fn page() -> String {
    let read = |name: &str, embedded: &'static str| -> String {
        std::env::var_os("OT_SETTINGS_DIR")
            .and_then(|dir| std::fs::read_to_string(PathBuf::from(dir).join(name)).ok())
            .unwrap_or_else(|| embedded.to_owned())
    };
    read("index.html", HTML)
        .replace("/*@CSS*/", &read("settings.css", CSS))
        .replace("/*@SCRIPT*/", &read("settings.js", SCRIPT))
        .replace("/*@ICONS*/", &read("widget-icons.json", WIDGET_ICONS))
}

fn create_window() -> Option<HWND> {
    let class = wide(CLASS_NAME);
    let title = wide(&Catalog::get("SettingsTitle", &AppLanguage::english()));
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
        // Black is transparent where the frame is extended: the acrylic shows through the transparent page.
        wc.hbrBackground = GetStockObject(BLACK_BRUSH) as HBRUSH;
        wm::RegisterClassExW(&wc);
        let hwnd = wm::CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            wm::WS_POPUP
                | wm::WS_THICKFRAME
                | wm::WS_MINIMIZEBOX
                | wm::WS_MAXIMIZEBOX
                | wm::WS_SYSMENU,
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
        // `WindowStartupLocation="CenterScreen"`: the primary work area, sized for its DPI.
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let (width, height) = (WIDTH_DIP * dpi / 96, HEIGHT_DIP * dpi / 96);
        let mut work: RECT = std::mem::zeroed();
        wm::SystemParametersInfoW(wm::SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        let width = width.min(work.right - work.left);
        let height = height.min(work.bottom - work.top);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
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

fn set_attribute(hwnd: HWND, attribute: u32, value: i32) -> bool {
    // SAFETY: a 4-byte attribute on our own window; unsupported attributes (older Windows) just fail.
    unsafe { DwmSetWindowAttribute(hwnd, attribute as _, (&value as *const i32).cast(), 4) >= 0 }
}

/// `SettingsBackdrop.TryEnable` / `TryEnableTransparent` / `Disable` for the Settings surface style.
fn apply_backdrop(hwnd: HWND, settings: &DockSettings) {
    let dark = tone(settings.theme) == Tone::Dark;
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(dark));
    set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    // The page draws the only rim.
    set_attribute(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
    let backdrop = match settings.settings_background {
        SurfaceBackground::Glass => DWMSBT_TRANSIENTWINDOW,
        _ => DWMSBT_NONE,
    };
    set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, backdrop);
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: our own window.
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
}

fn dpi_of(hwnd: HWND) -> i32 {
    // SAFETY: our own window.
    unsafe { GetDpiForWindow(hwnd).max(96) as i32 }
}

fn is_maximized(hwnd: HWND) -> bool {
    // SAFETY: our own window.
    unsafe { wm::IsZoomed(hwnd) != 0 }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        wm::WM_NCCALCSIZE if wparam != 0 => {
            // The whole window is client area; a maximized window would hang its frame off the screen, so it is
            // trimmed to the monitor's work area.
            if is_maximized(hwnd) {
                // SAFETY: lparam is NCCALCSIZE_PARAMS when wparam is TRUE.
                let params = unsafe { &mut *(lparam as *mut wm::NCCALCSIZE_PARAMS) };
                // SAFETY: plain Win32 calls.
                unsafe {
                    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut info: MONITORINFO = std::mem::zeroed();
                    info.cbSize = size_of::<MONITORINFO>() as u32;
                    if GetMonitorInfoW(monitor, &mut info) != 0 {
                        params.rgrc[0] = info.rcWork;
                    }
                }
            }
            0
        }
        wm::WM_NCHITTEST => {
            if is_maximized(hwnd) {
                return wm::HTCLIENT as LRESULT;
            }
            let point = POINT {
                x: i32::from(lparam as u16 as i16),
                y: i32::from((lparam >> 16) as u16 as i16),
            };
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            // SAFETY: our own window.
            unsafe { wm::GetWindowRect(hwnd, &mut rect) };
            let edge = RESIZE_DIP * dpi_of(hwnd) / 96;
            let left = point.x < rect.left + edge;
            let right = point.x >= rect.right - edge;
            let top = point.y < rect.top + edge;
            let bottom = point.y >= rect.bottom - edge;
            (match (left, right, top, bottom) {
                (true, _, true, _) => wm::HTTOPLEFT,
                (_, true, true, _) => wm::HTTOPRIGHT,
                (true, _, _, true) => wm::HTBOTTOMLEFT,
                (_, true, _, true) => wm::HTBOTTOMRIGHT,
                (true, _, _, _) => wm::HTLEFT,
                (_, true, _, _) => wm::HTRIGHT,
                (_, _, true, _) => wm::HTTOP,
                (_, _, _, true) => wm::HTBOTTOM,
                _ => wm::HTCLIENT,
            }) as LRESULT
        }
        wm::WM_GETMINMAXINFO => {
            // SAFETY: lparam points at the MINMAXINFO to fill.
            let info = unsafe { &mut *(lparam as *mut wm::MINMAXINFO) };
            let dpi = dpi_of(hwnd);
            info.ptMinTrackSize.x = MIN_DIP.0 * dpi / 96;
            info.ptMinTrackSize.y = MIN_DIP.1 * dpi / 96;
            info.ptMaxTrackSize.x = MAX_DIP.0 * dpi / 96;
            info.ptMaxTrackSize.y = MAX_DIP.1 * dpi / 96;
            0
        }
        wm::WM_SIZE => {
            let (w, h) = (i32::from(lparam as u16), i32::from((lparam >> 16) as u16));
            with_state(|s| {
                if let Some(webview) = &s.webview {
                    let _ = webview.set_bounds(wry::Rect {
                        position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                        size: wry::dpi::PhysicalSize::new(w as u32, h as u32).into(),
                    });
                }
            });
            send("maximized", json!(is_maximized(hwnd)));
            0
        }
        WM_IPC => {
            drain_inbox();
            0
        }
        wm::WM_TIMER if wparam == TIMER_SHOW => {
            show_window();
            0
        }
        wm::WM_CLOSE => {
            close();
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
            // Windows switched light/dark or the accent: matters for the System theme and the accent brushes.
            send_theme();
            // SAFETY: default handling.
            unsafe { wm::DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { wm::DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn close() {
    let state = STATE.with(|slot| slot.borrow_mut().take());
    INBOX.with(|inbox| inbox.borrow_mut().clear());
    if let Some(mut state) = state {
        state.webview.take();
        // SAFETY: our own window; the WebView is gone.
        unsafe { wm::DestroyWindow(state.hwnd) };
        log::info("Settings closed.");
    }
}

fn drain_inbox() {
    if BUSY.with(Cell::get) {
        return;
    }
    while let Some(message) = INBOX.with(|inbox| inbox.borrow_mut().pop_front()) {
        let Ok(value) = serde_json::from_str::<Value>(&message) else {
            log::warn(format!("Settings: unreadable message {message:?}"));
            continue;
        };
        let kind = value["type"].as_str().unwrap_or_default().to_owned();
        match kind.as_str() {
            // Modal loops: run outside the busy section.
            "drag" => {
                drag();
                continue;
            }
            "close" => {
                close();
                return;
            }
            "resetSetup" => {
                // Asked first: everything goes back to defaults and the dock closes.
                let text = with_state(|s| {
                    Catalog::get("ResetAndRunSetupConfirm", &s.settings.language).into_owned()
                })
                .unwrap_or_default();
                if confirm(&text) {
                    log::info("Settings: reset and run setup again.");
                    close();
                    ot_dock::restart_into_setup();
                    return;
                }
                continue;
            }
            _ => {}
        }
        BUSY.with(|busy| busy.set(true));
        handle(&kind, &value);
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
        "reset" => {
            if let Some(keys) = message["keys"].as_array() {
                reset(keys);
            }
        }
        "hardware" => set_hardware(message["value"].as_bool().unwrap_or(false)),
        "startup" => set_startup(message["value"].as_bool().unwrap_or(false)),
        "hotkey" => record_hotkey(message),
        "hotkeyDisable" => set_hotkey(message["target"].as_str() == Some("dock"), Hotkey::Disabled),
        "hotkeyReset" => set_hotkey(message["target"].as_str() == Some("dock"), Hotkey::Default),
        "themeSelect" => select_theme(message["id"].as_str().map(str::to_owned)),
        "themeNew" | "themeDuplicate" | "themeCreateEditable" => new_theme(kind, message),
        "themeExport" => export_theme(),
        "themeImport" => import_theme(),
        "themeDelete" => delete_theme(),
        "themeOpenFolder" => {
            let root = theme_pack::root();
            let _ = std::fs::create_dir_all(&root);
            shell_open(&root.to_string_lossy());
        }
        "themeReload" => {
            ot_dock::reload_theme();
            send_themes();
            send_theme();
        }
        "borderColorSet" => set_border_color(message["value"].as_str().unwrap_or_default()),
        "themeToken" | "themeInfo" => edit_theme(kind, message),
        "export" => export_settings(false),
        "fullExport" => export_settings(true),
        "import" => import_settings(false),
        "fullImport" => import_settings(true),
        "openUrl" => {
            // `OpenGithub_Click`: only the project's page, never a URL from the page.
            shell_open("https://github.com/alztrk/opentaskbar");
        }
        "openLog" => {
            // `AppLog.TryOpenLogFile`.
            shell_open(
                &app_data_dir()
                    .join(ot_core::store::LOG_FILE)
                    .to_string_lossy(),
            );
        }
        "minimize" | "maximize" => {
            if let Some(hwnd) = with_state(|s| s.hwnd) {
                let command = match kind {
                    "minimize" => wm::SW_MINIMIZE,
                    _ if is_maximized(hwnd) => wm::SW_RESTORE,
                    _ => wm::SW_MAXIMIZE,
                };
                // SAFETY: our own window.
                unsafe { wm::ShowWindow(hwnd, command) };
            }
        }
        "log" => log::info(format!(
            "Settings page: {}",
            message["text"].as_str().unwrap_or_default()
        )),
        other => log::warn(format!("Settings: unknown message {other:?}")),
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
            log::warn(format!("Settings: message {kind} not delivered: {error}"));
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

fn color_hex((a, r, g, b): (u8, u8, u8, u8)) -> String {
    format!("#{a:02X}{r:02X}{g:02X}{b:02X}")
}

/// `WindowsPalette.ContrastForeground`.
fn contrast_foreground((_, r, g, b): (u8, u8, u8, u8)) -> &'static str {
    let channel = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
    if luminance < 0.18 {
        "#FFFFFFFF"
    } else {
        "#FF05070B"
    }
}

/// The brushes the page needs for the current theme (`WindowsPalette` values when Windows has an accent).
fn theme_payload(settings: &DockSettings) -> Value {
    let dark = tone(settings.theme) == Tone::Dark;
    let accent = windows_accent().map(|c| (c.A, c.R, c.G, c.B));
    let accent_hex = accent
        .map(color_hex)
        .unwrap_or_else(|| (if dark { "#FF38BDF8" } else { "#FF0369A1" }).to_owned());
    let mut accent_text = match accent {
        Some(c) => contrast_foreground(c).to_owned(),
        None => (if dark { "#FF05070B" } else { "#FFFFFFFF" }).to_owned(),
    };
    let token =
        |key: &str| ot_dock::tokens::color_of(key, dark).map(|c| color_hex((c.A, c.R, c.G, c.B)));
    let accent_hex = token("AccentBrush").unwrap_or(accent_hex);
    if let Some(text) = token("AccentText") {
        accent_text = text;
    }
    // A theme pack's page brushes (CSS variable, token); the page keeps its palette for the others.
    let overrides: Map<String, Value> = [
        ("--text", "TextPrimary"),
        ("--text2", "TextSecondary"),
        ("--stroke", "StrokeSubtle"),
        ("--control", "SettingsControl"),
        ("--control-hover", "SettingsControlHover"),
        ("--track", "SettingsTrack"),
        ("--danger-text", "DangerText"),
        ("--selected-strong", "SurfaceSelectedStrong"),
        ("--surface-hover", "SurfaceHover"),
        ("--surface-selected", "SurfaceSelected"),
        ("--popup-solid", "SettingsControl"),
        ("page", "SettingsBackground"),
        ("sidebar", "SettingsSidebarOverlay"),
    ]
    .iter()
    .filter_map(|(var, key)| token(key).map(|v| ((*var).to_owned(), Value::String(v))))
    .collect();
    let focus = ot_dock::theme::stroke_focus(dark).map(|c| color_hex((c.A, c.R, c.G, c.B)));
    json!({
        "dark": dark,
        "focus": focus,
        "overrides": overrides,
        "accent": accent_hex,
        "accentText": accent_text,
        "background": settings.settings_background,
        "opacity": settings.settings_opacity,
        "borderThickness": settings.settings_border_thickness,
        "borderColor": settings.settings_border_color,
    })
}

fn send_theme() {
    let Some((hwnd, settings)) = with_state(|s| (s.hwnd, s.settings.clone())) else {
        return;
    };
    apply_backdrop(hwnd, &settings);
    send("theme", theme_payload(&settings));
}

/// `DisplayService.GetDisplays`: "1: 1920x1080 (Primary)".
fn displays(language: &AppLanguage) -> Value {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        // SAFETY: data is the Vec passed below; the info struct is sized for the EX variant.
        unsafe {
            let list = &mut *(data as *mut Vec<(String, i32, i32, bool)>);
            let mut info: MONITORINFOEXW = std::mem::zeroed();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) != 0 {
                let end = info
                    .szDevice
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(info.szDevice.len());
                let r = info.monitorInfo.rcMonitor;
                list.push((
                    String::from_utf16_lossy(&info.szDevice[..end]),
                    r.right - r.left,
                    r.bottom - r.top,
                    info.monitorInfo.dwFlags & 1 != 0,
                ));
            }
        }
        1
    }
    let mut list: Vec<(String, i32, i32, bool)> = Vec::new();
    // SAFETY: the callback only touches the list for the duration of the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            (&mut list as *mut Vec<(String, i32, i32, bool)>) as LPARAM,
        )
    };
    let primary = Catalog::get("TargetDisplayPrimary", language).into_owned();
    let mut out = vec![json!({ "value": Value::Null, "label": primary })];
    for (i, (device, w, h, is_primary)) in list.into_iter().enumerate() {
        let suffix = if is_primary {
            format!(" ({primary})")
        } else {
            String::new()
        };
        out.push(json!({ "value": device, "label": format!("{}: {w}x{h}{suffix}", i + 1) }));
    }
    Value::Array(out)
}

enum Hotkey {
    Custom(HotkeyBinding),
    Disabled,
    Default,
}

/// The two global hotkeys as the page shows them: their display text (`None`: disabled) and whether the reset button
/// has anything to do. Legacy `WinAltD` reads as Ctrl+Alt+D when no custom binding overrides it (`LoadAllSelections`).
fn hotkeys_payload(settings: &DockSettings) -> Value {
    let dock_choice = match (settings.dock_hotkey, settings.custom_dock_hotkey.as_deref()) {
        (HotkeyChoice::WinAltD, None) => HotkeyChoice::CtrlAltD,
        (choice, _) => choice,
    };
    let dock = HotkeyBinding::resolve(dock_choice, settings.custom_dock_hotkey.as_deref());
    let launcher = HotkeyBinding::resolve(
        settings.launcher_hotkey,
        settings.custom_launcher_hotkey.as_deref(),
    );
    json!({
        "dock": dock.map(|b| b.display()),
        "launcher": launcher.map(|b| b.display()),
        "dockResettable": settings.custom_dock_hotkey.is_some() || dock_choice != HotkeyChoice::CtrlAltD,
        "launcherResettable": settings.custom_launcher_hotkey.is_some() || settings.launcher_hotkey != HotkeyChoice::AltSpace,
    })
}

/// A key pressed while a hotkey is recorded (`Window_PreviewKeyDown`).
fn record_hotkey(message: &Value) {
    let dock = message["target"].as_str() == Some("dock");
    let flag = |k: &str| message[k].as_bool().unwrap_or(false);
    let vk = message["vk"].as_u64().unwrap_or(0).min(255) as u8;
    match HotkeyBinding::from_key_event(vk, flag("ctrl"), flag("alt"), flag("shift"), flag("win")) {
        Ok(binding) => set_hotkey(dock, Hotkey::Custom(binding)),
        Err(Some(key)) => {
            let text = with_state(|s| Catalog::get(key, &s.settings.language).into_owned())
                .unwrap_or_default();
            send(
                "hotkeyError",
                json!({ "text": text, "keepRecording": true }),
            );
        }
        Err(None) => {}
    }
}

/// Sets one hotkey unless the other one already uses the same keys (`HotkeyConflict`), then saves.
fn set_hotkey(dock: bool, hotkey: Hotkey) {
    let Some(previous) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let binding = match &hotkey {
        Hotkey::Custom(b) => Some(*b),
        Hotkey::Disabled => None,
        Hotkey::Default => HotkeyBinding::from_choice(if dock {
            HotkeyChoice::CtrlAltD
        } else {
            HotkeyChoice::AltSpace
        }),
    };
    let other = if dock {
        HotkeyBinding::resolve(
            previous.launcher_hotkey,
            previous.custom_launcher_hotkey.as_deref(),
        )
    } else {
        HotkeyBinding::resolve(previous.dock_hotkey, previous.custom_dock_hotkey.as_deref())
    };
    if ot_core::hotkey::conflicts(binding, other) {
        let other_name = Catalog::get(
            if dock { "LauncherHotkey" } else { "DockHotkey" },
            &previous.language,
        );
        let text = Catalog::get("HotkeyConflict", &previous.language).replace("{0}", &other_name);
        send(
            "hotkeyError",
            json!({ "text": text, "keepRecording": false }),
        );
        return;
    }
    let (choice, custom) = match hotkey {
        Hotkey::Custom(b) => (None, Some(b.display())),
        Hotkey::Disabled => (None, Some("Disabled".to_owned())),
        Hotkey::Default if dock => (Some(HotkeyChoice::CtrlAltD), None),
        Hotkey::Default => (Some(HotkeyChoice::AltSpace), None),
    };
    let mut patch = Map::new();
    let (choice_key, custom_key) = if dock {
        ("DockHotkey", "CustomDockHotkey")
    } else {
        ("LauncherHotkey", "CustomLauncherHotkey")
    };
    if let Some(choice) = choice {
        patch.insert(
            choice_key.to_owned(),
            serde_json::to_value(choice).unwrap_or(Value::Null),
        );
    }
    patch.insert(
        custom_key.to_owned(),
        custom.map_or(Value::Null, Value::String),
    );
    apply_patch(&patch);
    send("hotkeyError", json!({ "text": "", "keepRecording": false }));
}

fn set_startup(enabled: bool) {
    let result = std::env::current_exe()
        .map_err(|_| "StartupPathError".to_owned())
        .and_then(|exe| crate::install::set_start_with_windows(&exe, enabled, false));
    if let Err(key) = result {
        let language =
            with_state(|s| s.settings.language.clone()).unwrap_or_else(AppLanguage::english);
        log::report_error(&key, &language, None);
    }
    send("startup", json!(crate::install::starts_with_windows()));
}

fn shell_open(target: &str) {
    let verb = wide("open");
    let file = wide(target);
    // SAFETY: null-terminated strings; the shell opens the URL or file with its default handler.
    unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            wm::SW_SHOWNORMAL,
        )
    };
}

/// A save or open file dialog owned by the Settings window; `filter` is WPF's "Label|*.ext" form.
fn file_dialog(save: bool, filter: &str, default_name: &str, extension: &str) -> Option<PathBuf> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let owner = with_state(|s| s.hwnd).unwrap_or(std::ptr::null_mut());
    let filter: Vec<u16> = filter
        .replace('|', "\0")
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let mut buffer = vec![0u16; 1024];
    for (slot, unit) in buffer.iter_mut().zip(default_name.encode_utf16()) {
        *slot = unit;
    }
    let extension = wide(extension.trim_start_matches('.'));
    // SAFETY: the struct points at buffers that live through the modal call.
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buffer.as_mut_ptr();
        ofn.nMaxFile = buffer.len() as u32;
        ofn.lpstrDefExt = extension.as_ptr();
        ofn.Flags = OFN_EXPLORER
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            };
        let ok = if save {
            GetSaveFileNameW(&mut ofn)
        } else {
            GetOpenFileNameW(&mut ofn)
        };
        if ok == 0 {
            return None;
        }
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..end])))
}

fn confirm(text: &str) -> bool {
    let owner = with_state(|s| s.hwnd).unwrap_or(std::ptr::null_mut());
    let text = wide(text);
    let title = wide("OpenTaskbar");
    // SAFETY: a modal message box owned by our window. IDYES = 6.
    unsafe {
        wm::MessageBoxW(
            owner,
            text.as_ptr(),
            title.as_ptr(),
            wm::MB_YESNO | wm::MB_ICONQUESTION | wm::MB_DEFBUTTON2,
        ) == 6
    }
}

fn message_box(text: &str) {
    let owner = with_state(|s| s.hwnd).unwrap_or(std::ptr::null_mut());
    let text = wide(text);
    let title = wide("OpenTaskbar");
    // SAFETY: a modal message box owned by our window.
    unsafe {
        wm::MessageBoxW(
            owner,
            text.as_ptr(),
            title.as_ptr(),
            wm::MB_OK | wm::MB_ICONINFORMATION,
        )
    };
}

/// `ExportSettings_Click` / `ExportFullBackup_Click`.
fn export_settings(full: bool) {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let text = |key: &str| Catalog::get(key, &settings.language).into_owned();
    let path = if full {
        file_dialog(
            true,
            &text("FullBackupFileFilter"),
            "OpenTaskbar-backup.otbackup",
            ".otbackup",
        )
    } else {
        file_dialog(
            true,
            "OpenTaskbar (*.json)|*.json",
            "OpenTaskbar-settings.json",
            ".json",
        )
    };
    let Some(path) = path else {
        return;
    };
    let pins = ot_dock::export_pins();
    let result = if full {
        crate::backup::export_full(&path, &settings, &pins)
    } else {
        crate::backup::export(&path, &settings, &pins)
    };
    match result {
        Ok(()) => {
            log::info(format!("Settings exported to {}.", path.display()));
            message_box(&text(if full {
                "FullBackupExportDone"
            } else {
                "ExportDone"
            }));
        }
        Err(key) => log::report_error(key, &settings.language, None),
    }
}

/// `ImportSettings_Click` / `ImportFullBackup_Click`: the backup's settings replace the current ones (the pin order
/// is kept when the backup has none), its pins are added, and the page reloads.
fn import_settings(full: bool) {
    let Some(previous) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let text = |key: &str| Catalog::get(key, &previous.language).into_owned();
    let path = if full {
        file_dialog(false, &text("FullBackupFileFilter"), "", ".otbackup")
    } else {
        file_dialog(false, "OpenTaskbar (*.json)|*.json", "", ".json")
    };
    let Some(path) = path else {
        return;
    };
    let store = SettingsStore::new(PathBuf::new(), previous.language.clone());
    let result = if full {
        crate::backup::import_full(&path, &store).map(|(mut s, p, theme)| {
            // `FullBackupArchive.Import(restoreTheme: true)`: the theme is installed under a new id and selected.
            match theme.map(|t| (t.id, theme_pack::import_entries(&t.files))) {
                Some((_, Ok(id))) => {
                    log::info(format!("Full backup theme installed as \"{id}\"."));
                    s.theme_pack = Some(id);
                }
                Some((old, Err(key))) => {
                    log::warn(format!("Full backup theme {old:?} not restored: {key}."));
                    s.theme_pack = previous.theme_pack.clone();
                }
                None => {}
            }
            (s, p)
        })
    } else {
        std::fs::read_to_string(&path)
            .map_err(|_| "ErrorSettingsFileInvalid")
            .and_then(|t| crate::backup::parse(&t, &store))
    };
    let (mut settings, pins) = match result {
        Ok(found) => found,
        Err(key) => {
            log::report_error(key, &previous.language, None);
            return;
        }
    };
    if settings.pin_order.is_none() {
        settings.pin_order = previous.pin_order.clone();
    }
    // A backup (or OpenTaskbar's file) has no setup version: importing it must not bring the wizard back.
    settings.onboarding_version = settings.onboarding_version.max(previous.onboarding_version);
    let added = ot_dock::import_pins(&pins);
    log::info(format!(
        "Settings imported from {}; {added} pins added.",
        path.display()
    ));
    with_state(|s| s.settings = settings.clone());
    ot_dock::apply_and_save(settings.clone());
    send(
        "settings",
        serde_json::to_value(&settings).unwrap_or(Value::Null),
    );
    send("strings", json!(Catalog::all(&settings.language)));
    send_theme();
    message_box(
        &Catalog::get(
            if full {
                "FullBackupImportDone"
            } else {
                "ImportDone"
            },
            &settings.language,
        )
        .replace("{0}", &added.to_string()),
    );
}

fn render_mode_path() -> PathBuf {
    app_data_dir().join("render-mode.json")
}

/// The renderer's GPU choice, kept outside the settings file (`RenderModePreference`); read at the next start.
pub fn hardware_rendering() -> bool {
    std::fs::read_to_string(render_mode_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|v| v["software"] == Value::Bool(false))
}

fn set_hardware(enabled: bool) {
    let path = render_mode_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, json!({ "software": !enabled }).to_string()) {
        log::warn(format!(
            "Render mode preference could not be saved: {error}"
        ));
    }
}

/// `UiText.Languages`: the bundled languages with their native names and flags, ordered by English name.
fn languages() -> Value {
    let mut list: Vec<(String, String, String)> = AppLanguage::bundled()
        .map(|l| {
            let culture = l.culture_name().to_owned();
            let native = ot_dock::locale::culture_display_name(&culture, &culture)
                .unwrap_or_else(|| culture.clone());
            let english = ot_dock::locale::culture_display_name(&culture, "en")
                .unwrap_or_else(|| culture.clone());
            (english, native, culture)
        })
        .collect();
    list.sort_by_key(|(english, _, _)| english.to_lowercase());
    list.into_iter()
        .map(|(_, native, culture)| {
            json!({ "value": culture, "label": native, "image": format!("http://ot.localhost/flags/{culture}.png") })
        })
        .collect()
}

fn init_payload() -> Value {
    with_state(|s| {
        let language = &s.settings.language;
        json!({
            "languages": languages(),
            "catalog": catalog(),
            "hotkeys": hotkeys_payload(&s.settings),
            "startup": crate::install::starts_with_windows(),
            "settings": serde_json::to_value(&s.settings).unwrap_or(Value::Null),
            "strings": Catalog::all(language),
            "version": env!("CARGO_PKG_VERSION"),
            "displays": displays(language),
            "hardware": hardware_rendering(),
            "theme": theme_payload(&s.settings),
            "themes": themes_payload(&s.settings),
        })
    })
    .unwrap_or(Value::Null)
}

/// Merges `{ "PascalCaseKey": value }` into the settings, normalized the way a settings file is read, then applies
/// and saves them.
fn apply_patch(patch: &Map<String, Value>) {
    let Some(previous) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let mut value = serde_json::to_value(&previous).unwrap_or(Value::Null);
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for (key, item) in patch {
        object.insert(key.clone(), item.clone());
    }
    commit(previous, value);
}

/// `ResetDockDefaults_Click`: the listed keys go back to their defaults.
fn reset(keys: &[Value]) {
    let Some(previous) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let defaults = serde_json::to_value(DockSettings::default()).unwrap_or(Value::Null);
    let mut value = serde_json::to_value(&previous).unwrap_or(Value::Null);
    if let (Some(object), Some(defaults)) = (value.as_object_mut(), defaults.as_object()) {
        for key in keys.iter().filter_map(Value::as_str) {
            if let Some(default) = defaults.get(key) {
                object.insert(key.to_owned(), default.clone());
            }
        }
    }
    commit(previous, value);
    let settings = with_state(|s| s.settings.clone());
    if let Some(settings) = settings {
        send(
            "settings",
            serde_json::to_value(&settings).unwrap_or(Value::Null),
        );
    }
}

fn commit(previous: DockSettings, value: Value) {
    let store = SettingsStore::new(PathBuf::new(), previous.language.clone());
    let settings = match store.parse(&value.to_string()) {
        Ok(settings) => settings,
        Err(error) => {
            log::warn(format!("Settings: rejected change: {}", error.key()));
            send(
                "settings",
                serde_json::to_value(&previous).unwrap_or(Value::Null),
            );
            return;
        }
    };
    let theme_changed = settings.theme != previous.theme
        || settings.theme_pack != previous.theme_pack
        || settings.settings_background != previous.settings_background
        || settings.settings_opacity != previous.settings_opacity
        || settings.settings_border_thickness != previous.settings_border_thickness
        || settings.settings_border_color != previous.settings_border_color;
    let themes_changed = theme_changed
        || settings.size != previous.size
        || settings.dock_scale != previous.dock_scale
        || settings.dock_icon_padding != previous.dock_icon_padding;
    let language_changed = settings.language != previous.language;
    with_state(|s| s.settings = settings.clone());
    send("hotkeys", hotkeys_payload(&settings));
    ot_dock::apply_and_save(settings.clone());
    if theme_changed {
        send_theme();
    }
    if themes_changed || language_changed {
        send_themes();
    }
    if language_changed {
        send("strings", json!(Catalog::all(&settings.language)));
    }
}

// ---------- Themes page ----------

fn hex(c: ot_render::Color) -> String {
    color_hex((c.A, c.R, c.G, c.B))
}

/// A token brush (or `fallback`) as CSS: a color or a `linear-gradient`.
fn brush_css(key: &str, dark: bool, fallback: ot_render::Color) -> String {
    let rgba = |c: ot_render::Color| {
        format!(
            "rgba({}, {}, {}, {:.4})",
            c.R,
            c.G,
            c.B,
            f64::from(c.A) / 255.0
        )
    };
    match ot_dock::tokens::brush(key, dark) {
        Some((stops, start, end)) if stops.len() > 1 => {
            let angle = (f64::from(end.0 - start.0))
                .atan2(-f64::from(end.1 - start.1))
                .to_degrees();
            let list: Vec<String> = stops
                .iter()
                .map(|(o, c)| format!("{} {:.1}%", rgba(*c), f64::from(*o) * 100.0))
                .collect();
            format!(
                "linear-gradient({:.1}deg, {})",
                (angle + 360.0) % 360.0,
                list.join(", ")
            )
        }
        Some((stops, _, _)) if !stops.is_empty() => rgba(stops[0].1),
        _ => rgba(fallback),
    }
}

/// `ResolveThemePreviewColors`: a gallery card's backdrop, dock, accent and text colors from the pack's widget pack
/// (first gradient stop), else the defaults.
fn preview_colors(pack: Option<&ThemePack>, dark: bool) -> Value {
    let (bg, dock, accent, text) = if dark {
        (0xFF1B2029u32, 0xFF252B37u32, 0xFF38BDF8u32, 0xFFF3F4F6u32)
    } else {
        (0xFFEBEEF3, 0xFFFFFFFF, 0xFF0369A1, 0xFF1B1B1B)
    };
    let h = |v: u32| format!("#{v:08X}");
    if pack.and_then(|p| p.widgets.as_ref()).is_none() {
        return json!({ "bg": h(bg), "dock": h(dock), "accent": h(accent), "text": h(text) });
    }
    let read = |key: &str, default: u32| -> u32 {
        let Some(widgets) = pack.and_then(|p| p.widgets.as_ref()) else {
            return default;
        };
        let value = widgets.raw(key, dark);
        let text = value.and_then(|v| {
            v.as_str().map(str::to_owned).or_else(|| {
                v.get("stops")
                    .and_then(|s| s.get(0))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        });
        text.and_then(|t| theme_pack::parse_color(&t))
            .map(|c| c | 0xFF00_0000)
            .unwrap_or(default)
    };
    let bg = read("SettingsBackground", bg);
    let dock = read("DockMaterial", bg);
    let accent = read("AccentBrush", read("WidgetAccent", accent));
    let text = read("TextPrimary", text);
    json!({ "bg": h(bg), "dock": h(dock), "accent": h(accent), "text": h(text) })
}

/// `DescribeThemePack`: "Icon pack · Widget pack — Author".
fn describe(pack: &ThemePack, language: &AppLanguage) -> String {
    let mut parts = Vec::new();
    if pack.has_icons {
        parts.push(Catalog::get("ThemePackIcons", language).into_owned());
    }
    if pack.widgets.is_some() {
        parts.push(Catalog::get("ThemePackWidgets", language).into_owned());
    }
    let author = pack
        .manifest
        .author
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(|a| format!(" — {a}"))
        .unwrap_or_default();
    parts.join(" · ") + &author
}

/// The Themes page: the gallery (`BuildThemeGallery`), the selected theme's details and the color overview
/// (`ThemePropertyEditor.ShowColorSwatches`).
fn themes_payload(settings: &DockSettings) -> Value {
    let dark = tone(settings.theme) == Tone::Dark;
    let language = &settings.language;
    let selected = settings
        .theme_pack
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let packs = theme_pack::discover();
    let choice = |pack: &ThemePack| {
        json!({
            "id": pack.id,
            "name": pack.display_name(),
            "details": describe(pack, language),
            "preview": preview_colors(Some(pack), dark),
        })
    };
    let mut built_in = vec![json!({
        "id": Value::Null,
        "name": Catalog::get("ThemePackNone", language),
        "details": "",
        "preview": preview_colors(None, dark),
    })];
    let mut custom = Vec::new();
    for pack in &packs {
        if pack.is_built_in() {
            built_in.push(choice(pack));
        } else {
            custom.push(choice(pack));
        }
    }
    // A stored theme whose folder is gone stays listed, so an unrelated edit does not clear it.
    if let Some(id) = selected
        && !packs.iter().any(|p| p.id.eq_ignore_ascii_case(id))
    {
        custom.push(json!({
            "id": id,
            "name": format!("{id} (?)"),
            "details": Catalog::get("ThemePackMissing", language),
            "preview": preview_colors(None, dark),
        }));
    }
    let active = selected.and_then(|id| packs.iter().find(|p| p.id.eq_ignore_ascii_case(id)));
    let message = match active {
        Some(pack) if !pack.is_built_in() => {
            Catalog::get("ThemeEditing", language).replace("{0}", &pack.display_name())
        }
        Some(pack) => pack.display_name(),
        None => Catalog::get("ThemeWindowsAppearance", language).into_owned(),
    };
    let page = if dark { 0xFF1B2029u32 } else { 0xFFF3F3F3 };
    let control = if dark { 0xFF252B37u32 } else { 0xFFFFFFFF };
    let argb = |v: u32| ot_render::color((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8);
    let swatches = [
        (
            "ColorSwatchAccent",
            "AccentBrush",
            ot_dock::theme::accent(dark),
        ),
        ("ColorSwatchBackground", "SettingsBackground", argb(page)),
        ("ColorSwatchSurface", "SettingsControl", argb(control)),
        (
            "ColorSwatchText",
            "TextPrimary",
            ot_dock::theme::text_primary(dark),
        ),
        (
            "ColorSwatchSecondary",
            "TextSecondary",
            ot_dock::theme::text_secondary(dark),
        ),
        (
            "ColorSwatchBorder",
            "StrokeSubtle",
            ot_dock::theme::stroke_subtle(dark),
        ),
    ]
    .iter()
    .map(|(label, key, fallback)| json!({ "label": label, "css": brush_css(key, dark, *fallback) }))
    .collect::<Vec<_>>();
    let border = settings
        .menu_border_color
        .as_deref()
        .and_then(ot_dock::theme::parse_color);
    json!({
        "builtIn": built_in,
        "custom": custom,
        "selected": selected,
        "details": active.map(|p| describe(p, language)).unwrap_or_default(),
        "selectedBuiltIn": selected.is_some_and(theme_pack::is_built_in),
        "editable": active.is_some_and(|p| !p.is_built_in()),
        "message": message,
        "swatches": swatches,
        "cornerDefault": ot_dock::default_corner_radius(settings),
        "borderSwatch": border.map(hex),
        "editor": active
            .filter(|p| !p.is_built_in())
            .map(|p| crate::theme_editor::payload(p, dark, language)),
        "borderDefaultValue": ot_dock::token_defaults::DEFAULTS
            .iter()
            .find(|(k, ..)| *k == "MenuStroke")
            .map(|&(_, _, d, l)| serde_json::from_str::<Value>(if dark { d } else { l }).unwrap_or(Value::Null)),
        "borderDefault": brush_css("MenuStroke", dark, if dark {
            ot_render::color(0x70, 0xFF, 0xFF, 0xFF)
        } else {
            ot_render::color(0x85, 0xFF, 0xFF, 0xFF)
        }),
    })
}

fn send_themes() {
    if let Some(settings) = with_state(|s| s.settings.clone()) {
        send("themes", themes_payload(&settings));
    }
}

/// A theme card clicked: the theme is selected, applied and saved (`ThemePack`, `ThemeApplicationIcons = false`).
fn select_theme(id: Option<String>) {
    apply_patch(
        &json!({ "ThemePack": id, "ThemeApplicationIcons": false })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );
    send_themes();
}

/// `ReloadThemePacks`: lists the themes again and selects `id`, saved at once.
fn reload_themes(id: Option<String>) {
    select_theme(id);
    ot_dock::reload_theme();
    send_themes();
    send_theme();
}

/// `ThemePackNew_Click`, `ThemePackDuplicate_Click` and the editor's "Create editable theme".
fn new_theme(kind: &str, message: &Value) {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let language = &settings.language;
    let selected = settings.theme_pack.clone().filter(|s| !s.trim().is_empty());
    let (name, based_on, done) = match kind {
        "themeNew" => (
            message["name"].as_str().unwrap_or_default().to_owned(),
            None,
            Some("ThemePackCreated"),
        ),
        "themeDuplicate" => {
            if selected.is_none() {
                log::report_error("ErrorThemeMissing", language, None);
                return;
            }
            (
                message["name"].as_str().unwrap_or_default().to_owned(),
                selected.clone(),
                Some("ThemePackDuplicated"),
            )
        }
        _ => {
            // `ThemeNewEditableName` with the current mode ("My Dark Theme").
            let mode = Catalog::get(
                if tone(settings.theme) == Tone::Light {
                    "Light"
                } else {
                    "Dark"
                },
                language,
            );
            (
                Catalog::get("ThemeNewEditableName", language).replace("{0}", &mode),
                selected.clone(),
                None,
            )
        }
    };
    if name.trim().is_empty() {
        return;
    }
    match theme_pack::create(&name, None, based_on.as_deref()) {
        Ok(id) => {
            log::info(format!("Theme pack \"{id}\" created."));
            reload_themes(Some(id));
            if let Some(done) = done {
                message_box(&Catalog::get(done, language));
            }
        }
        Err(error) => {
            log::warn(format!("Theme creation failed: {error}"));
            log::report_error("ErrorThemeInstall", language, None);
        }
    }
}

/// `ThemePackExport_Click`: a zip whose entries sit under a folder named after the theme.
fn export_theme() {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let language = &settings.language;
    let Some(id) = settings.theme_pack.clone().filter(|s| !s.trim().is_empty()) else {
        log::report_error("ErrorThemeMissing", language, None);
        return;
    };
    let files = match theme_pack::export_files(&id) {
        Ok(files) => files,
        Err(key) => {
            log::report_error(key, language, None);
            return;
        }
    };
    let Some(path) = file_dialog(
        true,
        &Catalog::get("ThemePackFileFilter", language),
        &format!("{id}{}", theme_pack::EXPORT_EXTENSION),
        theme_pack::EXPORT_EXTENSION,
    ) else {
        return;
    };
    let names: Vec<String> = files.iter().map(|(n, _)| format!("{id}/{n}")).collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .zip(&files)
        .map(|(n, (_, b))| (n.as_str(), b.as_slice()))
        .collect();
    match std::fs::write(&path, crate::backup::zip_write(&entries)) {
        Ok(()) => {
            log::info(format!("Theme \"{id}\" exported to {}.", path.display()));
            message_box(&Catalog::get("ThemePackExported", language));
        }
        Err(error) => {
            log::warn(format!("Theme export failed: {error}"));
            log::report_error("ErrorThemeInstall", language, None);
        }
    }
}

/// `ThemePackImport_Click`.
fn import_theme() {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let language = &settings.language;
    let Some(path) = file_dialog(
        false,
        &Catalog::get("ThemePackFileFilter", language),
        "",
        theme_pack::EXPORT_EXTENSION,
    ) else {
        return;
    };
    let entries = std::fs::metadata(&path)
        .ok()
        .filter(|m| m.len() <= theme_pack::MAX_UNCOMPRESSED_BYTES)
        .and_then(|_| std::fs::read(&path).ok())
        .and_then(|bytes| crate::backup::zip_read(&bytes));
    let Some(entries) = entries else {
        log::report_error("ErrorThemeInvalid", language, None);
        return;
    };
    match theme_pack::import_entries(&entries) {
        Ok(id) => {
            log::info(format!("Theme \"{id}\" imported from {}.", path.display()));
            reload_themes(Some(id));
            message_box(&Catalog::get("ThemePackImported", language));
        }
        Err(key) => log::report_error(key, language, None),
    }
}

/// `ThemePackDelete_Click`: asked first; the built-in look comes back.
fn delete_theme() {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let language = &settings.language;
    let Some(id) = settings.theme_pack.clone().filter(|s| !s.trim().is_empty()) else {
        log::report_error("ErrorThemeMissing", language, None);
        return;
    };
    if theme_pack::is_built_in(&id)
        || !confirm(&Catalog::get("ThemePackDeleteConfirm", language).replace("{0}", &id))
    {
        return;
    }
    match theme_pack::delete(&id) {
        Ok(()) => {
            log::info(format!("Theme \"{id}\" deleted."));
            reload_themes(None);
            message_box(&Catalog::get("ThemePackDeleted", language));
        }
        Err(key) => log::report_error(key, language, None),
    }
}

/// `BorderColorButton_Click` after the brush picker: the color (a gradient's first stop, `BrushToHex`) goes to all
/// four surfaces.
fn set_border_color(value: &str) {
    let Some(color) = theme_pack::parse_color(value) else {
        return;
    };
    let value = format!("#{color:08X}");
    apply_patch(
        &json!({
            "DockBorderColor": value, "MenuBorderColor": value,
            "WidgetBorderColor": value, "SettingsBorderColor": value,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    );
    send(
        "settings",
        serde_json::to_value(with_state(|s| s.settings.clone())).unwrap_or(Value::Null),
    );
    send_themes();
}

/// An edit in the theme editor: the token or the theme's information is written to the active custom theme, which is
/// then reread and applied (`SaveTheme`).
fn edit_theme(kind: &str, message: &Value) {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let Some(id) = settings.theme_pack.clone().filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let dark = tone(settings.theme) == Tone::Dark;
    let language = &settings.language;
    let result = if kind == "themeToken" {
        let key = message["key"].as_str().unwrap_or_default();
        crate::theme_editor::save_token(&id, dark, key, &message["value"]).map_err(|error| {
            log::warn(format!("Theme token {key} not saved: {error}"));
            "ThemeSaveFailed"
        })
    } else {
        let text = |name: &str| message[name].as_str().unwrap_or_default().to_owned();
        crate::theme_editor::save_info(&id, &text("name"), &text("author"), &text("description"))
    };
    let note = match result {
        Ok(()) => Catalog::get("ThemeSavedMessage", language).into_owned(),
        Err("ThemeNameInvalid") => Catalog::get("ThemeNameInvalid", language).into_owned(),
        Err(key) => Catalog::get(key, language).replace("{0}", ""),
    };
    ot_dock::reload_theme();
    send_theme();
    send_themes();
    send("editorMessage", json!(note));
}
