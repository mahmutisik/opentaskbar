//! The dock window: a composition-rendered bar driven by the taskbar session, the window tracker and the
//! settings. Phase 3 scope: Start, Search, Task View, pinned and running apps with indicators, Show Desktop and the
//! Recycle Bin; clicking launches, activates or minimizes. Phase 5: magnification (`magnify.rs`). Menus and widgets
//! come later.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ot_core::language::Catalog;
use ot_core::log;
use ot_core::settings::{
    AutoHideMode, DockEdge, DockLayout, DockSettings, DockWidthMode, StartButtonPlacement,
};
use ot_render::Composition::{
    CompositionColorBrush, CompositionRoundedRectangleGeometry, CompositionSurfaceBrush,
    ContainerVisual, ShapeVisual, SpriteVisual,
};
use ot_render::{Color, Renderer, Vector2, Vector3};
use ot_shell::session::{self, Session, SessionEvent};
use ot_shell::tracker::{self, TrackedWindow, TrackerEvent, WindowTracker};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::core::Result as WinResult;
use windows::core::{HSTRING, Interface};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, CreateRoundRectRgn, SetWindowRgn};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_ESCAPE, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use crate::actions;
use crate::autohide::{self, AutoHide};
use crate::fisheye;
use crate::frame_clock::FrameClock;
use crate::hotkey;
use crate::icon_loader::IconLoader;
use crate::icons;
use crate::jumplist::{self, JumpLists};
use crate::layout::{
    BarLayout, BarPlacement, BarSpec, Group, SizeProfile, Slot, application_icon_pixels, layout_bar,
};
use crate::magnify::Magnifier;
use crate::menu;
use crate::pins::{self, Pin};
use crate::preview;
use crate::reorder;
use crate::theme::{self, DockColors, SHADOW_BLUR_DIPS, SHADOW_DEPTH_DIPS, SHADOW_OPACITY};
use crate::tooltip;
use crate::tray_flyout;
use crate::traypin;
use crate::uia::{self, UiaItem, UiaModel};

/// Private bytes of this process in MB, for startup memory checkpoints in the log.
pub fn private_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: the struct size is passed; the pseudo handle needs no closing.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    if ok == 0 {
        0.0
    } else {
        counters.PrivateUsage as f64 / 1_048_576.0
    }
}

fn memory_mark(stage: &str) {
    log::info(format!("Memory {stage}: {:.1} MB private", private_mb()));
}

const WM_TRACKER: u32 = wm::WM_APP + 10;
const WM_ICONS: u32 = wm::WM_APP + 11;
const WM_INVOKE: u32 = wm::WM_APP + 12;
const WM_FRAME: u32 = wm::WM_APP + 13;
const WM_JUMPLIST: u32 = wm::WM_APP + 14;
const WM_TRAY_CHANGED: u32 = wm::WM_APP + 15;
const WM_FULL_SCREEN: u32 = wm::WM_APP + 16;
/// Opens the Settings window outside the dock's own borrow (the window reads the dock's settings).
const WM_OPEN_SETTINGS: u32 = wm::WM_APP + 25;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: u32 = 0x7;
const WTS_SESSION_UNLOCK: u32 = 0x8;
/// Defined with the common controls in the Windows headers.
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_END: usize = 1;
const TIMER_HIDE: usize = 2;
const TIMER_GUARD: usize = 3;
const TIMER_HIDDEN: usize = 4;
const TIMER_REMOVE: usize = 5;
const TIMER_TIP_SHOW: usize = 6;
const TIMER_TIP_HIDE: usize = 7;
const TIMER_PREVIEW_OPEN: usize = 8;
const TIMER_PREVIEW_CLOSE: usize = 9;
const TIMER_JUMPLIST: usize = 10;
/// The widgets' shared tick, aligned to the start of each second.
const TIMER_WIDGETS: usize = 11;
/// `LauncherHotkeyId`, `DockHotkeyId`.
const HOTKEY_LAUNCHER: i32 = 0x4F01;
const HOTKEY_DOCK: i32 = 0x4F02;
const WM_HOTKEY: u32 = 0x0312;
/// `LoadJumpListAsync`: a jump list that is not read within 3 s shows `TasksTimeout`.
const JUMPLIST_TIMEOUT_MS: u32 = 3000;
/// Menu commands at and above this value are jump list entries.
const JUMPLIST_COMMAND: u32 = 1000;
const GLYPH_FONTS: &[&str] = &["Segoe Fluent Icons", "Segoe MDL2 Assets"];

#[derive(Clone, Debug, PartialEq)]
enum ItemKind {
    Start,
    Search,
    TaskView,
    /// `key`: lowercase AUMID for packaged apps, otherwise the lowercase executable path.
    App {
        key: String,
        pin: Option<Box<Pin>>,
        executable_path: String,
        app_id: Option<String>,
    },
    ShowDesktop,
    /// A pinned tray icon in the strip before the background-apps button (`PinnedTrayStrip`).
    TrayPin(Box<TrayPin>),
    /// The background-apps button (`TrayButton`).
    Tray,
    /// A dock widget card (`widget:<id>`).
    Widget(&'static str),
    RecycleBin,
}

/// A running tray icon the user pinned to the dock.
#[derive(Clone, Debug)]
struct TrayPin {
    /// The stored pin key it matched (decides how a click opens it).
    key: String,
    title: String,
    hwnd: isize,
    uid: u32,
    path: Option<String>,
    icon: Option<Arc<crate::icons::Icon>>,
    /// Identifies the icon's pixels, so a changed icon is redrawn.
    icon_hash: u64,
    /// A registry app with no live icon: both buttons open the app by its path.
    fallback: bool,
}

impl PartialEq for TrayPin {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.title == other.title
            && self.hwnd == other.hwnd
            && self.uid == other.uid
            && self.path == other.path
            && self.icon_hash == other.icon_hash
            && self.fallback == other.fallback
    }
}

struct Item {
    kind: ItemKind,
    group: Group,
    windows: Vec<Hwnd>,
    foreground: bool,
    /// A window of the app asks for attention (flashing): the notification badge shows.
    notify: bool,
}

type Hwnd = isize;

/// Saves settings the dock changed itself.
type SaveSettings = Box<dyn Fn(&DockSettings)>;

/// Composition values captured before and after a change, to animate between them.
struct Snapshot {
    bar_offset: Vector3,
    bar_size: Vector2,
    geometry_size: Vector2,
    items: Vec<ItemSnapshot>,
    separators: Vec<Vector3>,
}

struct ItemSnapshot {
    container_offset: Vector3,
    container_size: Vector2,
    icon_offset: Vector3,
    icon_scale: Vector3,
    indicator_offset: Vector3,
}

/// A drag in progress: the apps along the bar, their rest slots (bar-relative pixels) and the pointer.
struct DragState {
    item: usize,
    apps: Vec<usize>,
    dragged: usize,
    slots: Vec<(f32, f32)>,
    origin: f32,
    offset: f32,
    insertion: Option<usize>,
    pinned: usize,
}

/// A rounded `SurfaceKeyboardFocus` surface behind the focused item's icon.
/// `ShellDockButtonTemplate`: the hover and pressed layers under a shell button's glyph.
struct ShellSurface {
    hover: ShapeVisual,
    hover_geometry: CompositionRoundedRectangleGeometry,
    hover_brush: CompositionColorBrush,
    pressed: ShapeVisual,
    pressed_geometry: CompositionRoundedRectangleGeometry,
    pressed_brush: CompositionColorBrush,
    hovered: bool,
    pressed_on: bool,
    /// A widget card's own background and rim, under the hover layers (`WidgetCardButton`'s Surface).
    card: Option<(
        ShapeVisual,
        CompositionRoundedRectangleGeometry,
        ot_render::Composition::CompositionSpriteShape,
    )>,
    /// What a widget draws under the hover layers (the battery level fill).
    backdrop: Option<SpriteVisual>,
}

struct FocusVisual {
    shape: ShapeVisual,
    geometry: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
    parent: Option<ContainerVisual>,
}

/// Keyboard navigation in progress.
struct KeyboardNav {
    focused: usize,
    foreground_before: Hwnd,
    /// Space was pressed on this item; it clicks when released (a WPF button).
    space_down: Option<usize>,
}

/// Removal fade (`AnimateDockAppRemoval`), dock width change (`ResizeDockToContent`), launch bounce
/// (`DockMetrics.LaunchBounceMs`).
const REMOVE_MS: u32 = 75;
const RESIZE_MS: u32 = 120;
const BOUNCE_MS: u32 = 420;

/// Identity of an item across rebuilds.
fn item_key(item: &Item) -> String {
    match &item.kind {
        ItemKind::App { key, .. } => key.clone(),
        ItemKind::TrayPin(pin) => format!("traypin:{}", pin.key.to_lowercase()),
        ItemKind::Widget(id) => format!("widget:{id}"),
        other => format!("shell:{other:?}"),
    }
}

fn ease_out(
    c: &ot_render::Composition::Compositor,
) -> WinResult<ot_render::Composition::CubicBezierEasingFunction> {
    // WPF CubicEase EaseOut.
    c.CreateCubicBezierEasingFunction(Vector2 { X: 0.33, Y: 1.0 }, Vector2 { X: 0.68, Y: 1.0 })
}

fn milliseconds(ms: u32) -> windows::Foundation::TimeSpan {
    windows::Foundation::TimeSpan {
        Duration: i64::from(ms) * 10_000,
    }
}

/// The app a context menu was opened for, captured when it opened (the dock may change while it is open).
struct MenuTarget {
    name: String,
    executable: String,
    pin: Option<Pin>,
    windows: Vec<Hwnd>,
    /// The process of each window when the menu opened: Close only posts to a window still owned by it.
    owners: Vec<u32>,
    foreground: bool,
    /// A background app from the tray flyout: Close asks each of its processes (`CloseApplication`), and either
    /// command closes the flyout.
    background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum MenuCommand {
    Activate = 1,
    NewInstance,
    RunAsAdmin,
    CloseApp,
    Terminate,
    TaskManager,
    FileExplorer,
    Run,
    Terminal,
    TerminalAdmin,
    DeviceManager,
    DiskManagement,
    ComputerManagement,
    EventViewer,
    NetworkConnections,
    Lock,
    SignOut,
    Restart,
    ShutDown,
    Exit,
    Pin,
    Unpin,
    RemoveWidget,
    Settings,
}

impl MenuCommand {
    const ALL: [MenuCommand; 24] = [
        Self::Activate,
        Self::NewInstance,
        Self::RunAsAdmin,
        Self::CloseApp,
        Self::Terminate,
        Self::TaskManager,
        Self::FileExplorer,
        Self::Run,
        Self::Terminal,
        Self::TerminalAdmin,
        Self::DeviceManager,
        Self::DiskManagement,
        Self::ComputerManagement,
        Self::EventViewer,
        Self::NetworkConnections,
        Self::Lock,
        Self::SignOut,
        Self::Restart,
        Self::ShutDown,
        Self::Exit,
        Self::Pin,
        Self::Unpin,
        Self::RemoveWidget,
        Self::Settings,
    ];

    fn from(value: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|c| *c as usize == value)
    }
}

/// Per-item values of the last magnified frame (NaN: never written).
#[derive(Clone, Copy)]
struct Applied {
    start: f32,
    extra: f32,
    scale: f32,
    lift: f32,
}

impl Default for Applied {
    fn default() -> Self {
        Self {
            start: f32::NAN,
            extra: f32::NAN,
            scale: f32::NAN,
            lift: f32::NAN,
        }
    }
}

struct ItemVisual {
    container: ContainerVisual,
    /// Holds the icon; the click bounce animates its offset.
    bounce: ContainerVisual,
    icon: SpriteVisual,
    indicator: SpriteVisual,
    icon_key: Option<(String, u32)>,
    /// The resting bitmap (drawn 1:1) and the larger variant shown while magnified, so icons stay sharp.
    rest_brush: Option<CompositionSurfaceBrush>,
    hover_brush: Option<CompositionSurfaceBrush>,
    hover_key: Option<(String, u32)>,
    /// Notification dot at the icon's top-right corner; a child of the icon, so it scales with it.
    badge: SpriteVisual,
    /// Hover and pressed layers of a shell button (Start, Search, Task View, tray button, pinned tray icons).
    shell: Option<ShellSurface>,
}

struct Visuals {
    bar: ContainerVisual,
    shadow: SpriteVisual,
    shape: ShapeVisual,
    /// The bar's fill and rim.
    fill: ot_render::Composition::CompositionSpriteShape,
    geometry: CompositionRoundedRectangleGeometry,
    separators: Vec<SpriteVisual>,
    items: Vec<ItemVisual>,
    /// `ActiveIndicator` colors; the dots are drawn for the current scale on first use.
    indicator_colors: (Color, Color),
    indicator_dots: Option<(f32, CompositionSurfaceBrush, CompositionSurfaceBrush)>,
    separator_brush: CompositionColorBrush,
    glyphs: HashMap<&'static str, ot_render::Composition::CompositionSurfaceBrush>,
    badge_brush: Option<CompositionSurfaceBrush>,
}

struct Dock {
    hwnd: Hwnd,
    session: Session,
    renderer: Renderer,
    tracker: Option<WindowTracker>,
    icons: IconLoader,
    settings: DockSettings,
    profile: SizeProfile,
    colors: DockColors,
    pins: Vec<Pin>,
    items: Vec<Item>,
    foreground: Hwnd,
    foreground_at_press: Option<Hwnd>,
    /// The foreground app when the pointer went down, read on `WM_MOUSEACTIVATE`: by `WM_LBUTTONDOWN` the click has
    /// already moved the foreground input queue to the dock's thread.
    foreground_before_click: Option<Option<Hwnd>>,
    visuals: Visuals,
    layout: Option<BarLayout>,
    placement: Option<BarPlacement>,
    corner_radius: f32,
    uia: std::sync::Arc<UiaModel>,
    /// Bar rectangle in window pixels (left, top, right, bottom), for hit testing.
    bar_rect: (i32, i32, i32, i32),
    magnifier: Magnifier,
    frames: FrameClock,
    tracking_mouse: bool,
    last_mouse: Option<(i32, i32)>,
    hover_icons_requested: bool,
    /// While magnified: each item's slot (start along the bar in window pixels, length) and icon rectangle.
    slots: Vec<(f32, f32)>,
    icon_rects: Vec<(f32, f32, f32, f32)>,
    /// What the last magnified frame wrote per item, so unchanged values are not written again.
    applied: Vec<Applied>,
    /// Smart overlap checks: (count, total microseconds, max microseconds), logged when the dock stops.
    overlap_stats: (u64, u64, u64),
    autohide: AutoHide,
    menu_target: Option<MenuTarget>,
    /// Left button pressed on an app (item, x, y): a drag starts past the drag distance.
    press: Option<(usize, i32, i32)>,
    drag: Option<DragState>,
    /// Window changes that arrived during a drag, applied when it ends.
    rebuild_pending: bool,
    /// Leaving apps are fading out; the new item list is applied when the fade ends.
    removal_pending: bool,
    removal_faded: bool,
    /// Display names of running apps (FileDescription or the packaged app's name), by item key.
    names: HashMap<String, String>,
    tip_target: Option<usize>,
    tip_shown: bool,
    tip_closed_at: Option<Instant>,
    /// A click hides the tooltip until the pointer leaves the item (WPF).
    tip_suppressed: Option<usize>,
    /// Window previews: the app item under the pointer, the one waiting for the open delay, and the app shown.
    preview_item: Option<usize>,
    preview_pending: Option<usize>,
    preview_key: Option<String>,
    /// The Recycle Bin's display name, read once (Explorer windows showing it get its name and icon in previews).
    recycle_bin_name: Option<Option<String>>,
    jump_lists: JumpLists,
    /// Registered global hotkeys (launcher, dock).
    hotkeys: (Option<hotkey::Binding>, Option<hotkey::Binding>),
    /// Keyboard navigation (the dock hotkey): the focused item and the app to return to.
    keyboard: Option<KeyboardNav>,
    /// The focused item's highlight and the item container it is drawn in.
    focus_visual: Option<FocusVisual>,
    /// The window each app's last click brought forward (`_lastSelectedWindows`).
    last_selected: HashMap<String, Hwnd>,
    /// Wheel deltas not yet worth a 2 % volume step (touchpads send small ones).
    wheel_remainder: i32,
    /// The jump list session of the open app menu, and whether it is still loading.
    jump_session: Option<u64>,
    jump_loading: bool,
    /// The pinned tray icons shown before the tray button.
    tray_pins: Vec<TrayPin>,
    /// The shell button under the pointer (its hover layer is shown).
    shell_hovered: Option<usize>,
    /// Dock widgets (Phase 7), their text measurer and their card widths in DIPs.
    widgets: crate::widgets::Widgets,
    widget_painter: Option<crate::widgets::Painter>,
    widget_widths: HashMap<&'static str, f32>,
    /// Bumped when a widget's drawing changes, so its surface is redrawn.
    widget_generation: HashMap<&'static str, u64>,
    /// A widget's width changed while the icons were magnified: relayout once the magnification ends.
    widget_relayout_pending: bool,
    /// The widget a context menu was opened for.
    menu_widget: Option<&'static str>,
    /// Registry background apps (`TrayIconSource`), for pins whose app has no live icon.
    tray_apps: Option<crate::trayapps::Source>,
    hide_timer: bool,
    guard_timer: bool,
    /// Hidden and out of sight: the input region is the edge strip.
    strip_region: bool,
}

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    /// Kept apart from `DOCK`: UI Automation can ask for it while the dock is mid-update.
    static UIA: RefCell<Option<std::sync::Arc<UiaModel>>> = const { RefCell::new(None) };
    /// Saves settings the dock changes itself (pin order). Unset: read-only (e.g. `--use-settings` in development).
    static PERSIST: RefCell<Option<SaveSettings>> = const { RefCell::new(None) };
    static SETTINGS_OPENER: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static SETTINGS_ON_START: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The standalone dock (`run`) ends the message loop when its window closes; a hosted dock does not.
    static QUIT_ON_CLOSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Creates the dock window, hides Explorer's taskbar and shows the dock. The caller runs the thread's message
/// loop (the setup wizard shares it). Use `apply_settings` for live changes and `stop` to put the taskbar back.
pub fn start(settings: DockSettings) -> std::result::Result<(), &'static str> {
    if is_running() {
        return Ok(());
    }
    // SAFETY: COM for pin shortcuts on this thread (the renderer sets up its own apartment via the queue).
    let _ = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    memory_mark("before window");
    let hwnd = create_window().ok_or("ErrorDockWindow")?;
    let dock = match Dock::new(hwnd, settings) {
        Ok(dock) => dock,
        Err(error) => {
            log::error(format!("Dock renderer failed: {error}"));
            // SAFETY: our own window.
            unsafe { wm::DestroyWindow(hwnd as HWND) };
            return Err("ErrorDockRenderer");
        }
    };
    UIA.with(|slot| *slot.borrow_mut() = Some(dock.uia.clone()));
    DOCK.with(|slot| *slot.borrow_mut() = Some(dock));
    let started = with_dock(|dock| dock.start()).unwrap_or(Err("ErrorDockWindow"));
    if let Err(key) = started {
        log::error(format!("Dock start failed: {key}"));
        DOCK.with(|slot| slot.borrow_mut().take());
        // SAFETY: our own window.
        unsafe { wm::DestroyWindow(hwnd as HWND) };
        return Err(key);
    }
    ot_core::startup::mark("dock visible");
    if SETTINGS_ON_START.with(|flag| flag.replace(false)) {
        // SAFETY: posting to our own window: Settings opens once the loop runs.
        unsafe { wm::PostMessageW(hwnd as HWND, WM_OPEN_SETTINGS, 0, 0) };
    }
    Ok(())
}

/// The Windows taskbar pins for a settings backup (`WindowsTaskbarPinImporter`): name, target, arguments.
pub fn export_pins() -> Vec<(String, String, Option<String>)> {
    pins::load(None)
        .into_iter()
        .map(|p| (p.name, p.executable_path, p.arguments))
        .collect()
}

/// `MainWindow.ImportPins`: pins the backup's apps that are not pinned yet; returns how many were added.
pub fn import_pins(list: &[(String, String, Option<String>)]) -> usize {
    let existing: Vec<String> = pins::load(None)
        .into_iter()
        .map(|p| p.executable_path.to_lowercase())
        .collect();
    let mut added = 0;
    for (name, executable, arguments) in list {
        if executable.trim().is_empty() || existing.contains(&executable.to_lowercase()) {
            continue;
        }
        match pins::add(name, executable, arguments.as_deref()) {
            Ok(_) => added += 1,
            Err(key) => log::warn(format!("Pin import skipped {executable}: {key}")),
        }
    }
    if added > 0 {
        with_dock(|dock| {
            let order = dock.pin_order();
            dock.apply_pin_order(order);
        });
    }
    added
}

/// A widget card as the dock draws it, as PNG, for the Settings catalog (the running widget when it is on the dock,
/// else a fresh one updated once); `scale` is the Settings window's.
pub fn widget_preview_png(id: &str, scale: f32) -> Option<Vec<u8>> {
    with_dock(|dock| dock.widget_preview(id, scale)).flatten()
}

static SETUP_RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Settings' "Reset and run setup again": the dock closes (the taskbar comes back) and `run` returns; the app then
/// starts again with `--reset-setup`.
pub fn restart_into_setup() {
    SETUP_RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(hwnd) = with_dock(|dock| dock.hwnd) {
        // SAFETY: our own window; closing it restores Explorer's taskbar.
        unsafe { wm::PostMessageW(hwnd as HWND, wm::WM_CLOSE, 0, 0) };
    }
}

/// Whether the dock closed for `restart_into_setup`.
pub fn setup_restart_requested() -> bool {
    SETUP_RESTART.load(std::sync::atomic::Ordering::SeqCst)
}

/// `--settings` (`OpenSettingsOnStartup`): the Settings window opens as soon as the dock is up.
pub fn open_settings_on_start() {
    SETTINGS_ON_START.with(|flag| flag.set(true));
}

pub fn is_running() -> bool {
    DOCK.with(|slot| slot.try_borrow().map(|d| d.is_some()).unwrap_or(true))
}

/// Where the dock saves settings it changes itself (pins and their order). Without it the dock never writes.
/// Who opens the Settings window (the app registers it; the dock crate does not know the window).
pub fn set_settings_opener(open: impl Fn() + 'static) {
    SETTINGS_OPENER.with(|slot| *slot.borrow_mut() = Some(Box::new(open)));
}

/// `OpenSettings_Click`.
fn open_settings() {
    SETTINGS_OPENER.with(|slot| match slot.borrow().as_ref() {
        Some(open) => open(),
        None => log::info("Settings window is not available in this mode."),
    });
}

/// The dock's current settings (the Settings window starts from them).
pub fn current_settings() -> Option<DockSettings> {
    with_dock(|dock| dock.settings.clone())
}

/// Applies `settings` to the running dock and saves them (the Settings window's auto-save).
pub fn apply_and_save(settings: DockSettings) {
    persist(&settings);
    apply_settings(settings);
}

pub fn set_persist(save: impl Fn(&DockSettings) + 'static) {
    PERSIST.with(|slot| *slot.borrow_mut() = Some(Box::new(save)));
}

fn persist(settings: &DockSettings) {
    PERSIST.with(|slot| match slot.borrow().as_ref() {
        Some(save) => save(settings),
        None => log::info("Settings changed by the dock are not saved (read-only settings)."),
    });
}

/// Records the completed setup version in the running dock's settings (nothing is redrawn).
pub fn set_onboarding_version(version: u32) {
    with_dock(|dock| dock.settings.onboarding_version = version);
}

/// `ThemePackReload_Click`: rereads the active theme pack from disk and redraws the dock with it.
pub fn reload_theme() {
    with_dock(|dock| {
        crate::tokens::select(dock.settings.theme_pack.as_deref(), true);
        let settings = dock.settings.clone();
        dock.apply(settings);
    });
}

/// `DockMetrics.For(size, scale).Radius`: the corner radius used when `DockCornerRadius` is not set.
pub fn default_corner_radius(settings: &DockSettings) -> i32 {
    crate::layout::SizeProfile::for_size(
        settings.size,
        settings.dock_scale,
        settings.dock_icon_padding,
    )
    .radius
    .round() as i32
}

/// Applies new settings to the running dock immediately (theme, material, size, edge, buttons, layout).
pub fn apply_settings(settings: DockSettings) {
    with_dock(|dock| dock.apply(settings));
}

/// Restores Explorer's taskbar and closes the dock window.
pub fn stop() -> std::result::Result<(), &'static str> {
    let Some((hwnd, result)) = with_dock(|dock| {
        dock.log_stats();
        dock.tracker.take();
        // Tray icons go back to Explorer before its taskbar returns.
        tray_flyout::close();
        crate::panel::close();
        ot_shell::tray::stop();
        dock.tray_apps.take();
        (dock.hwnd, dock.session.stop())
    }) else {
        return Ok(());
    };
    DOCK.with(|slot| slot.borrow_mut().take());
    // SAFETY: our own window.
    unsafe { wm::DestroyWindow(hwnd as HWND) };
    result
}

/// Runs the dock alone on the calling thread until it closes (or for `run_for_seconds`, for tests).
/// Returns the taskbar restore result.
pub fn run(
    settings: DockSettings,
    run_for_seconds: Option<u32>,
) -> std::result::Result<(), &'static str> {
    QUIT_ON_CLOSE.with(|q| q.set(true));
    start(settings)?;
    if let Some(seconds) = run_for_seconds
        && let Some(hwnd) = with_dock(|dock| dock.hwnd)
    {
        // SAFETY: timer on our own window.
        unsafe { wm::SetTimer(hwnd as HWND, TIMER_END, seconds * 1000, None) };
    }
    let mut msg: wm::MSG = unsafe { std::mem::zeroed() };
    // SAFETY: standard message loop.
    while unsafe { wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            wm::TranslateMessage(&msg);
            wm::DispatchMessageW(&msg);
        }
    }
    let result = with_dock(|dock| {
        dock.log_stats();
        dock.tracker.take();
        tray_flyout::close();
        crate::panel::close();
        ot_shell::tray::stop();
        dock.tray_apps.take();
        dock.session.stop()
    })
    .unwrap_or(Ok(()));
    DOCK.with(|slot| slot.borrow_mut().take());
    result
}

fn with_dock<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    let result = DOCK.with(|slot| {
        slot.try_borrow_mut()
            .ok()
            .and_then(|mut dock| dock.as_mut().map(f))
    });
    if result.is_some() {
        // The outermost borrow just ended: work that arrived while it was held runs now.
        repost_deferred();
    }
    result
}

thread_local! {
    static LAST_START: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    /// Work messages that arrived while the dock was borrowed (a modal loop inside a handler: a message box, a menu,
    /// a drag), posted again once the borrow ends. Dropping them lost work for good: a dropped `WM_FRAME` kept the
    /// frame clock's "queued" flag set, so magnification stopped until the dock restarted.
    static DEFERRED: RefCell<Vec<(HWND, u32, WPARAM, LPARAM)>> = const { RefCell::new(Vec::new()) };
}

/// `with_dock` for posted work messages: while the dock is busy the message is kept (once) and posted again when the
/// borrow ends, instead of being dropped.
fn with_dock_or_defer<R>(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    f: impl FnOnce(&mut Dock) -> R,
) -> Option<R> {
    let result = with_dock(f);
    if result.is_none() && DOCK.with(|slot| slot.try_borrow().is_err()) {
        DEFERRED.with(|list| {
            let mut list = list.borrow_mut();
            let entry = (hwnd, message, wparam, lparam);
            if !list.contains(&entry) {
                list.push(entry);
            }
        });
    }
    result
}

fn repost_deferred() {
    let pending = DEFERRED.with(|list| std::mem::take(&mut *list.borrow_mut()));
    for (hwnd, message, wparam, lparam) in pending {
        // SAFETY: posting to our own window; a destroyed window just drops it.
        unsafe { wm::PostMessageW(hwnd, message, wparam, lparam) };
    }
}

impl Dock {
    fn new(hwnd: Hwnd, settings: DockSettings) -> WinResult<Self> {
        // `ThemePacks.InstallBuiltInThemes` + `Select`: before any color is read.
        if let Err(key) = ot_core::theme_pack::install_built_in() {
            log::warn(format!("Theme packs: {key}"));
        }
        crate::tokens::select(settings.theme_pack.as_deref(), true);
        // Lock and unlock notifications (`WM_WTSSESSION_CHANGE`); unregistered in WM_DESTROY.
        // SAFETY: our own window; NOTIFY_FOR_THIS_SESSION.
        unsafe {
            windows_sys::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(
                hwnd as HWND,
                0,
            )
        };
        let renderer = Renderer::new(hwnd)?;
        memory_mark("after renderer");
        let profile = SizeProfile::for_size(
            settings.size,
            settings.dock_scale,
            settings.dock_icon_padding,
        )
        .with_border(
            settings.dock_border_thickness,
            settings.dock_background != ot_core::settings::SurfaceBackground::None,
        );
        let colors = theme::dock_colors(
            theme::tone(settings.theme),
            settings.dock_background,
            settings.dock_opacity,
        );
        let visuals = Visuals::new(&renderer, &colors)?;
        let target = hwnd;
        let icons = IconLoader::new(move || {
            // SAFETY: posting to our own window from the icon thread.
            unsafe { wm::PostMessageW(target as HWND, WM_ICONS, 0, 0) };
        });
        let pins = pins::load(settings.pin_order.as_deref());
        memory_mark("after pins");
        log::info(format!("Dock: {} pinned apps.", pins.len()));
        let auto_hide = settings.auto_hide != ot_core::settings::AutoHideMode::Disabled;
        let session = Session::new(hwnd, auto_hide, move |event| match event {
            SessionEvent::OperationReported(key, error) => {
                log::warn(format!("Session reported {key} (error={error})"))
            }
            SessionEvent::FullScreenChanged(suppressed) => {
                // Raised inside session code (possibly while the dock is borrowed): handled as a message.
                // SAFETY: posting to our own window.
                unsafe {
                    wm::PostMessageW(target as HWND, WM_FULL_SCREEN, usize::from(suppressed), 0)
                };
            }
            SessionEvent::LayoutChanged { .. } => {}
        });
        Ok(Self {
            hwnd,
            session,
            renderer,
            tracker: None,
            icons,
            settings,
            profile,
            colors,
            pins,
            items: Vec::new(),
            foreground: 0,
            foreground_at_press: None,
            foreground_before_click: None,
            visuals,
            layout: None,
            placement: None,
            corner_radius: 0.0,
            uia: UiaModel::new(hwnd, WM_INVOKE),
            bar_rect: (0, 0, 0, 0),
            magnifier: Magnifier::default(),
            frames: FrameClock::new(hwnd, WM_FRAME),
            tracking_mouse: false,
            last_mouse: None,
            hover_icons_requested: false,
            slots: Vec::new(),
            icon_rects: Vec::new(),
            applied: Vec::new(),
            overlap_stats: (0, 0, 0),
            autohide: AutoHide::default(),
            menu_target: None,
            press: None,
            drag: None,
            rebuild_pending: false,
            removal_pending: false,
            removal_faded: false,
            names: HashMap::new(),
            tip_target: None,
            tip_shown: false,
            tip_closed_at: None,
            tip_suppressed: None,
            preview_item: None,
            preview_pending: None,
            preview_key: None,
            recycle_bin_name: None,
            jump_lists: JumpLists::new(move || {
                // SAFETY: posting to our own window from the jump list thread.
                unsafe { wm::PostMessageW(target as HWND, WM_JUMPLIST, 0, 0) };
            }),
            hotkeys: (None, None),
            keyboard: None,
            focus_visual: None,
            last_selected: HashMap::new(),
            wheel_remainder: 0,
            jump_session: None,
            jump_loading: false,
            tray_pins: Vec::new(),
            shell_hovered: None,
            widgets: crate::widgets::Widgets::default(),
            widget_painter: None,
            widget_widths: HashMap::new(),
            widget_generation: HashMap::new(),
            widget_relayout_pending: false,
            menu_widget: None,
            tray_apps: None,
            hide_timer: false,
            guard_timer: false,
            strip_region: false,
        })
    }

    /// Live settings change: material, size, edge and buttons are rebuilt; pins and running apps are kept.
    fn apply(&mut self, settings: DockSettings) {
        self.preview_hide();
        tray_flyout::close();
        crate::panel::close();
        self.keyboard_end(false);
        crate::tokens::select(settings.theme_pack.as_deref(), false);
        let profile = SizeProfile::for_size(
            settings.size,
            settings.dock_scale,
            settings.dock_icon_padding,
        )
        .with_border(
            settings.dock_border_thickness,
            settings.dock_background != ot_core::settings::SurfaceBackground::None,
        );
        let colors = theme::dock_colors(
            theme::tone(settings.theme),
            settings.dock_background,
            settings.dock_opacity,
        );
        let rebuilt = Visuals::new(&self.renderer, &colors);
        match rebuilt {
            Ok(visuals) => {
                if let Ok(children) = self.renderer.root.Children() {
                    let _ = children.Remove(&self.visuals.bar);
                }
                self.visuals = visuals;
            }
            Err(error) => {
                log::warn(format!("Dock restyle failed: {error}"));
                return;
            }
        }
        let gap = f64::from(settings.dock_bottom_gap_pixels.max(0));
        let slack = if settings.edge == DockEdge::Bottom {
            2.0
        } else {
            16.0
        };
        self.session
            .set_shelf_height(profile.shelf.max(profile.surface_height() + gap + slack));
        self.session
            .set_auto_hide(settings.auto_hide != ot_core::settings::AutoHideMode::Disabled);
        self.session.set_edge(match settings.edge {
            DockEdge::Bottom => session::Edge::Bottom,
            DockEdge::Left => session::Edge::Left,
            DockEdge::Right => session::Edge::Right,
        });
        self.profile = profile;
        self.colors = colors;
        self.settings = settings;
        // Pins, the tray button or the language may have changed.
        self.tray_pins = self.collect_tray_pins();
        self.sync_widgets();
        self.items.clear();
        self.layout = None;
        self.rebuild_items();
        self.relayout();
        let mode = self.settings.auto_hide;
        self.set_auto_hide_mode(mode);
        self.register_hotkeys();
    }

    fn start(&mut self) -> std::result::Result<(), &'static str> {
        self.sync_widgets();
        let gap = f64::from(self.settings.dock_bottom_gap_pixels.max(0));
        let slack = if self.settings.edge == DockEdge::Bottom {
            2.0
        } else {
            16.0
        };
        self.session.set_shelf_height(
            self.profile
                .shelf
                .max(self.profile.surface_height() + gap + slack),
        );
        self.session.set_edge(match self.settings.edge {
            DockEdge::Bottom => session::Edge::Bottom,
            DockEdge::Left => session::Edge::Left,
            DockEdge::Right => session::Edge::Right,
        });
        self.session.start()?;
        preview::exclude_from_peek(self.hwnd);
        self.register_hotkeys();
        // `EnsureTrayHostStarted`: background apps' icons come to the dock while Explorer's taskbar is hidden.
        let owner = self.hwnd;
        // `OT_TRAY_DISABLED=1` (tests): no tray host, as when it cannot start; the flyout falls back to the
        // registry list.
        let tray_disabled = std::env::var_os("OT_TRAY_DISABLED").is_some_and(|v| v == "1");
        if !tray_disabled {
            ot_shell::tray::start(move || {
                // SAFETY: posting to our own window.
                unsafe { wm::PostMessageW(owner as HWND, WM_TRAY_CHANGED, 0, 0) };
            });
        }
        let owner = self.hwnd as usize;
        self.tray_apps = crate::trayapps::Source::start(move || {
            // SAFETY: posting to our own window from the registry worker.
            unsafe { wm::PostMessageW(owner as HWND, WM_TRAY_CHANGED, 0, 0) };
        });
        memory_mark("after session");
        let target = self.hwnd;
        let tracker = WindowTracker::start(self.hwnd, move |event| {
            let wparam = match event {
                TrackerEvent::WindowsChanged(_) => 0,
                TrackerEvent::ForegroundChanged(_) => 1,
                TrackerEvent::TaskbarShown { recreated } => {
                    // SAFETY: posting to our own window from the tracker thread; the session handles it.
                    unsafe {
                        wm::PostMessageW(
                            target as HWND,
                            session::TASKBAR_SHOWN_MESSAGE,
                            usize::from(recreated),
                            0,
                        )
                    };
                    return;
                }
            };
            // SAFETY: posting to our own window from the tracker thread.
            unsafe { wm::PostMessageW(target as HWND, WM_TRACKER, wparam, 0) };
        });
        match tracker {
            Ok(tracker) => self.tracker = Some(tracker),
            Err(key) => log::warn(format!("Window tracker unavailable: {key}")),
        }
        memory_mark("after tracker");
        self.foreground = tracker::foreground_application_window().unwrap_or(0);
        self.rebuild_items();
        // SWP_SHOWWINDOW instead of ShowWindow: the first ShowWindow of a process follows the launcher's
        // STARTUPINFO, so a hidden launch would keep the dock hidden.
        // SAFETY: our own window.
        unsafe {
            wm::SetWindowPos(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                wm::SWP_NOMOVE
                    | wm::SWP_NOSIZE
                    | wm::SWP_NOZORDER
                    | wm::SWP_NOACTIVATE
                    | wm::SWP_SHOWWINDOW,
            )
        };
        self.note_foreground(ot_shell::win::foreground());
        let mode = self.settings.auto_hide;
        self.set_auto_hide_mode(mode);
        Ok(())
    }

    fn scale(&self) -> f64 {
        // SAFETY: plain Win32 call.
        let dpi = unsafe { GetDpiForWindow(self.hwnd as HWND) };
        if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
    }

    fn rebuild_items(&mut self) {
        let snapshot: Arc<Vec<TrackedWindow>> = self
            .tracker
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default();
        let s = &self.settings;
        let mut items: Vec<Item> = Vec::new();
        let shell = |kind| Item {
            kind,
            group: Group::Shell,
            windows: Vec::new(),
            foreground: false,
            notify: false,
        };
        // `ApplyPinnedGroups`: Start (unless hidden), Search and Task View lead the strip.
        if s.start_placement != StartButtonPlacement::Hidden {
            items.push(shell(ItemKind::Start));
        }
        if s.show_search_button {
            items.push(shell(ItemKind::Search));
        }
        if s.show_task_view_button {
            items.push(shell(ItemKind::TaskView));
        }
        // `DefaultDockOrder`: Show Desktop ("peek") right after the shell group, before the apps.
        if s.show_desktop_button {
            items.push(Item {
                kind: ItemKind::ShowDesktop,
                group: Group::Apps,
                windows: Vec::new(),
                foreground: false,
                notify: false,
            });
        }
        for pin in &self.pins {
            items.push(Item {
                kind: ItemKind::App {
                    key: pin.executable_path.to_lowercase(),
                    pin: Some(Box::new(pin.clone())),
                    executable_path: pin.executable_path.clone(),
                    app_id: None,
                },
                group: Group::Apps,
                windows: Vec::new(),
                foreground: false,
                notify: false,
            });
        }
        for window in snapshot.iter() {
            let Some(path) = window.executable_path.as_deref() else {
                continue;
            };
            let packaged = window
                .app_user_model_id
                .as_deref()
                .filter(|id| id.contains('!'));
            let path_key = path.to_lowercase();
            let id_key = packaged.map(str::to_lowercase);
            let existing = items.iter_mut().find(|item| match &item.kind {
                ItemKind::App { key, .. } => {
                    *key == path_key || id_key.as_deref() == Some(key.as_str())
                }
                _ => false,
            });
            match existing {
                Some(item) => {
                    item.windows.push(window.handle);
                    item.notify |= window.has_notification;
                }
                None => items.push(Item {
                    kind: ItemKind::App {
                        key: id_key.unwrap_or(path_key),
                        pin: None,
                        executable_path: path.to_owned(),
                        app_id: packaged.map(str::to_owned),
                    },
                    group: Group::Apps,
                    windows: vec![window.handle],
                    foreground: false,
                    notify: window.has_notification,
                }),
            }
        }
        // `RefreshRunningApps`: running apps keep the place they had; new ones go after them, by name. The window
        // list is in Z order, which changes with every activation, so it must not decide the item order.
        let pinned_end = items
            .iter()
            .position(|item| matches!(item.kind, ItemKind::App { pin: None, .. }))
            .unwrap_or(items.len());
        let mut running = items.split_off(pinned_end);
        for item in &running {
            if let ItemKind::App {
                key,
                executable_path,
                app_id,
                ..
            } = &item.kind
                && !self.names.contains_key(key)
            {
                let name = crate::app_name::display_name(executable_path, app_id.as_deref());
                self.names.insert(key.clone(), name);
            }
        }
        let previous: HashMap<String, usize> = self
            .items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::App { pin: None, .. }))
            .enumerate()
            .map(|(index, item)| (item_key(item), index))
            .collect();
        let names = &self.names;
        running.sort_by_cached_key(|item| {
            let key = item_key(item);
            (
                previous.get(&key).copied().unwrap_or(usize::MAX),
                names
                    .get(&key)
                    .map(|n| n.to_lowercase())
                    .unwrap_or_default(),
            )
        });
        items.extend(running);
        // `DefaultDockOrder`: the widgets follow the apps; `ApplyPinnedGroups` keeps the tray and the bin last.
        for id in self.widgets.visible_ids() {
            items.push(Item {
                kind: ItemKind::Widget(id),
                group: Group::Apps,
                windows: Vec::new(),
                foreground: false,
                notify: false,
            });
        }
        // `ApplyPinnedGroups`: the background-apps button and the Recycle Bin trail the strip, the bin last.
        if s.show_tray_button {
            for pin in &self.tray_pins {
                items.push(Item {
                    kind: ItemKind::TrayPin(Box::new(pin.clone())),
                    group: Group::Tray,
                    windows: Vec::new(),
                    foreground: false,
                    notify: false,
                });
            }
            items.push(Item {
                kind: ItemKind::Tray,
                group: Group::Tray,
                windows: Vec::new(),
                foreground: false,
                notify: false,
            });
        }
        if s.show_recycle_bin {
            items.push(Item {
                kind: ItemKind::RecycleBin,
                group: Group::End,
                windows: Vec::new(),
                foreground: false,
                notify: false,
            });
        }
        let foreground = self.foreground;
        for item in &mut items {
            item.foreground = foreground != 0 && item.windows.contains(&foreground);
        }
        self.last_selected
            .retain(|_, window| snapshot.iter().any(|w| w.handle == *window));
        for item in &items {
            if let ItemKind::App {
                key,
                pin: None,
                executable_path,
                app_id,
            } = &item.kind
                && !self.names.contains_key(key)
            {
                let name = crate::app_name::display_name(executable_path, app_id.as_deref());
                self.names.insert(key.clone(), name);
            }
        }
        let structure_changed = items.len() != self.items.len()
            || items.iter().zip(&self.items).any(|(a, b)| a.kind != b.kind);
        if !structure_changed {
            self.items = items;
            self.update_indicators();
            return;
        }
        if self.removal_pending {
            // The fade timer applies the latest window list.
            return;
        }
        let animate = self.animations_allowed() && self.layout.is_some();
        let new_keys: Vec<String> = items.iter().map(item_key).collect();
        let leaving: Vec<usize> = (0..self.items.len())
            .filter(|&i| !new_keys.contains(&item_key(&self.items[i])))
            .collect();
        if animate && !leaving.is_empty() && !self.removal_faded {
            // `AnimateDockAppRemoval`: 75 ms fade to 0 and scale to 0.95, then the dock closes the gap.
            for index in leaving {
                self.fade_out(index);
            }
            self.removal_pending = true;
            // SAFETY: timer on our own window.
            unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_REMOVE, REMOVE_MS, None) };
            return;
        }
        let before = animate.then(|| {
            (
                self.items.iter().map(item_key).collect::<Vec<_>>(),
                self.snapshot(),
            )
        });
        self.items = items;
        self.relayout();
        if let Some((old_keys, before)) = before {
            let after = self.snapshot();
            let mapping: Vec<Option<usize>> = new_keys
                .iter()
                .map(|key| old_keys.iter().position(|k| k == key))
                .collect();
            // The dock width animates in 120 ms (`ResizeDockToContent`); items glide with it.
            self.ease_between(&before, &after, &mapping, None, RESIZE_MS, true);
        }
    }

    fn removal_elapsed(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_REMOVE) };
        if std::mem::take(&mut self.removal_pending) {
            // The leaving apps already faded: this rebuild applies the list and closes the gap.
            self.removal_faded = true;
            self.rebuild_items();
            self.removal_faded = false;
        }
    }

    fn fade_out(&self, index: usize) {
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let c = &self.renderer.compositor;
        let result: WinResult<()> = (|| {
            let size = visual.container.Size()?;
            visual.container.SetCenterPoint(Vector3 {
                X: size.X / 2.0,
                Y: size.Y / 2.0,
                Z: 0.0,
            })?;
            let ease = ease_out(c)?;
            let fade = c.CreateScalarKeyFrameAnimation()?;
            fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, &ease)?;
            fade.SetDuration(milliseconds(REMOVE_MS))?;
            visual
                .container
                .StartAnimation(&HSTRING::from("Opacity"), &fade)?;
            let shrink = c.CreateVector3KeyFrameAnimation()?;
            shrink.InsertKeyFrameWithEasingFunction(
                1.0,
                Vector3 {
                    X: 0.95,
                    Y: 0.95,
                    Z: 1.0,
                },
                &ease,
            )?;
            shrink.SetDuration(milliseconds(REMOVE_MS))?;
            visual
                .container
                .StartAnimation(&HSTRING::from("Scale"), &shrink)
        })();
        if let Err(error) = result {
            log::warn(format!("Dock removal animation failed: {error}"));
        }
    }

    /// Launch feedback (`AnimateActivationFeedback`): 420 ms, 12 DIP away from the edge and back, then a 5 DIP
    /// rebound.
    fn bounce(&self, index: usize) {
        if !self.animations_allowed() {
            return;
        }
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let scale = self.scale() as f32;
        let (property, direction) = match self.settings.edge {
            DockEdge::Bottom => ("Offset.Y", -1.0f32),
            DockEdge::Left => ("Offset.X", 1.0),
            DockEdge::Right => ("Offset.X", -1.0),
        };
        let c = &self.renderer.compositor;
        let result: WinResult<()> = (|| {
            // Relative to wherever the icon is (at rest or magnified): the bounce is an extra translation.
            let base = 0.0f32;
            let out = ease_out(c)?;
            let ease_in = c.CreateCubicBezierEasingFunction(
                Vector2 { X: 0.32, Y: 0.0 },
                Vector2 { X: 0.67, Y: 0.0 },
            )?;
            let total = BOUNCE_MS as f32;
            let a = c.CreateScalarKeyFrameAnimation()?;
            a.InsertKeyFrame(0.0, base)?;
            a.InsertKeyFrameWithEasingFunction(
                120.0 / total,
                base + direction * 12.0 * scale,
                &out,
            )?;
            a.InsertKeyFrameWithEasingFunction(240.0 / total, base, &ease_in)?;
            a.InsertKeyFrameWithEasingFunction(
                330.0 / total,
                base + direction * 5.0 * scale,
                &out,
            )?;
            a.InsertKeyFrameWithEasingFunction(1.0, base, &ease_in)?;
            a.SetDuration(milliseconds(BOUNCE_MS))?;
            visual.bounce.StartAnimation(&HSTRING::from(property), &a)
        })();
        if let Err(error) = result {
            log::warn(format!("Dock bounce failed: {error}"));
        }
    }

    /// Shift+click or middle click: a new window; Ctrl+Shift+click: elevated (`LaunchApp`).
    fn launch_new(&self, index: usize, elevated: bool) {
        let Some(Item {
            kind:
                ItemKind::App {
                    pin,
                    executable_path,
                    ..
                },
            ..
        }) = self.items.get(index)
        else {
            return;
        };
        match (pin, elevated) {
            (Some(pin), false) => actions::launch_pin(pin),
            (None, false) => actions::launch_executable(executable_path),
            (Some(pin), true) => actions::launch_elevated(&pin.shortcut_path.to_string_lossy()),
            (None, true) => actions::launch_elevated(executable_path),
        }
    }

    fn client_size(&self) -> (i32, i32) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: our own window.
        unsafe { wm::GetClientRect(self.hwnd as HWND, &mut rect) };
        (rect.right - rect.left, rect.bottom - rect.top)
    }

    fn bar_spec(&self) -> BarSpec {
        let s = &self.settings;
        let fixed_length = match s.dock_width_mode {
            DockWidthMode::Pixels => Some(f64::from(s.dock_width_pixels)),
            DockWidthMode::Percent => {
                let (w, h) = self.client_size();
                let main = if s.edge == DockEdge::Bottom { w } else { h };
                Some(f64::from(main) / self.scale() * f64::from(s.dock_width_percent) / 100.0)
            }
            DockWidthMode::Auto | DockWidthMode::Fill => None,
        };
        BarSpec {
            edge: s.edge,
            position: s.position,
            full_width: s.layout == DockLayout::FullWidth,
            fill: s.dock_width_mode == DockWidthMode::Fill,
            fixed_length,
            content_alignment: s.content_alignment,
            gap: f64::from(s.dock_bottom_gap_pixels.max(0)),
        }
    }

    fn relayout(&mut self) {
        if let Some(nav) = self.keyboard.as_mut() {
            nav.focused = nav.focused.min(self.items.len().saturating_sub(1));
        }
        self.tip_hide();
        self.tip_target = None;
        self.preview_item = None;
        self.preview_pending = None;
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_OPEN) };
        self.end_magnification_now();
        self.magnifier.invalidate();
        let slots: Vec<Slot> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| match item.kind {
                ItemKind::ShowDesktop => Slot::show_desktop(item.group),
                // `WidgetCardButton`: the card, 3 DIP of `WidgetSpacing` on each side.
                ItemKind::Widget(id) => Slot {
                    group: item.group,
                    size: f64::from(self.widget_widths.get(id).copied().unwrap_or(80.0)),
                    margin: crate::widgets::SPACING,
                    after: crate::widgets::SPACING,
                },
                ItemKind::TrayPin(_) => Slot::tray_pin(
                    &self.profile,
                    !matches!(
                        self.items.get(index + 1).map(|i| &i.kind),
                        Some(ItemKind::TrayPin(_))
                    ),
                ),
                _ => Slot::button(&self.profile, item.group),
            })
            .collect();
        let layout = layout_bar(&self.profile, &slots);
        let scale = self.scale();
        let (window_w, window_h) = self.client_size();
        if window_w <= 0 || window_h <= 0 {
            return;
        }
        let spec = self.bar_spec();
        let place = spec.place(
            (window_w as f32, window_h as f32),
            layout.width,
            layout.height,
            scale,
        );
        let thickness = place.width.min(place.height);
        let radius = if place.square_corners {
            0.0
        } else {
            ((f64::from(
                self.settings
                    .dock_corner_radius
                    .unwrap_or(self.profile.radius as i32),
            ) * scale)
                .round() as f32)
                .min(thickness / 2.0)
        };
        let v = &mut self.visuals;
        let size = Vector2 {
            X: place.width,
            Y: place.height,
        };
        let result: WinResult<()> = (|| {
            v.bar.SetOffset(Vector3 {
                X: place.x,
                Y: place.y,
                Z: 0.0,
            })?;
            v.bar.SetSize(size)?;
            v.shape.SetSize(size)?;
            // The rim is drawn inside the bounds, as a WPF Border's.
            let rim = (self.profile.border * scale) as f32;
            v.fill.SetStrokeThickness(rim)?;
            v.geometry.SetOffset(Vector2 {
                X: rim / 2.0,
                Y: rim / 2.0,
            })?;
            v.geometry.SetSize(Vector2 {
                X: place.width - rim,
                Y: place.height - rim,
            })?;
            v.geometry.SetCornerRadius(Vector2 {
                X: radius,
                Y: radius,
            })?;
            v.shadow.SetSize(size)?;
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Dock layout failed: {error}"));
        }
        self.bar_rect = (
            place.x as i32,
            place.y as i32,
            (place.x + place.width) as i32,
            (place.y + place.height) as i32,
        );
        self.corner_radius = radius;
        self.placement = Some(place);
        self.layout = Some(layout);
        self.sync_item_visuals();
        self.sync_shell_surfaces();
        self.update_region();
        if self.keyboard.is_some() {
            self.show_focus();
        }
    }

    /// Input region: the bar plus its shadow, like OpenTaskbar's per-pixel hit testing (any visible pixel takes
    /// clicks); everything else in the full-width window passes clicks through.
    fn update_region(&self) {
        if self.strip_region {
            self.update_strip_region();
            return;
        }
        if self.magnifier.engaged {
            self.update_magnified_region();
            return;
        }
        let scale = self.scale();
        let margin = if self.colors.shadow {
            (f64::from(SHADOW_BLUR_DIPS) * scale * 0.5).round() as i32
        } else {
            0
        };
        let (l, t, r, b) = self.bar_rect;
        let (w, h) = self.client_size();
        let radius = (self.corner_radius as i32) * 2;
        // SAFETY: the region is owned by the window after SetWindowRgn.
        unsafe {
            let region = CreateRoundRectRgn(
                (l - margin).max(0),
                (t - margin).max(0),
                (r + margin).min(w) + 1,
                (b + margin).min(h) + 1,
                radius,
                radius,
            );
            SetWindowRgn(self.hwnd as HWND, region, 1);
        }
    }

    fn vertical(&self) -> bool {
        self.settings.edge != DockEdge::Bottom
    }

    /// Converts a (main axis, cross axis) position inside the bar to (x, y).
    fn axis(&self, main: f32, cross: f32) -> (f32, f32) {
        if self.vertical() {
            (cross, main)
        } else {
            (main, cross)
        }
    }

    fn sync_item_visuals(&mut self) {
        let (Some(layout), Some(place)) = (self.layout.clone(), self.placement) else {
            return;
        };
        let scale = self.scale();
        let px = |dips: f64| (dips * scale).round() as f32;
        let compositor = self.renderer.compositor.clone();
        let vertical = self.vertical();
        let badge_brush = self.badge_brush();
        let result: WinResult<()> = (|| {
            while self.visuals.items.len() < self.items.len() {
                let container = compositor.CreateContainerVisual()?;
                let icon = compositor.CreateSpriteVisual()?;
                let indicator = compositor.CreateSpriteVisual()?;
                let badge = compositor.CreateSpriteVisual()?;
                badge.SetIsVisible(false)?;
                icon.Children()?.InsertAtTop(&badge)?;
                // The click bounce moves this wrapper; magnification frames keep writing the icon's own offset
                // inside it, so the two never overwrite each other.
                let bounce = compositor.CreateContainerVisual()?;
                bounce.Children()?.InsertAtTop(&icon)?;
                container.Children()?.InsertAtTop(&bounce)?;
                container.Children()?.InsertAtTop(&indicator)?;
                self.visuals.bar.Children()?.InsertAtTop(&container)?;
                self.visuals.items.push(ItemVisual {
                    container,
                    bounce,
                    icon,
                    indicator,
                    icon_key: None,
                    rest_brush: None,
                    hover_brush: None,
                    hover_key: None,
                    badge,
                    shell: None,
                });
            }
            while self.visuals.items.len() > self.items.len() {
                if let Some(visual) = self.visuals.items.pop() {
                    self.visuals.bar.Children()?.Remove(&visual.container)?;
                }
            }
            let cell_cross = px(self.profile.cell_height);
            let icon = application_icon_pixels(self.profile.icon, scale) as f32;
            let pad = px(self.profile.vertical_padding + self.profile.border);
            let row = self.indicator_row_px();
            for (index, main) in layout.cells.iter().enumerate() {
                let cell = px(layout
                    .sizes
                    .get(index)
                    .copied()
                    .unwrap_or(self.profile.cell));
                let container_size = if vertical {
                    Vector2 {
                        X: cell_cross,
                        Y: cell,
                    }
                } else {
                    Vector2 {
                        X: cell,
                        Y: cell_cross,
                    }
                };
                let (x, y) = self.axis(place.content_offset + px(*main), pad);
                let visual = &self.visuals.items[index];
                visual.container.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
                visual.container.SetSize(container_size)?;
                visual.container.SetOpacity(1.0)?;
                visual.container.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                })?;
                if matches!(self.items[index].kind, ItemKind::Widget(_)) {
                    // The card is the whole slot, centered across the bar (`VerticalAlignment="Center"`).
                    let card = px(crate::widgets::card_height(self.profile.cell_height));
                    let inset = ((cell_cross - card) / 2.0).round();
                    let (w, h) = if vertical { (card, cell) } else { (cell, card) };
                    visual.icon.SetSize(Vector2 { X: w, Y: h })?;
                    visual.icon.SetScale(Vector3 {
                        X: 1.0,
                        Y: 1.0,
                        Z: 1.0,
                    })?;
                    let (ox, oy) = if vertical { (inset, 0.0) } else { (0.0, inset) };
                    visual.icon.SetOffset(Vector3 {
                        X: ox,
                        Y: oy,
                        Z: 0.0,
                    })?;
                    visual.badge.SetIsVisible(false)?;
                    continue;
                }
                visual.icon.SetSize(Vector2 { X: icon, Y: icon })?;
                // `NotificationBrush` ellipse, 9 DIP, top-right of the icon box.
                let dot = px(9.0);
                visual.badge.SetSize(Vector2 { X: dot, Y: dot })?;
                visual.badge.SetOffset(Vector3 {
                    X: icon - dot,
                    Y: 0.0,
                    Z: 0.0,
                })?;
                if let Some(brush) = &badge_brush {
                    visual.badge.SetBrush(brush)?;
                }
                visual.icon.SetScale(Vector3 {
                    X: 1.0,
                    Y: 1.0,
                    Z: 1.0,
                })?;
                // `DockButton` keeps a 6 DIP indicator row under the content on a bottom dock.
                let below = if has_indicator_row(&self.items[index].kind) {
                    row
                } else {
                    0.0
                };
                visual.icon.SetOffset(Vector3 {
                    X: ((container_size.X - icon) / 2.0).round(),
                    Y: ((container_size.Y - below - icon) / 2.0).round(),
                    Z: 0.0,
                })?;
            }
            while self.visuals.separators.len() < layout.separators.len() {
                let sprite = compositor.CreateSpriteVisual()?;
                sprite.SetBrush(&self.visuals.separator_brush)?;
                self.visuals.bar.Children()?.InsertAtTop(&sprite)?;
                self.visuals.separators.push(sprite);
            }
            while self.visuals.separators.len() > layout.separators.len() {
                if let Some(sprite) = self.visuals.separators.pop() {
                    self.visuals.bar.Children()?.Remove(&sprite)?;
                }
            }
            let line = px(crate::layout::DIVIDER_HEIGHT).min(cell_cross);
            let hairline = px(crate::layout::DIVIDER_WIDTH).max(1.0);
            for (sprite, main) in self.visuals.separators.iter().zip(&layout.separators) {
                let (x, y) = self.axis(
                    place.content_offset + px(*main),
                    pad + ((cell_cross - line) / 2.0).round(),
                );
                sprite.SetSize(if vertical {
                    Vector2 {
                        X: line,
                        Y: hairline,
                    }
                } else {
                    Vector2 {
                        X: hairline,
                        Y: line,
                    }
                })?;
                sprite.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Dock item visuals failed: {error}"));
        }
        self.update_icons();
        self.update_indicators();
        self.publish_uia();
    }

    /// Refreshes the UI Automation model: names, AutomationIds and screen rectangles of the items.
    fn publish_uia(&self) {
        let (Some(layout), Some(place)) = (self.layout.as_ref(), self.placement) else {
            return;
        };
        let scale = self.scale();
        let px = |dips: f64| (dips * scale).round() as i32;
        let mut origin = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: our own window.
        unsafe { wm::GetWindowRect(self.hwnd as HWND, &mut origin) };
        let cross = px(self.profile.cell_height);
        let pad = px(self.profile.vertical_padding + self.profile.border);
        let items = self
            .items
            .iter()
            .zip(&layout.cells)
            .enumerate()
            .map(|(index, (item, main))| {
                let cell = px(layout
                    .sizes
                    .get(index)
                    .copied()
                    .unwrap_or(self.profile.cell));
                let main = place.content_offset as i32 + px(*main);
                let (x, y, w, h) = if self.vertical() {
                    (place.x as i32 + pad, place.y as i32 + main, cross, cell)
                } else {
                    (place.x as i32 + main, place.y as i32 + pad, cell, cross)
                };
                let automation_id = match &item.kind {
                    ItemKind::Start => "StartButton",
                    ItemKind::Search => "SearchButton",
                    ItemKind::TaskView => "TaskViewButton",
                    ItemKind::ShowDesktop => "ShowDesktopButton",
                    ItemKind::Tray => "TrayButton",
                    ItemKind::RecycleBin => "RecycleBinButton",
                    ItemKind::App { .. } | ItemKind::TrayPin(_) | ItemKind::Widget(_) => "",
                };
                let name = match self.items[index].kind {
                    ItemKind::Widget(id) => {
                        self.widgets.get(id).map(|w| w.name()).unwrap_or_default()
                    }
                    _ => self.item_name(index),
                };
                UiaItem {
                    name,
                    automation_id: automation_id.to_owned(),
                    rect: (
                        origin.left + x,
                        origin.top + y,
                        origin.left + x + w,
                        origin.top + y + h,
                    ),
                }
            })
            .collect();
        self.uia.set_items(items);
    }

    fn icon_source(&self, index: usize) -> Option<(String, u32)> {
        let size = application_icon_pixels(self.profile.icon, self.scale());
        match &self.items[index].kind {
            // A pin's own shortcut carries its icon (Store apps and browser web apps have no usable exe icon),
            // as OpenTaskbar's GetForPinnedApp uses the shortcut's icon location.
            ItemKind::App { pin: Some(pin), .. } => Some((
                match &pin.app_user_model_id {
                    Some(id) => format!(r"shell:AppsFolder\{id}"),
                    None => pin.shortcut_path.to_string_lossy().into_owned(),
                },
                size,
            )),
            ItemKind::App {
                executable_path,
                app_id,
                ..
            } => Some((
                icons::parsing_name(executable_path, app_id.as_deref()),
                size,
            )),
            ItemKind::RecycleBin => Some((icons::RECYCLE_BIN.to_owned(), size)),
            _ => None,
        }
    }

    fn update_icons(&mut self) {
        let scale = self.scale();
        for index in 0..self.items.len() {
            if let ItemKind::Widget(id) = self.items[index].kind {
                if let Err(error) = self.draw_widget(index, id) {
                    log::warn(format!("Widget {id} drawing failed: {error}"));
                }
                continue;
            }
            if let ItemKind::TrayPin(pin) = &self.items[index].kind {
                let key = (format!("traypin:{:016x}", pin.icon_hash), 0);
                if self.visuals.items[index].icon_key.as_ref() == Some(&key) {
                    continue;
                }
                let icon = pin.icon.clone();
                let result = self
                    .tray_pin_brush(icon.as_deref())
                    .and_then(|brush| self.set_fixed_brush(index, Some(brush), key));
                if let Err(error) = result {
                    log::warn(format!("Pinned tray icon failed: {error}"));
                }
                continue;
            }
            let result: WinResult<()> = match self.icon_source(index) {
                Some(key) => {
                    if self.visuals.items[index].icon_key.as_ref() == Some(&key) {
                        continue;
                    }
                    match self.icons.get(&key.0, key.1) {
                        Some(icon) => self
                            .renderer
                            .bitmap_brush(icon.width, icon.height, &icon.pixels, key.1)
                            .and_then(|brush| {
                                let visual = &mut self.visuals.items[index];
                                if !self.magnifier.hover.get(index).copied().unwrap_or(false)
                                    || visual.hover_brush.is_none()
                                {
                                    visual.icon.SetBrush(&brush)?;
                                }
                                visual.rest_brush = Some(brush);
                                visual.icon_key = Some(key);
                                Ok(())
                            }),
                        None => Ok(()),
                    }
                }
                None => {
                    let glyph = match self.items[index].kind {
                        ItemKind::Start => "start",
                        ItemKind::Search => "\u{E721}",
                        ItemKind::TaskView => "\u{E7C4}",
                        // An empty strip, as in OpenTaskbar.
                        ItemKind::ShowDesktop => "",
                        // The chevron points where the flyout opens.
                        ItemKind::Tray => match self.settings.edge {
                            DockEdge::Bottom => "tray:ChevronUp",
                            DockEdge::Left => "tray:ChevronRight",
                            DockEdge::Right => "tray:ChevronLeft",
                        },
                        _ => continue,
                    };
                    let key = (glyph.to_owned(), 0);
                    if self.visuals.items[index].icon_key.as_ref() == Some(&key) {
                        continue;
                    }
                    if glyph.is_empty() {
                        self.set_fixed_brush(index, None, key)
                    } else {
                        let size = application_icon_pixels(self.profile.icon, scale);
                        self.glyph(glyph, size)
                            .and_then(|brush| self.set_fixed_brush(index, Some(brush), key))
                    }
                }
            };
            if let Err(error) = result {
                log::warn(format!("Dock icon failed: {error}"));
            }
        }
    }

    /// Shows a non-magnifying item's image (none for Show Desktop's empty strip). Its visual may have belonged to
    /// an app before the items changed, so the app's resting and hover images are dropped too: ending a hover
    /// puts every item's resting image back, which showed the old app's icon on the tray button.
    fn set_fixed_brush(
        &mut self,
        index: usize,
        brush: Option<ot_render::Composition::CompositionSurfaceBrush>,
        key: (String, u32),
    ) -> WinResult<()> {
        let visual = &mut self.visuals.items[index];
        match &brush {
            Some(brush) => visual.icon.SetBrush(brush)?,
            None => visual
                .icon
                .SetBrush(None::<&ot_render::Composition::CompositionBrush>)?,
        }
        visual.rest_brush = brush;
        visual.hover_brush = None;
        visual.hover_key = None;
        visual.icon_key = Some(key);
        Ok(())
    }

    /// A pinned tray icon: the image (`tray_pin_image`) centered in the item's icon sprite, high-quality scaled.
    fn tray_pin_brush(
        &self,
        icon: Option<&crate::icons::Icon>,
    ) -> WinResult<ot_render::Composition::CompositionSurfaceBrush> {
        let scale = self.scale();
        let size = application_icon_pixels(self.profile.icon, scale);
        let image = (crate::layout::tray_pin_image(self.profile.icon) * scale).round() as f32;
        self.renderer.draw(size, size, |context| {
            let Some(icon) = icon.filter(|i| i.width > 0 && i.height > 0) else {
                return Ok(());
            };
            use windows::Win32::Graphics::Direct2D::Common::{
                D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
            };
            use windows::Win32::Graphics::Direct2D::{
                D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
            };
            use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
            // SAFETY: drawing inside BeginDraw/EndDraw; the pixels outlive the call.
            unsafe {
                let bitmap = context.CreateBitmap(
                    D2D_SIZE_U {
                        width: icon.width,
                        height: icon.height,
                    },
                    Some(icon.pixels.as_ptr().cast()),
                    icon.width * 4,
                    &D2D1_BITMAP_PROPERTIES1 {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                        colorContext: std::mem::ManuallyDrop::new(None),
                    },
                )?;
                // `Stretch.Uniform`.
                let fit = image / icon.width.max(icon.height) as f32;
                let (w, h) = (icon.width as f32 * fit, icon.height as f32 * fit);
                let (x, y) = ((size as f32 - w) / 2.0, (size as f32 - h) / 2.0);
                context.DrawBitmap(
                    &bitmap,
                    Some(&D2D_RECT_F {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    }),
                    1.0,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                );
            }
            Ok(())
        })
    }

    fn glyph(
        &mut self,
        glyph: &'static str,
        size: u32,
    ) -> WinResult<ot_render::Composition::CompositionSurfaceBrush> {
        if let Some(brush) = self.visuals.glyphs.get(glyph) {
            return Ok(brush.clone());
        }
        let color = self.colors.glyph;
        let brush = match glyph {
            "start" => windows_logo(&self.renderer, size)?,
            // `TrayShellButton`: the chevron in `WidgetIcon` style, `DockUtilityGlyphSize` (60 % of the icon),
            // `TextPrimary`, centered in the icon box.
            tray if tray.starts_with("tray:") => {
                let name = &tray[5..];
                let ink =
                    theme::text_primary(theme::tone(self.settings.theme) == theme::Tone::Dark);
                let glyph = ((self.profile.icon * 0.6).round() * self.scale()) as f32;
                self.renderer.draw(size, size, |context| {
                    // SAFETY: drawing inside BeginDraw/EndDraw.
                    unsafe {
                        let brush: windows::Win32::Graphics::Direct2D::ID2D1Brush =
                            context.CreateSolidColorBrush(&to_d2d(ink), None)?.cast()?;
                        let at = (size as f32 - glyph) / 2.0;
                        crate::vector_icon::draw_canvas(context, name, at, at, glyph, &brush)
                    }
                })?
            }
            "desktop" => {
                let (w, h) = (size, size);
                self.renderer.draw(w, h, |context| {
                    // SAFETY: drawing inside BeginDraw/EndDraw.
                    unsafe {
                        let brush = context.CreateSolidColorBrush(&to_d2d(color), None)?;
                        let x = w as f32 / 2.0;
                        let rect = D2D_RECT_F {
                            left: x - 1.0,
                            top: h as f32 * 0.25,
                            right: x + 1.0,
                            bottom: h as f32 * 0.75,
                        };
                        context.FillRectangle(&rect, &brush);
                    }
                    Ok(())
                })?
            }
            _ => self.renderer.glyph_brush(glyph, GLYPH_FONTS, size, color)?,
        };
        self.visuals.glyphs.insert(glyph, brush.clone());
        Ok(brush)
    }

    /// `DockIndicatorRowHeight`: 6 DIP on a bottom dock, none on a side dock.
    fn indicator_row_px(&self) -> f32 {
        if self.settings.edge == DockEdge::Bottom {
            (6.0 * self.scale()).round() as f32
        } else {
            0.0
        }
    }

    /// The running (4 DIP) and foreground (5 DIP) dots, drawn once per scale.
    fn indicator_dots(&mut self) -> WinResult<(CompositionSurfaceBrush, CompositionSurfaceBrush)> {
        let scale = self.scale() as f32;
        if let Some((at, running, front)) = &self.visuals.indicator_dots
            && *at == scale
        {
            return Ok((running.clone(), front.clone()));
        }
        let (running_color, front_color) = self.visuals.indicator_colors;
        // A white dot with a 1 DIP black ring around it (the ring is outside the dot's own size).
        let ring = indicator_ring_px(f64::from(scale));
        let dot = |dips: f64, fill: Color| -> WinResult<CompositionSurfaceBrush> {
            let inner = indicator_px(dips, f64::from(scale));
            let size = (inner + 2.0 * ring) as u32;
            self.renderer.draw(size, size, |context| {
                let c = size as f32 / 2.0;
                let ellipse = |r: f32| windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
                    point: Vector2 { X: c, Y: c },
                    radiusX: r,
                    radiusY: r,
                };
                // SAFETY: drawing inside BeginDraw/EndDraw.
                unsafe {
                    let outline = context
                        .CreateSolidColorBrush(&to_d2d(ot_render::color(0xFF, 0, 0, 0)), None)?;
                    let brush = context.CreateSolidColorBrush(&to_d2d(fill), None)?;
                    context.FillEllipse(&ellipse(c), &outline);
                    context.FillEllipse(&ellipse(inner / 2.0), &brush);
                }
                Ok(())
            })
        };
        let running = dot(4.0, running_color)?;
        let front = dot(5.0, front_color)?;
        self.visuals.indicator_dots = Some((scale, running.clone(), front.clone()));
        Ok((running, front))
    }

    fn update_indicators(&mut self) {
        let dots = match self.indicator_dots() {
            Ok(dots) => dots,
            Err(error) => {
                log::warn(format!("Dock indicators failed: {error}"));
                return;
            }
        };
        let scale = self.scale();
        let px = |dips: f64| (dips * scale).round() as f32;
        let cell = px(self.profile.cell);
        let cell_cross = px(self.profile.cell_height);
        let edge = self.settings.edge;
        let result: WinResult<()> = (|| {
            for (index, (item, visual)) in self.items.iter().zip(&self.visuals.items).enumerate() {
                visual.badge.SetIsVisible(item.notify)?;
                // Magnified slots are wider; the indicator stays centered under the icon.
                let cell = self.slots.get(index).map_or(cell, |slot| slot.1);
                let running = !item.windows.is_empty();
                visual.indicator.SetIsVisible(running)?;
                if !running {
                    continue;
                }
                let (size, x, y) =
                    indicator_geometry(edge, scale, cell, cell_cross, item.foreground);
                visual.indicator.SetSize(Vector2 { X: size, Y: size })?;
                visual.indicator.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
                visual
                    .indicator
                    .SetBrush(if item.foreground { &dots.1 } else { &dots.0 })?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Dock indicators failed: {error}"));
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        if !self.slots.is_empty() {
            let (xf, yf) = (x as f32, y as f32);
            if let Some(index) = self
                .icon_rects
                .iter()
                .position(|r| xf >= r.0 && xf < r.2 && yf >= r.1 && yf < r.3)
            {
                return Some(index);
            }
            let (l, t, r, b) = self.bar_rect;
            if x < l || x > r || y < t || y > b {
                return None;
            }
            let main = if self.vertical() { yf } else { xf };
            return self
                .slots
                .iter()
                .position(|(start, length)| main >= *start && main < start + length);
        }
        let (l, t, r, b) = self.bar_rect;
        if x < l || x > r || y < t || y > b {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let place = self.placement?;
        let scale = self.scale();
        let main = if self.vertical() { y - t } else { x - l };
        let local = (f64::from(main) - f64::from(place.content_offset)) / scale;
        layout
            .cells
            .iter()
            .zip(&layout.sizes)
            .position(|(&cell, &size)| local >= cell && local < cell + size)
    }

    // ---------- Magnification ----------

    /// Magnification follows the dock setting, the animations setting and Windows' "Animation effects".
    fn magnification_allowed(&self) -> bool {
        self.settings.magnification_enabled
            && self.settings.animations_enabled
            && client_area_animation()
    }

    fn mouse_moved(&mut self, x: i32, y: i32) {
        if !self.tracking_mouse {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd as HWND,
                dwHoverTime: 0,
            };
            // SAFETY: asks for WM_MOUSELEAVE on our own window.
            unsafe { TrackMouseEvent(&mut track) };
            self.tracking_mouse = true;
        }
        if self.autohide.mode != AutoHideMode::Disabled {
            // OnMouseEnterDock / OnMouseEnterEdge: the pointer is on the dock or on the edge strip.
            self.stop_hide_timer();
            if self.autohide.hidden {
                self.autohide.at_edge = true;
                self.reveal();
                self.last_mouse = Some((x, y));
                return;
            }
            self.autohide.over_dock = true;
            self.sync_guard();
        }
        let hovered = self.hit_test(x, y);
        self.shell_hover(hovered);
        self.preview_hover(hovered);
        self.tip_hover(hovered);
        // Windows repeats WM_MOUSEMOVE at the same point (e.g. after a region change); nothing moved.
        if self.last_mouse == Some((x, y)) {
            return;
        }
        self.last_mouse = Some((x, y));
        if !self.magnification_allowed() || !self.over_surface(x, y) {
            self.release_magnification();
            return;
        }
        if !self.magnifier.engaged && !self.magnifier.is_captured() {
            self.capture_rest_geometry();
        }
        let main = if self.vertical() { y } else { x };
        let was_engaged = self.magnifier.engaged;
        if self.magnifier.pointer_at(f64::from(main) / self.scale()) {
            if !was_engaged {
                // Grown once for the whole hover: the region also clips drawing, so magnified icons need it.
                self.update_region();
                self.load_hover_icons();
            }
            self.frames.set_running(true);
        } else {
            self.release_magnification();
        }
    }

    fn mouse_left(&mut self) {
        self.shell_hover(None);
        self.preview_hover(None);
        self.tip_hover(None);
        self.tracking_mouse = false;
        self.last_mouse = None;
        self.release_magnification();
        self.autohide_evaluate();
    }

    /// Over the bar or over a magnified icon (which reaches above the bar).
    fn over_surface(&self, x: i32, y: i32) -> bool {
        let (l, t, r, b) = self.bar_rect;
        if x >= l && x <= r && y >= t && y <= b {
            return true;
        }
        let (xf, yf) = (x as f32, y as f32);
        self.icon_rects
            .iter()
            .any(|r| xf >= r.0 && xf < r.2 && yf >= r.1 && yf < r.3)
    }

    fn release_magnification(&mut self) {
        if !self.magnifier.engaged {
            return;
        }
        self.magnifier.release();
        if self.magnifier.needs_frames() {
            self.frames.set_running(true);
        } else {
            self.finish_magnification();
        }
    }

    /// Rest geometry for one hover: item centers, which items magnify and the room the bar may grow into.
    fn capture_rest_geometry(&mut self) {
        let (Some(layout), Some(place)) = (self.layout.as_ref(), self.placement) else {
            return;
        };
        let scale = self.scale();
        let px = |dips: f64| (dips * scale).round() as f32;
        let vertical = self.vertical();
        let origin = if vertical { place.y } else { place.x } + place.content_offset;
        let centers = layout
            .cells
            .iter()
            .zip(&layout.sizes)
            .map(|(cell, size)| f64::from(origin + px(*cell) + px(*size) / 2.0) / scale)
            .collect();
        let magnifies = self
            .items
            .iter()
            .map(|item| matches!(item.kind, ItemKind::App { .. }))
            .collect();
        let spec = self.bar_spec();
        let length = f64::from(if vertical { place.height } else { place.width }) / scale;
        let available = if spec.full_width || spec.fill || spec.fixed_length.is_some() {
            length - layout.width
        } else {
            let (w, h) = self.client_size();
            let window_main = f64::from(if vertical { h } else { w }) / scale;
            window_main - crate::layout::SIDE_INSET * 2.0 - layout.width
        };
        let icon = (f64::from(application_icon_pixels(self.profile.icon, scale)) / scale)
            .max(self.profile.icon);
        self.magnifier
            .capture(magnifies, centers, self.profile.cell, icon, available);
    }

    fn frame(&mut self) {
        self.frames.frame_handled();
        if !self.magnifier.engaged {
            self.frames.set_running(false);
            return;
        }
        let frame = self.magnifier.step(Instant::now());
        for (index, magnified) in frame.hover_changes {
            self.show_hover_icon(index, magnified);
        }
        self.apply_magnified_geometry();
        if frame.settled {
            self.frames.set_running(false);
            if self.magnifier.at_rest() {
                self.finish_magnification();
            }
        }
    }

    /// The effect is over: exact rest geometry, rest icons and the rest input region.
    fn finish_magnification(&mut self) {
        self.end_magnification_now();
        self.widget_relayout_pending = false;
        self.relayout();
    }

    /// Drops the effect at once (also when the layout changes under it).
    fn end_magnification_now(&mut self) {
        if !self.magnifier.engaged {
            return;
        }
        self.frames.set_running(false);
        for index in 0..self.visuals.items.len() {
            self.show_hover_icon(index, false);
        }
        self.magnifier.reset();
        self.slots.clear();
        self.icon_rects.clear();
        self.applied.clear();
    }

    fn show_hover_icon(&self, index: usize, magnified: bool) {
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let brush = if magnified {
            visual.hover_brush.as_ref()
        } else {
            visual.rest_brush.as_ref()
        };
        if let Some(brush) = brush {
            let _ = visual.icon.SetBrush(brush);
        }
    }

    /// Larger app icons for the magnified state (`ApplicationIconProvider.GetHoverVariant`), loaded in the
    /// background on the first hover and kept.
    fn load_hover_icons(&mut self) {
        self.hover_icons_requested = true;
        let rest = application_icon_pixels(self.profile.icon, self.scale());
        // Source: the native size that covers the peak (as OpenTaskbar's hover variant). Surface: exactly the
        // peak's device pixels, resampled once in Direct2D, so the hovered icon is drawn 1:1 at its peak instead
        // of being shrunk again by the compositor (which softened it).
        let hover = fisheye::hover_icon_pixels(rest, fisheye::HOVER_SCALE);
        let target = (f64::from(rest) * self.magnifier.peak()).round() as u32;
        if hover <= rest || target <= rest {
            return;
        }
        for index in 0..self.items.len() {
            if !matches!(self.items[index].kind, ItemKind::App { .. }) {
                continue;
            }
            let Some((name, _)) = self.icon_source(index) else {
                continue;
            };
            let key = (name, target);
            if self.visuals.items[index].hover_key.as_ref() == Some(&key) {
                continue;
            }
            let Some(icon) = self.icons.get(&key.0, hover) else {
                continue;
            };
            match self
                .renderer
                .bitmap_brush(icon.width, icon.height, &icon.pixels, target)
            {
                Ok(brush) => {
                    let visual = &mut self.visuals.items[index];
                    visual.hover_brush = Some(brush);
                    visual.hover_key = Some(key);
                    if self.magnifier.hover.get(index).copied().unwrap_or(false) {
                        self.show_hover_icon(index, true);
                    }
                }
                Err(error) => log::warn(format!("Dock hover icon failed: {error}")),
            }
        }
    }

    /// Input (and drawing) region while magnified: the widest bar one hover can produce, across the whole window
    /// thickness so icons can grow into the headroom.
    fn update_magnified_region(&self) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let scale = self.scale();
        let (w, h) = self.client_size();
        let place = self.bar_spec().place(
            (w as f32, h as f32),
            layout.width + self.magnifier.max_growth(),
            layout.height,
            scale,
        );
        let margin = if self.colors.shadow {
            (f64::from(SHADOW_BLUR_DIPS) * scale * 0.5).round() as i32
        } else {
            0
        };
        let (l, t, r, b) = if self.vertical() {
            (
                0,
                place.y as i32 - margin,
                w,
                (place.y + place.height) as i32 + margin,
            )
        } else {
            (
                place.x as i32 - margin,
                0,
                (place.x + place.width) as i32 + margin,
                h,
            )
        };
        // SAFETY: the region is owned by the window after SetWindowRgn.
        unsafe {
            let region = CreateRectRgn(l.max(0), t.max(0), r.min(w) + 1, b.min(h) + 1);
            SetWindowRgn(self.hwnd as HWND, region, 1);
        }
    }

    /// Places the bar, slots, icons, indicators and separators for the current spring state.
    fn apply_magnified_geometry(&mut self) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let scale = self.scale();
        let px = |dips: f64| (dips * scale).round() as f32;
        let (w, h) = self.client_size();
        let place = self.bar_spec().place(
            (w as f32, h as f32),
            layout.width + self.magnifier.total_extra,
            layout.height,
            scale,
        );
        let vertical = self.vertical();
        let edge = self.settings.edge;
        let cell_px = px(self.profile.cell);
        let cell_cross = px(self.profile.cell_height);
        let icon = application_icon_pixels(self.profile.icon, scale) as f32;
        let icon_dips = f64::from(icon) / scale;
        let pad = px(self.profile.vertical_padding + self.profile.border);
        let headroom = fisheye::HEADROOM + self.profile.vertical_padding;
        let main_start = if vertical { place.y } else { place.x };
        let row = self.indicator_row_px();
        let rest_cross_full = ((cell_cross - icon) / 2.0).round();
        let rest_cross_row = ((cell_cross - row - icon) / 2.0).round();
        let axis = |main: f32, cross: f32| {
            if vertical {
                (cross, main)
            } else {
                (main, cross)
            }
        };
        // Bottom dock: icons grow upward from their bottom edge; side docks: from the center.
        let (cx, cy) = if vertical {
            (icon / 2.0, icon / 2.0)
        } else {
            (icon / 2.0, icon)
        };
        let first_frame = self.applied.is_empty();
        if first_frame {
            self.applied = vec![Applied::default(); layout.cells.len()];
        }
        // Buffers are reused between frames; only values that changed are written to the visuals.
        let mut slots = std::mem::take(&mut self.slots);
        let mut icon_rects = std::mem::take(&mut self.icon_rects);
        slots.clear();
        icon_rects.clear();
        let v = &self.visuals;
        let m = &self.magnifier;
        let applied = &mut self.applied;
        let items = &self.items;
        let result: WinResult<()> = (|| {
            let size = Vector2 {
                X: place.width,
                Y: place.height,
            };
            v.bar.SetOffset(Vector3 {
                X: place.x,
                Y: place.y,
                Z: 0.0,
            })?;
            v.bar.SetSize(size)?;
            v.shape.SetSize(size)?;
            v.geometry.SetSize(Vector2 {
                X: place.width - 1.0,
                Y: place.height - 1.0,
            })?;
            v.shadow.SetSize(size)?;
            let mut shift = 0.0f32;
            for (index, cell) in layout.cells.iter().enumerate() {
                let (Some(visual), Some(&s), Some(last)) = (
                    v.items.get(index),
                    m.scales.get(index),
                    applied.get_mut(index),
                ) else {
                    break;
                };
                let s = s as f32;
                let extra = (m.extras.get(index).copied().unwrap_or(0.0) * scale) as f32;
                let lift = (m.lift(index, icon_dips, headroom) * scale) as f32;
                let start = place.content_offset + px(*cell) + shift;
                let item_px = layout.sizes.get(index).map_or(cell_px, |size| px(*size));
                let slot = item_px + extra;
                // Centered in the item's own slot, as at rest (pinned tray icons are narrower than a cell).
                let rest_main = ((item_px - icon) / 2.0).round();
                shift += extra;
                let (x, y) = axis(start, pad);
                if last.start != start {
                    visual.container.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
                }
                let (dx, dy) = match edge {
                    DockEdge::Bottom => (0.0, -lift),
                    DockEdge::Left => (lift, 0.0),
                    DockEdge::Right => (-lift, 0.0),
                };
                if items
                    .get(index)
                    .is_some_and(|i| matches!(i.kind, ItemKind::Widget(_)))
                {
                    // Widgets do not magnify; they only move along with the bar.
                    let (x, y) = axis(start, pad);
                    if last.start != start {
                        visual.container.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
                    }
                    *last = Applied {
                        start,
                        extra,
                        scale: 1.0,
                        lift: 0.0,
                    };
                    shift += extra;
                    let size = visual.icon.Size()?;
                    let offset = visual.icon.Offset()?;
                    slots.push((main_start + start, item_px + extra));
                    let left = place.x + x + offset.X;
                    let top = place.y + y + offset.Y;
                    icon_rects.push((left, top, left + size.X, top + size.Y));
                    continue;
                }
                let rest_cross = if items.get(index).is_some_and(|i| has_indicator_row(&i.kind)) {
                    rest_cross_row
                } else {
                    rest_cross_full
                };
                let (ix, iy) = axis(rest_main + extra / 2.0, rest_cross);
                if last.extra != extra {
                    let (sw, sh) = axis(slot, cell_cross);
                    visual.container.SetSize(Vector2 { X: sw, Y: sh })?;
                    // The running indicator stays centered under the icon in the wider slot.
                    if let Some(item) = items.get(index)
                        && !item.windows.is_empty()
                    {
                        let (_, ox, oy) =
                            indicator_geometry(edge, scale, slot, cell_cross, item.foreground);
                        visual.indicator.SetOffset(Vector3 {
                            X: ox,
                            Y: oy,
                            Z: 0.0,
                        })?;
                    }
                }
                if last.extra != extra || last.lift != lift {
                    visual.icon.SetOffset(Vector3 {
                        X: ix + dx,
                        Y: iy + dy,
                        Z: 0.0,
                    })?;
                }
                if first_frame {
                    visual.icon.SetCenterPoint(Vector3 {
                        X: cx,
                        Y: cy,
                        Z: 0.0,
                    })?;
                }
                if last.scale != s {
                    visual.icon.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
                }
                *last = Applied {
                    start,
                    extra,
                    scale: s,
                    lift,
                };
                slots.push((main_start + start, slot));
                let left = place.x + x + ix + dx + cx - cx * s;
                let top = place.y + y + iy + dy + cy - cy * s;
                icon_rects.push((left, top, left + icon * s, top + icon * s));
            }
            let line = px(crate::layout::DIVIDER_HEIGHT).min(cell_cross);
            for (sprite, separator) in v.separators.iter().zip(&layout.separators) {
                let before: f32 = layout
                    .cells
                    .iter()
                    .zip(&m.extras)
                    .filter(|(cell, _)| **cell < *separator)
                    .map(|(_, e)| (*e * scale) as f32)
                    .sum();
                let (x, y) = axis(
                    place.content_offset + px(*separator) + before,
                    pad + ((cell_cross - line) / 2.0).round(),
                );
                sprite.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Dock magnification failed: {error}"));
        }
        self.bar_rect = (
            place.x as i32,
            place.y as i32,
            (place.x + place.width) as i32,
            (place.y + place.height) as i32,
        );
        self.slots = slots;
        self.icon_rects = icon_rects;
    }

    // ---------- Auto-hide ----------

    fn animations_allowed(&self) -> bool {
        self.settings.animations_enabled && client_area_animation()
    }

    /// The bar's resting rectangle on screen (the slide offset is not included).
    fn dock_screen_rect(&self) -> Option<ot_shell::Rect> {
        let origin = ot_shell::win::window_rect(self.hwnd)?;
        let (l, t, r, b) = self.bar_rect;
        Some(ot_shell::Rect::new(
            origin.left + l,
            origin.top + t,
            origin.left + r,
            origin.top + b,
        ))
    }

    /// Remembers the application in front, for when the Start menu or the dock itself takes the foreground.
    fn note_foreground(&mut self, foreground: Hwnd) {
        let root = ot_shell::win::root_owner(foreground);
        let target = if root != 0 { root } else { foreground };
        if target != 0
            && target != self.hwnd
            && ot_shell::win::process_id(target) != std::process::id()
            && !autohide::is_shell_overlay(target)
        {
            self.autohide.last_application = target;
        }
    }

    /// Smart mode: does any application window cover the dock (`RefreshSmartOverlap`).
    fn refresh_overlap(&mut self) {
        if self.autohide.mode != AutoHideMode::Smart {
            return;
        }
        let foreground = ot_shell::win::foreground();
        if foreground == 0 {
            return;
        }
        let root = ot_shell::win::root_owner(foreground);
        let mut target = if root != 0 { root } else { foreground };
        if !ot_shell::win::is_window(target) {
            return;
        }
        let last = self.autohide.last_application;
        let usable = |w: Hwnd| w != 0 && ot_shell::win::is_window(w);
        if autohide::is_shell_overlay(target) {
            // Judge the application beneath the Start menu or the switcher.
            target = if usable(last) { last } else { 0 };
        }
        if target != 0 && ot_shell::win::process_id(target) == std::process::id() {
            target = if usable(last) && last != target {
                last
            } else {
                0
            };
        }
        let Some(dock) = self.dock_screen_rect() else {
            return;
        };
        let started = Instant::now();
        let covers = |w: Hwnd| autohide::window_covers_dock(w, self.hwnd, dock);
        self.autohide.overlap = (target != 0 && covers(target))
            || self.tracker.as_ref().is_some_and(|t| {
                t.snapshot()
                    .iter()
                    .any(|w| w.handle != target && covers(w.handle))
            });
        let micros = started.elapsed().as_micros() as u64;
        self.overlap_stats.0 += 1;
        self.overlap_stats.1 += micros;
        self.overlap_stats.2 = self.overlap_stats.2.max(micros);
    }

    fn log_stats(&self) {
        let (count, total, max) = self.overlap_stats;
        if let Some(average) = total.checked_div(count) {
            log::info(format!(
                "Auto-hide overlap checks: {count}, average {average} us, max {max} us."
            ));
        }
    }

    fn sync_pointer(&mut self) {
        let Some(cursor) = autohide::cursor() else {
            return;
        };
        let scale = self.scale();
        let edge = self.settings.edge;
        if let Some(dock) = self.dock_screen_rect() {
            let headroom = (fisheye::HEADROOM * scale).ceil() as i32;
            self.autohide.over_dock = autohide::pointer_over_dock(cursor, dock, edge, headroom);
        }
        if let Some((monitor, _)) =
            ot_shell::win::monitor_info(ot_shell::win::monitor_of(self.hwnd))
        {
            self.autohide.at_edge =
                autohide::pointer_at_edge(cursor, monitor, edge, autohide::edge_pixels(scale));
        }
    }

    /// `EvaluateState`: start the hide delay, or reveal.
    fn autohide_evaluate(&mut self) {
        if self.autohide.mode == AutoHideMode::Disabled {
            return;
        }
        if !self.autohide.hidden {
            self.sync_pointer();
        }
        self.autohide.shell_menu_open = autohide::start_menu_open();
        if self.autohide.should_hide(
            menu::is_open()
                || self.drag.is_some()
                || preview::is_visible()
                || tray_flyout::is_open()
                || crate::panel::is_open(None)
                || self.keyboard.is_some(),
        ) {
            if !self.autohide.hidden && !self.hide_timer {
                // SAFETY: timer on our own window.
                unsafe {
                    wm::SetTimer(self.hwnd as HWND, TIMER_HIDE, autohide::HIDE_DELAY_MS, None)
                };
                self.hide_timer = true;
            }
        } else {
            self.stop_hide_timer();
            if self.autohide.hidden {
                self.reveal();
            }
        }
        self.sync_guard();
    }

    fn stop_hide_timer(&mut self) {
        if self.hide_timer {
            // SAFETY: timer on our own window.
            unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_HIDE) };
            self.hide_timer = false;
        }
    }

    /// The pointer is polled only while leaving the dock would hide it.
    fn sync_guard(&mut self) {
        let want = self.autohide.hide_pending_on_pointer();
        if want == self.guard_timer {
            return;
        }
        // SAFETY: timer on our own window.
        unsafe {
            if want {
                wm::SetTimer(self.hwnd as HWND, TIMER_GUARD, autohide::GUARD_MS, None);
            } else {
                wm::KillTimer(self.hwnd as HWND, TIMER_GUARD);
            }
        }
        self.guard_timer = want;
    }

    fn hide_timer_elapsed(&mut self) {
        self.stop_hide_timer();
        if self.autohide.hidden {
            return;
        }
        self.sync_pointer();
        self.autohide.shell_menu_open = autohide::start_menu_open();
        if self
            .autohide
            .should_hide(menu::is_open() || self.drag.is_some())
        {
            self.hide();
        }
    }

    fn hide(&mut self) {
        // Nothing ticks while the dock is out of sight; the reveal updates the widgets at once.
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_WIDGETS) };
        self.tip_hide();
        if self.magnifier.engaged {
            self.finish_magnification();
        }
        self.autohide.hidden = true;
        self.sync_guard();
        let scale = self.scale();
        let thickness = self
            .placement
            .map(|p| f64::from(if self.vertical() { p.width } else { p.height }) / scale)
            .unwrap_or(self.profile.surface_height());
        let offset = (autohide::hide_offset(self.settings.edge, thickness) * scale) as f32;
        if self.slide_to(offset, autohide::HIDE_MS, true) {
            // The region also clips drawing: switch to the edge strip once the bar is out of sight.
            // SAFETY: timer on our own window.
            unsafe {
                wm::SetTimer(
                    self.hwnd as HWND,
                    TIMER_HIDDEN,
                    autohide::HIDE_MS + 20,
                    None,
                )
            };
        } else {
            self.hidden_slide_done();
        }
    }

    fn hidden_slide_done(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_HIDDEN) };
        if self.autohide.hidden {
            self.strip_region = true;
            self.update_region();
        }
    }

    fn reveal(&mut self) {
        self.autohide.hidden = false;
        self.widget_tick();
        self.stop_hide_timer();
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_HIDDEN) };
        self.strip_region = false;
        self.update_region();
        self.slide_to(0.0, autohide::REVEAL_MS, false);
        self.sync_guard();
    }

    /// Slides the dock along the edge's axis; returns true when it animates. Ease-in-out to hide, ease-out to
    /// reveal (WPF `CubicEase`).
    fn slide_to(&self, offset: f32, milliseconds: u32, ease_in_out: bool) -> bool {
        let property = if self.vertical() {
            "Offset.X"
        } else {
            "Offset.Y"
        };
        let root = &self.renderer.root;
        let animated = self.animations_allowed();
        let result: WinResult<()> = (|| {
            if !animated {
                root.StopAnimation(&windows::core::HSTRING::from(property))?;
                root.SetOffset(if self.vertical() {
                    Vector3 {
                        X: offset,
                        Y: 0.0,
                        Z: 0.0,
                    }
                } else {
                    Vector3 {
                        X: 0.0,
                        Y: offset,
                        Z: 0.0,
                    }
                })?;
                return Ok(());
            }
            let c = &self.renderer.compositor;
            let (p1, p2) = if ease_in_out {
                ((0.65, 0.0), (0.35, 1.0))
            } else {
                ((0.33, 1.0), (0.68, 1.0))
            };
            let easing = c.CreateCubicBezierEasingFunction(
                Vector2 { X: p1.0, Y: p1.1 },
                Vector2 { X: p2.0, Y: p2.1 },
            )?;
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(1.0, offset, &easing)?;
            animation.SetDuration(windows::Foundation::TimeSpan {
                Duration: i64::from(milliseconds) * 10_000,
            })?;
            root.StartAnimation(&windows::core::HSTRING::from(property), &animation)?;
            Ok(())
        })();
        match result {
            Ok(()) => animated,
            Err(error) => {
                log::warn(format!("Dock slide failed: {error}"));
                false
            }
        }
    }

    /// Hidden: only a thin strip along the screen edge takes the pointer, which reveals the dock.
    fn update_strip_region(&self) {
        let (w, h) = self.client_size();
        let strip = autohide::edge_pixels(self.scale());
        let (l, t, r, b) = match self.settings.edge {
            DockEdge::Bottom => (0, h - strip, w, h),
            DockEdge::Left => (0, 0, strip, h),
            DockEdge::Right => (w - strip, 0, w, h),
        };
        // SAFETY: the region is owned by the window after SetWindowRgn.
        unsafe {
            let region = CreateRectRgn(l.max(0), t.max(0), r, b);
            SetWindowRgn(self.hwnd as HWND, region, 1);
        }
    }

    /// Live mode change from the settings.
    fn set_auto_hide_mode(&mut self, mode: AutoHideMode) {
        if self.autohide.hidden {
            // Edge, size or mode may have changed: come back and judge again.
            self.reveal();
        }
        self.autohide.mode = mode;
        if mode == AutoHideMode::Disabled {
            self.stop_hide_timer();
            self.autohide.overlap = false;
            self.sync_guard();
            return;
        }
        self.refresh_overlap();
        self.autohide_evaluate();
    }

    // ---------- Pins ----------

    /// The pin order as OpenTaskbar saves it: executable paths, pinned apps first to last.
    fn pin_order(&self) -> Vec<String> {
        self.pins
            .iter()
            .map(|p| p.executable_path.clone())
            .collect()
    }

    /// Reloads pins in `order`, saves the order and rebuilds the dock.
    fn apply_pin_order(&mut self, order: Vec<String>) {
        self.pins = pins::load(Some(&order));
        self.settings.pin_order = Some(self.pin_order());
        persist(&self.settings);
        self.rebuild_items();
    }

    /// Pins a running app (`PinRunningApp`), at `index` among the pins or last.
    fn pin_app(&mut self, name: &str, executable: &str, index: Option<usize>) {
        let language = self.settings.language.clone();
        if let Err(key) = pins::add(name, executable, None) {
            log::report_error(key, &language, None);
            return;
        }
        let mut order = self.pin_order();
        order.retain(|p| !p.eq_ignore_ascii_case(executable));
        let at = index.unwrap_or(order.len()).min(order.len());
        order.insert(at, executable.to_owned());
        log::info(format!("Pinned {executable} at {at}."));
        self.apply_pin_order(order);
    }

    fn unpin_app(&mut self, pin: &Pin) {
        let language = self.settings.language.clone();
        if let Err(key) = pins::remove(&pin.shortcut_path) {
            log::report_error(key, &language, None);
            return;
        }
        let mut order = self.pin_order();
        order.retain(|p| !p.eq_ignore_ascii_case(&pin.executable_path));
        log::info(format!("Unpinned {}.", pin.executable_path));
        self.apply_pin_order(order);
    }

    // ---------- Drag reordering (OpenTaskbar #130) ----------

    /// Left button down on an item: a drag may start once the pointer moves past the system's drag distance.
    fn press(&mut self, x: i32, y: i32) {
        self.press = self
            .hit_test(x, y)
            .filter(|&i| matches!(self.items[i].kind, ItemKind::App { .. }))
            .map(|i| (i, x, y));
    }

    /// Starts a drag when the pressed button moved far enough; returns true while dragging.
    fn drag_motion(&mut self, x: i32, y: i32, buttons: WPARAM) -> bool {
        if self.drag.is_some() {
            self.drag_to(x, y);
            return true;
        }
        let Some((index, px, py)) = self.press else {
            return false;
        };
        if buttons & 0x0001 == 0 {
            // MK_LBUTTON released elsewhere.
            self.press = None;
            return false;
        }
        // SAFETY: plain Win32 calls.
        let (dx, dy) = unsafe {
            (
                wm::GetSystemMetrics(wm::SM_CXDRAG),
                wm::GetSystemMetrics(wm::SM_CYDRAG),
            )
        };
        if (x - px).abs() <= dx && (y - py).abs() <= dy {
            return false;
        }
        if matches!(
            self.items.get(index).map(|i| &i.kind),
            Some(ItemKind::TrayPin(_) | ItemKind::Widget(_))
        ) {
            // The strip's buttons are not dock items, and widgets do not reorder yet: no drag.
            return false;
        }
        self.press = None;
        self.tip_hide();
        self.preview_hide();
        tray_flyout::close();
        crate::panel::close();
        self.begin_drag(index, x, y);
        self.drag.is_some()
    }

    fn main_of(&self, x: i32, y: i32) -> f32 {
        let place = self.placement.unwrap_or(BarPlacement {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            content_offset: 0.0,
            square_corners: false,
        });
        if self.vertical() {
            y as f32 - place.y
        } else {
            x as f32 - place.x
        }
    }

    /// Container offset of item `index` at rest, along the bar (bar-relative pixels).
    fn rest_main(&self, index: usize) -> f32 {
        let scale = self.scale();
        let (Some(layout), Some(place)) = (self.layout.as_ref(), self.placement) else {
            return 0.0;
        };
        place.content_offset
            + (layout.cells.get(index).copied().unwrap_or(0.0) * scale).round() as f32
    }

    fn begin_drag(&mut self, index: usize, x: i32, y: i32) {
        // OpenTaskbar drops the magnification at once here (`ResetMagnification(immediate: true)`); at the user's
        // request the dock eases from the magnified state to rest in the lift's 130 ms instead.
        let mut carried = 0.0f32;
        if self.magnifier.engaged {
            let before = self.snapshot();
            let dragged_before = before
                .items
                .get(index)
                .map(|i| self.main(i.container_offset));
            self.end_magnification_now();
            self.relayout();
            let after = self.snapshot();
            if self.animations_allowed() {
                let identity: Vec<Option<usize>> = (0..after.items.len()).map(Some).collect();
                self.ease_between(
                    &before,
                    &after,
                    &identity,
                    Some(index),
                    reorder::LIFT_MS,
                    false,
                );
            }
            // The dragged item starts where it was shown and follows the pointer from there.
            if let (Some(shown), Some(rest)) = (
                dragged_before,
                after
                    .items
                    .get(index)
                    .map(|i| self.main(i.container_offset)),
            ) {
                carried = shown - rest;
            }
        }
        let apps: Vec<usize> = (0..self.items.len())
            .filter(|&i| matches!(self.items[i].kind, ItemKind::App { .. }))
            .collect();
        let Some(dragged) = apps.iter().position(|&i| i == index) else {
            return;
        };
        let cell = (self.profile.cell * self.scale()).round() as f32;
        let slots: Vec<(f32, f32)> = apps.iter().map(|&i| (self.rest_main(i), cell)).collect();
        let pinned = apps
            .iter()
            .filter(|&&i| matches!(&self.items[i].kind, ItemKind::App { pin: Some(_), .. }))
            .count();
        // SAFETY: our own window keeps the pointer while dragging.
        unsafe { SetCapture(self.hwnd as HWND) };
        if let Some(visual) = self.visuals.items.get(index) {
            // Drawn above the neighbours it passes over (`Panel.SetZIndex(dragged, 100)`).
            if let Ok(children) = self.visuals.bar.Children() {
                let _ = children.Remove(&visual.container);
                let _ = children.InsertAtTop(&visual.container);
            }
            let _ = visual.container.SetOpacity(reorder::DRAG_OPACITY);
            let animate = self.animations_allowed();
            let lift = (reorder::DRAG_LIFT_DIPS * self.scale()) as f32;
            self.lift_icon(index, lift, reorder::DRAG_SCALE, animate);
        }
        self.drag = Some(DragState {
            item: index,
            apps,
            dragged,
            slots,
            origin: self.main_of(x, y) - carried,
            offset: carried,
            insertion: None,
            pinned,
        });
        self.autohide_evaluate();
    }

    fn main(&self, offset: Vector3) -> f32 {
        if self.vertical() { offset.Y } else { offset.X }
    }

    /// Current composition values of the bar and its items (as last set; nothing animates while magnified).
    fn snapshot(&self) -> Snapshot {
        let v = &self.visuals;
        Snapshot {
            bar_offset: v.bar.Offset().unwrap_or_default(),
            bar_size: v.bar.Size().unwrap_or_default(),
            geometry_size: v.geometry.Size().unwrap_or_default(),
            items: v
                .items
                .iter()
                .map(|item| ItemSnapshot {
                    container_offset: item.container.Offset().unwrap_or_default(),
                    container_size: item.container.Size().unwrap_or_default(),
                    icon_offset: item.icon.Offset().unwrap_or_default(),
                    icon_scale: item.icon.Scale().unwrap_or(Vector3 {
                        X: 1.0,
                        Y: 1.0,
                        Z: 1.0,
                    }),
                    indicator_offset: item.indicator.Offset().unwrap_or_default(),
                })
                .collect(),
            separators: v
                .separators
                .iter()
                .map(|s| s.Offset().unwrap_or_default())
                .collect(),
        }
    }

    /// Animates every value that differs from `before` to `after` (cubic ease-out, the lift's duration). The
    /// dragged item's icon is left to the lift animation, which starts from the value restored here.
    fn ease_between(
        &self,
        before: &Snapshot,
        after: &Snapshot,
        mapping: &[Option<usize>],
        dragged: Option<usize>,
        duration_ms: u32,
        appear: bool,
    ) {
        let c = &self.renderer.compositor;
        let main = if self.vertical() {
            "Offset.Y"
        } else {
            "Offset.X"
        };
        let cross = if self.vertical() {
            "Offset.X"
        } else {
            "Offset.Y"
        };
        let pick = |o: Vector3, axis: &str| if axis == "Offset.X" { o.X } else { o.Y };
        let result: WinResult<()> = (|| {
            let ease = ease_out(c)?;
            let duration = milliseconds(duration_ms);
            let scalar = |object: &ot_render::Composition::CompositionObject,
                          property: &str,
                          from: f32,
                          to: f32|
             -> WinResult<()> {
                if (from - to).abs() < 0.01 {
                    return Ok(());
                }
                let a = c.CreateScalarKeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, from)?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
                a.SetDuration(duration)?;
                object.StartAnimation(&HSTRING::from(property), &a)
            };
            let size = |object: &ot_render::Composition::CompositionObject,
                        from: Vector2,
                        to: Vector2|
             -> WinResult<()> {
                if (from.X - to.X).abs() < 0.01 && (from.Y - to.Y).abs() < 0.01 {
                    return Ok(());
                }
                let a = c.CreateVector2KeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, from)?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
                a.SetDuration(duration)?;
                object.StartAnimation(&HSTRING::from("Size"), &a)
            };
            let v = &self.visuals;
            scalar(
                &v.bar.cast()?,
                main,
                pick(before.bar_offset, main),
                pick(after.bar_offset, main),
            )?;
            for object in [v.bar.cast()?, v.shape.cast()?, v.shadow.cast()?] {
                size(&object, before.bar_size, after.bar_size)?;
            }
            size(
                &v.geometry.cast()?,
                before.geometry_size,
                after.geometry_size,
            )?;
            for (index, (visual, a)) in v.items.iter().zip(&after.items).enumerate() {
                let Some(b) = mapping
                    .get(index)
                    .copied()
                    .flatten()
                    .and_then(|j| before.items.get(j))
                else {
                    if appear {
                        // A new app eases in like the removal in reverse (OpenTaskbar shows it at once).
                        visual.container.SetCenterPoint(Vector3 {
                            X: a.container_size.X / 2.0,
                            Y: a.container_size.Y / 2.0,
                            Z: 0.0,
                        })?;
                        let fade = c.CreateScalarKeyFrameAnimation()?;
                        fade.InsertKeyFrame(0.0, 0.0)?;
                        fade.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
                        fade.SetDuration(duration)?;
                        visual
                            .container
                            .StartAnimation(&HSTRING::from("Opacity"), &fade)?;
                        let grow = c.CreateVector3KeyFrameAnimation()?;
                        grow.InsertKeyFrame(
                            0.0,
                            Vector3 {
                                X: 0.95,
                                Y: 0.95,
                                Z: 1.0,
                            },
                        )?;
                        grow.InsertKeyFrameWithEasingFunction(
                            1.0,
                            Vector3 {
                                X: 1.0,
                                Y: 1.0,
                                Z: 1.0,
                            },
                            &ease,
                        )?;
                        grow.SetDuration(duration)?;
                        visual
                            .container
                            .StartAnimation(&HSTRING::from("Scale"), &grow)?;
                    }
                    continue;
                };
                let container: ot_render::Composition::CompositionObject =
                    visual.container.cast()?;
                scalar(
                    &container,
                    main,
                    pick(b.container_offset, main),
                    pick(a.container_offset, main),
                )?;
                size(&container, b.container_size, a.container_size)?;
                let indicator: ot_render::Composition::CompositionObject =
                    visual.indicator.cast()?;
                scalar(
                    &indicator,
                    main,
                    pick(b.indicator_offset, main),
                    pick(a.indicator_offset, main),
                )?;
                let icon: ot_render::Composition::CompositionObject = visual.icon.cast()?;
                scalar(
                    &icon,
                    main,
                    pick(b.icon_offset, main),
                    pick(a.icon_offset, main),
                )?;
                if Some(index) == dragged {
                    visual.icon.SetScale(b.icon_scale)?;
                    continue;
                }
                scalar(
                    &icon,
                    cross,
                    pick(b.icon_offset, cross),
                    pick(a.icon_offset, cross),
                )?;
                if (b.icon_scale.X - a.icon_scale.X).abs() >= 0.001 {
                    let grow = c.CreateVector3KeyFrameAnimation()?;
                    grow.InsertKeyFrame(0.0, b.icon_scale)?;
                    grow.InsertKeyFrameWithEasingFunction(1.0, a.icon_scale, &ease)?;
                    grow.SetDuration(duration)?;
                    icon.StartAnimation(&HSTRING::from("Scale"), &grow)?;
                }
            }
            let same_separators = before.separators.len() == after.separators.len();
            for ((sprite, b), a) in v
                .separators
                .iter()
                .zip(&before.separators)
                .zip(&after.separators)
                .filter(|_| same_separators)
            {
                scalar(&sprite.cast()?, main, pick(*b, main), pick(*a, main))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Dock rest transition failed: {error}"));
        }
    }

    /// The dragged icon grows and lifts away from the edge (`AnimateAppDrag`), or returns to rest.
    fn lift_icon(&self, index: usize, lift: f32, scale: f32, animate: bool) {
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let px = |dips: f64| (dips * self.scale()).round() as f32;
        let icon = application_icon_pixels(self.profile.icon, self.scale()) as f32;
        let cell_px = px(self.profile.cell);
        let cell_cross = px(self.profile.cell_height);
        let rest_main = ((cell_px - icon) / 2.0).round();
        let below = if self
            .items
            .get(index)
            .is_some_and(|i| has_indicator_row(&i.kind))
        {
            self.indicator_row_px()
        } else {
            0.0
        };
        let rest_cross = ((cell_cross - below - icon) / 2.0).round();
        let vertical = self.vertical();
        let (rx, ry) = if vertical {
            (rest_cross, rest_main)
        } else {
            (rest_main, rest_cross)
        };
        let (property, target) = match self.settings.edge {
            DockEdge::Bottom => ("Offset.Y", ry - lift),
            DockEdge::Left => ("Offset.X", rx + lift),
            DockEdge::Right => ("Offset.X", rx - lift),
        };
        let _ = visual.icon.SetCenterPoint(Vector3 {
            X: icon / 2.0,
            Y: if vertical { icon / 2.0 } else { icon },
            Z: 0.0,
        });
        let c = &self.renderer.compositor;
        let result: WinResult<()> = (|| {
            if !animate {
                visual.icon.StopAnimation(&HSTRING::from(property))?;
                visual.icon.StopAnimation(&HSTRING::from("Scale"))?;
                visual.icon.SetScale(Vector3 {
                    X: scale,
                    Y: scale,
                    Z: 1.0,
                })?;
                let mut offset = visual.icon.Offset()?;
                if property == "Offset.Y" {
                    offset.Y = target;
                } else {
                    offset.X = target;
                }
                return visual.icon.SetOffset(offset);
            }
            let ease = ease_out(c)?;
            let move_icon = c.CreateScalarKeyFrameAnimation()?;
            move_icon.InsertKeyFrameWithEasingFunction(1.0, target, &ease)?;
            move_icon.SetDuration(milliseconds(reorder::LIFT_MS))?;
            visual
                .icon
                .StartAnimation(&HSTRING::from(property), &move_icon)?;
            let grow = c.CreateVector3KeyFrameAnimation()?;
            grow.InsertKeyFrameWithEasingFunction(
                1.0,
                Vector3 {
                    X: scale,
                    Y: scale,
                    Z: 1.0,
                },
                &ease,
            )?;
            grow.SetDuration(milliseconds(reorder::LIFT_MS))?;
            visual.icon.StartAnimation(&HSTRING::from("Scale"), &grow)
        })();
        if let Err(error) = result {
            log::warn(format!("Dock drag animation failed: {error}"));
        }
    }

    /// Moves a container along the bar: at once, or with the 160 ms cubic ease-out slide.
    fn place_container(&self, index: usize, main: f32, slide: bool) {
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let property = if self.vertical() {
            "Offset.Y"
        } else {
            "Offset.X"
        };
        let c = &self.renderer.compositor;
        let result: WinResult<()> = (|| {
            if !slide || !self.animations_allowed() {
                visual.container.StopAnimation(&HSTRING::from(property))?;
                let mut offset = visual.container.Offset()?;
                if self.vertical() {
                    offset.Y = main;
                } else {
                    offset.X = main;
                }
                return visual.container.SetOffset(offset);
            }
            let animation = c.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(1.0, main, &ease_out(c)?)?;
            animation.SetDuration(milliseconds(reorder::SLIDE_MS))?;
            visual
                .container
                .StartAnimation(&HSTRING::from(property), &animation)
        })();
        if let Err(error) = result {
            log::warn(format!("Dock slide failed: {error}"));
        }
    }

    /// The dragged item follows the pointer; neighbours slide aside to open the landing slot.
    fn drag_to(&mut self, x: i32, y: i32) {
        // SAFETY: plain Win32 call; Esc cancels like OpenTaskbar's drag loop.
        if unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE)) } < 0 {
            self.cancel_drag();
            return;
        }
        let pointer = self.main_of(x, y);
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        let (start, size) = drag.slots[drag.dragged];
        let first = drag.slots.first().map_or(start, |s| s.0);
        let end = drag.slots.last().map_or(start + size, |s| s.0 + s.1);
        drag.offset =
            (pointer - drag.origin).clamp(first - start, (end - start - size).max(first - start));
        let insertion = reorder::insertion_index(&drag.slots, pointer);
        let changed = drag.insertion != Some(insertion);
        drag.insertion = Some(insertion);
        let (item, offset) = (drag.item, drag.offset);
        let slides: Vec<(usize, f32)> = if changed {
            drag.apps
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != drag.dragged)
                .map(|(i, &item)| {
                    (
                        item,
                        drag.slots[i].0 + reorder::slide_for(i, drag.dragged, insertion, size),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        self.place_container(item, start + offset, false);
        for (item, main) in slides {
            self.place_container(item, main, true);
        }
    }

    /// Esc, a lost capture or a release away from the dock: everything slides back (`CancelLiveDrag`).
    fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        // SAFETY: releases our own capture.
        unsafe { ReleaseCapture() };
        for (i, &item) in drag.apps.iter().enumerate() {
            self.place_container(item, drag.slots[i].0, true);
        }
        self.restore_dragged(drag.item);
        self.after_drag();
    }

    fn restore_dragged(&self, index: usize) {
        if let Some(visual) = self.visuals.items.get(index) {
            let _ = visual.container.SetOpacity(1.0);
        }
        self.lift_icon(index, 0.0, 1.0, self.animations_allowed());
    }

    /// Release: reorders the pins or pins a running app, then glides the item into its new slot and settles it.
    fn drop_drag(&mut self, x: i32, y: i32) {
        let (w, h) = self.client_size();
        let (l, _, r, b) = self.bar_rect;
        let on_dock = if self.vertical() {
            y >= 0 && y <= h && x >= 0 && x <= w
        } else {
            x >= l && x <= r && y >= 0 && y <= b
        };
        if !on_dock {
            self.cancel_drag();
            return;
        }
        let Some(drag) = self.drag.take() else {
            return;
        };
        // SAFETY: releases our own capture.
        unsafe { ReleaseCapture() };
        let pointer = self.main_of(x, y);
        let insertion = reorder::insertion_index(&drag.slots, pointer);
        let destination = reorder::destination(drag.dragged, insertion);
        let shown_at = drag.slots[drag.dragged].0 + drag.offset;
        let key = match &self.items[drag.item].kind {
            ItemKind::App {
                key,
                executable_path,
                pin,
                ..
            } => Some((
                key.clone(),
                executable_path.clone(),
                pin.as_ref().map(|p| p.name.clone()),
            )),
            _ => None,
        };
        if let Some(visual) = self.visuals.items.get(drag.item) {
            let _ = visual.container.SetOpacity(1.0);
        }
        match reorder::drop_action(drag.dragged, destination, drag.pinned) {
            reorder::DropAction::None => {
                for (i, &item) in drag.apps.iter().enumerate() {
                    self.place_container(item, drag.slots[i].0, true);
                }
                self.restore_dragged(drag.item);
                self.after_drag();
                return;
            }
            reorder::DropAction::MovePin(to) => {
                let mut order = self.pin_order();
                let moved = order.remove(drag.dragged);
                order.insert(to.min(order.len()), moved);
                log::info(format!("Moved pin to {to}."));
                self.drag_rebuild(|dock| dock.apply_pin_order(order));
            }
            reorder::DropAction::PinAt(at) => {
                if let Some((_, executable, _)) = &key {
                    // The shortcut is named like the app on the dock (`AddPin(app.Name, …)`), not after the exe.
                    let name = self.item_name(drag.item);
                    let executable = executable.clone();
                    self.drag_rebuild(|dock| dock.pin_app(&name, &executable, Some(at)));
                }
            }
        }
        // The dropped item glides from where it was shown into its new slot, then settles (`AnimateDropSettle`).
        let landed = key.and_then(|(key, _, _)| {
            self.items
                .iter()
                .position(|item| matches!(&item.kind, ItemKind::App { key: k, .. } if *k == key))
        });
        if let Some(index) = landed {
            let rest = self.rest_main(index);
            self.place_container(index, shown_at, false);
            self.place_container(index, rest, true);
            self.settle(index);
        }
        self.after_drag();
    }

    /// Runs a pin change with the drag's deferred rebuilds folded in.
    fn drag_rebuild(&mut self, change: impl FnOnce(&mut Self)) {
        self.rebuild_pending = false;
        change(self);
    }

    /// Scale 1.12 → 0.96 (110 ms) → 1.0 (200 ms), cubic ease-out.
    fn settle(&self, index: usize) {
        if !self.animations_allowed() {
            return;
        }
        let Some(visual) = self.visuals.items.get(index) else {
            return;
        };
        let icon = application_icon_pixels(self.profile.icon, self.scale()) as f32;
        let c = &self.renderer.compositor;
        let result: WinResult<()> = (|| {
            let _ = visual.icon.SetCenterPoint(Vector3 {
                X: icon / 2.0,
                Y: if self.vertical() { icon / 2.0 } else { icon },
                Z: 0.0,
            });
            let v = |s: f32| Vector3 { X: s, Y: s, Z: 1.0 };
            let animation = c.CreateVector3KeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(
                0.0,
                v(1.12),
                &c.CreateLinearEasingFunction()?,
            )?;
            animation.InsertKeyFrameWithEasingFunction(110.0 / 200.0, v(0.96), &ease_out(c)?)?;
            animation.InsertKeyFrameWithEasingFunction(1.0, v(1.0), &ease_out(c)?)?;
            animation.SetDuration(milliseconds(reorder::SETTLE_MS))?;
            visual
                .icon
                .StartAnimation(&HSTRING::from("Scale"), &animation)
        })();
        if let Err(error) = result {
            log::warn(format!("Dock settle animation failed: {error}"));
        }
    }

    fn after_drag(&mut self) {
        if std::mem::take(&mut self.rebuild_pending) {
            self.rebuild_items();
        }
        self.autohide_evaluate();
    }

    // ---------- Names, badges and tooltips ----------

    /// The item's name for tooltips, UI Automation and menus (OpenTaskbar's localized button names; pin names;
    /// running apps' display names).
    fn item_name(&self, index: usize) -> String {
        let language = &self.settings.language;
        let Some(item) = self.items.get(index) else {
            return String::new();
        };
        let key = match &item.kind {
            ItemKind::Start => "Start",
            ItemKind::Search => "WidgetLauncher",
            ItemKind::TaskView => "WidgetDesktops",
            ItemKind::ShowDesktop => "ShowDesktop",
            ItemKind::Tray => "TrayApps",
            ItemKind::TrayPin(pin) => return pin.title.clone(),
            // The widget's tooltip (the clock's long date).
            ItemKind::Widget(id) => {
                return self
                    .widgets
                    .get(id)
                    .and_then(|w| w.tooltip())
                    .unwrap_or_default();
            }
            ItemKind::RecycleBin => "RecycleBin",
            ItemKind::App { pin: Some(pin), .. } => return pin.name.clone(),
            ItemKind::App {
                key,
                executable_path,
                ..
            } => {
                return self.names.get(key).cloned().unwrap_or_else(|| {
                    std::path::Path::new(executable_path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            }
        };
        Catalog::get(key, language).into_owned()
    }

    /// The 9 DIP notification dot (`NotificationBrush` fill, `SurfacePopup` 1 DIP stroke), drawn once per theme.
    fn badge_brush(&mut self) -> Option<CompositionSurfaceBrush> {
        if let Some(brush) = &self.visuals.badge_brush {
            return Some(brush.clone());
        }
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        let fill = crate::tokens::color_of("NotificationBrush", dark).unwrap_or(if dark {
            ot_render::color(0xFF, 0xFF, 0x66, 0x73)
        } else {
            ot_render::color(0xFF, 0xD2, 0x2D, 0x42)
        });
        let stroke = theme::surface_popup(dark);
        let scale = self.scale() as f32;
        let size = (9.0 * scale).round().max(1.0) as u32;
        let brush = self
            .renderer
            .draw(size, size, |context| {
                let half = size as f32 / 2.0;
                let ellipse = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
                    point: Vector2 { X: half, Y: half },
                    radiusX: half - scale / 2.0,
                    radiusY: half - scale / 2.0,
                };
                // SAFETY: drawing inside BeginDraw/EndDraw.
                unsafe {
                    let brush = context.CreateSolidColorBrush(&to_d2d(fill), None)?;
                    context.FillEllipse(&ellipse, &brush);
                    let ring = context.CreateSolidColorBrush(&to_d2d(stroke), None)?;
                    context.DrawEllipse(
                        &ellipse,
                        &ring,
                        scale,
                        None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>,
                    );
                }
                Ok(())
            })
            .ok()?;
        self.visuals.badge_brush = Some(brush.clone());
        Some(brush)
    }

    /// The pointer is over `hovered` (or nothing): WPF ToolTipService timing.
    fn tip_hover(&mut self, hovered: Option<usize>) {
        if hovered == self.tip_target {
            return;
        }
        let was_shown = self.tip_shown;
        self.tip_hide();
        self.tip_target = hovered;
        if self.tip_suppressed != hovered {
            self.tip_suppressed = None;
        }
        let Some(index) = hovered else {
            return;
        };
        if self.tip_suppressed == Some(index) {
            return;
        }
        // `ToolTipService.SetIsEnabled(button, matchingWindows.Count == 0)` while previews are on.
        if self.settings.previews_enabled
            && matches!(self.items[index].kind, ItemKind::App { .. })
            && !self.items[index].windows.is_empty()
        {
            return;
        }
        let recent = was_shown
            || self
                .tip_closed_at
                .is_some_and(|t| t.elapsed().as_millis() < tooltip::BETWEEN_SHOW_MS);
        if recent {
            self.tip_show();
        } else {
            // SAFETY: timer on our own window.
            unsafe {
                wm::SetTimer(
                    self.hwnd as HWND,
                    TIMER_TIP_SHOW,
                    tooltip::SHOW_DELAY_MS,
                    None,
                )
            };
        }
    }

    fn tip_show(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_TIP_SHOW) };
        let Some(index) = self.tip_target else {
            return;
        };
        if self.drag.is_some() || menu::is_open() || self.autohide.hidden {
            return;
        }
        let name = self.item_name(index);
        if name.is_empty() {
            return;
        }
        let Some(anchor) = self.item_screen_rect(index) else {
            return;
        };
        let gap = self.flyout_gap();
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        tooltip::show(
            &self.renderer,
            &name,
            &tooltip::Placement {
                anchor,
                edge: self.settings.edge,
                gap_dips: gap,
                scale: self.scale() as f32,
            },
            &tooltip::colors(dark),
            self.animations_allowed(),
        );
        self.tip_shown = true;
        // SAFETY: timer on our own window.
        unsafe {
            wm::SetTimer(
                self.hwnd as HWND,
                TIMER_TIP_HIDE,
                tooltip::SHOW_DURATION_MS,
                None,
            )
        };
    }

    fn tip_hide(&mut self) {
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(self.hwnd as HWND, TIMER_TIP_SHOW);
            wm::KillTimer(self.hwnd as HWND, TIMER_TIP_HIDE);
        }
        if self.tip_shown {
            tooltip::hide();
            self.tip_shown = false;
            self.tip_closed_at = Some(Instant::now());
        }
    }

    /// A button press closes the tooltip; it stays closed until the pointer moves to another item.
    fn tip_press(&mut self) {
        self.tip_hide();
        self.tip_suppressed = self.tip_target;
    }

    // ---------- Hotkeys and keyboard navigation ----------

    /// `RegisterHotkeys`: the launcher hotkey opens Windows Search (until the launcher exists), the dock hotkey
    /// starts keyboard navigation. Unchanged bindings stay registered; failures are reported once per change.
    /// The session is locked: whatever was in progress ends, so nothing is left half done after unlocking (a drag,
    /// a preview, magnification, keyboard navigation, open menus and flyouts).
    fn session_locked(&mut self) {
        self.cancel_drag();
        self.keyboard_end(false);
        self.preview_hide();
        self.tip_hide();
        menu::close();
        tray_flyout::close();
        crate::panel::close();
        self.mouse_left();
        log::info("Session locked: transient dock state cleared.");
    }

    /// The session is unlocked: modifiers Windows still reports as pressed are released, the hotkeys are registered
    /// again and the windows and foreground are read again.
    fn session_unlocked(&mut self) {
        actions::release_stuck_modifiers();
        self.hotkeys = (None, None);
        self.register_hotkeys();
        // SAFETY: posting to our own window: a full window and foreground refresh.
        unsafe { wm::PostMessageW(self.hwnd as HWND, WM_TRACKER, 1, 0) };
        log::info("Session unlocked.");
    }

    fn register_hotkeys(&mut self) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
        let s = &self.settings;
        let launcher = hotkey::resolve(s.launcher_hotkey, s.custom_launcher_hotkey.as_deref());
        let dock = hotkey::resolve(s.dock_hotkey, s.custom_dock_hotkey.as_deref());
        let dock = dock.filter(|d| Some(*d) != launcher);
        if self.hotkeys == (launcher, dock) {
            return;
        }
        let hwnd = self.hwnd as HWND;
        // SAFETY: hotkeys of our own window.
        unsafe {
            UnregisterHotKey(hwnd, HOTKEY_LAUNCHER);
            UnregisterHotKey(hwnd, HOTKEY_DOCK);
        }
        const NO_REPEAT: u32 = 0x4000;
        let mut failed = false;
        let mut register = |id: i32, binding: Option<hotkey::Binding>| -> Option<hotkey::Binding> {
            let binding = binding?;
            // SAFETY: as above.
            let ok =
                unsafe { RegisterHotKey(hwnd, id, binding.modifiers | NO_REPEAT, binding.key) }
                    != 0;
            if !ok {
                // SAFETY: plain Win32 call.
                let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                log::warn(format!(
                    "RegisterHotKey {} (0x{:X}+0x{:X}) failed: {error}",
                    if id == HOTKEY_LAUNCHER {
                        "launcher"
                    } else {
                        "dock"
                    },
                    binding.modifiers,
                    binding.key
                ));
                failed = true;
            }
            ok.then_some(binding)
        };
        let registered = (
            register(HOTKEY_LAUNCHER, launcher),
            register(HOTKEY_DOCK, dock),
        );
        // A binding that failed stays unregistered here, so the next settings change retries it.
        self.hotkeys = registered;
        if failed {
            log::report_error("ErrorHotkeyRegister", &self.settings.language, None);
        }
    }

    fn hotkey(&mut self, id: i32) {
        match id {
            HOTKEY_LAUNCHER => {
                actions::open_search();
            }
            HOTKEY_DOCK => {
                if self.keyboard.is_some() {
                    self.keyboard_end(true);
                } else {
                    self.keyboard_start();
                }
            }
            _ => {}
        }
    }

    /// `StartKeyboardNavigation`: the dock shows, takes the keyboard (WM_HOTKEY grants this process the
    /// foreground) and focuses its first item.
    fn keyboard_start(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let foreground_before = tracker::foreground_application_window()
            .filter(|&w| w != 0)
            .unwrap_or(self.foreground);
        self.keyboard = Some(KeyboardNav {
            focused: 0,
            foreground_before,
            space_down: None,
        });
        if self.autohide.hidden {
            self.reveal();
        }
        self.stop_hide_timer();
        self.preview_hide();
        tray_flyout::close();
        crate::panel::close();
        self.tip_hide();
        // SAFETY: our own window.
        unsafe { wm::SetForegroundWindow(self.hwnd as HWND) };
        self.uia.set_focus(Some(0));
        self.show_focus();
    }

    /// `EndKeyboardNavigation`: focus leaves the dock; Escape and the hotkey return to the app that was in front.
    fn keyboard_end(&mut self, restore: bool) {
        let Some(nav) = self.keyboard.take() else {
            return;
        };
        self.uia.set_focus(None);
        self.show_focus();
        if restore && nav.foreground_before != 0 {
            tracker::activate(nav.foreground_before);
        }
        self.autohide_evaluate();
    }

    /// Keys while navigating (`Dock_PreviewKeyDown` and the buttons' own keys). Returns true when handled.
    fn keyboard_key(&mut self, key: u16, down: bool) -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            VK_APPS, VK_DOWN, VK_END, VK_F10, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE,
            VK_TAB, VK_UP,
        };
        let Some(nav) = self.keyboard.as_mut() else {
            return false;
        };
        let count = self.items.len();
        if count == 0 {
            return false;
        }
        let focused = nav.focused.min(count - 1);
        if !down {
            // A WPF button clicks when Space is released over it.
            if key == VK_SPACE {
                if nav.space_down.take() == Some(focused) {
                    self.keyboard_click(focused);
                }
                return true;
            }
            return false;
        }
        // SAFETY: plain Win32 calls.
        let shift = unsafe { GetKeyState(i32::from(VK_SHIFT)) } < 0;
        let vertical = self.vertical();
        let (forward, backward) = if vertical {
            (VK_DOWN, VK_UP)
        } else {
            (VK_RIGHT, VK_LEFT)
        };
        let target = match key {
            VK_ESCAPE => {
                self.keyboard_end(true);
                return true;
            }
            VK_HOME => Some(0),
            VK_END => Some(count - 1),
            k if k == forward => Some((focused + 1).min(count - 1)),
            k if k == backward => Some(focused.saturating_sub(1)),
            // Tab order runs through the buttons and wraps, as in a WPF window.
            VK_TAB if shift => Some((focused + count - 1) % count),
            VK_TAB => Some((focused + 1) % count),
            VK_RETURN => {
                self.keyboard_click(focused);
                return true;
            }
            VK_SPACE => {
                if let Some(nav) = self.keyboard.as_mut() {
                    nav.space_down = Some(focused);
                }
                return true;
            }
            VK_APPS => {
                self.keyboard_menu(focused);
                return true;
            }
            VK_F10 if shift => {
                self.keyboard_menu(focused);
                return true;
            }
            _ => None,
        };
        if let Some(target) = target {
            if let Some(nav) = self.keyboard.as_mut() {
                nav.focused = target;
                nav.space_down = None;
            }
            self.uia.set_focus(Some(target));
            self.show_focus();
            return true;
        }
        false
    }

    /// Draws the keyboard focus on the focused item (removes it when navigation ended). The highlight is a child of
    /// the item's container, below its icon, so it moves and fades with the item.
    /// Items drawn with `ShellDockButton` (hover and pressed layers): Start, Search, Task View, the tray button
    /// and the pinned tray icons. The Recycle Bin and Show Desktop use the plain `DockButton` template.
    fn has_shell_surface(&self, index: usize) -> bool {
        matches!(
            self.items.get(index).map(|i| &i.kind),
            Some(
                ItemKind::Start
                    | ItemKind::Search
                    | ItemKind::TaskView
                    | ItemKind::Tray
                    | ItemKind::TrayPin(_)
                    | ItemKind::Widget(_)
            )
        )
    }

    /// Creates, sizes and colors the shell buttons' layers after a layout change; other items lose theirs.
    fn sync_shell_surfaces(&mut self) {
        let scale = self.scale();
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        let (hover_color, pressed_color) = theme::shell_button_colors(dark);
        let start_colors = theme::start_tile_colors(dark);
        // `ControlRadius`: the dock radius less 8 (`RadiusManager`), 0 for an edge-to-edge dock.
        let square = self.placement.is_some_and(|p| p.square_corners);
        let dock_radius = self
            .settings
            .dock_corner_radius
            .unwrap_or(self.profile.radius as i32)
            .clamp(0, 48);
        let radius = if square {
            0.0
        } else {
            ((dock_radius - 8).max(0) as f64 * scale) as f32
        };
        let pin = (crate::layout::tray_pin_size(self.profile.icon) * scale).round() as f32;
        let vertical = self.vertical();
        let compositor = self.renderer.compositor.clone();
        // `WidgetHoverMaterial` / `WidgetPressedMaterial`; cards: `WidgetRadius` (the same derivation).
        let widget_colors = theme::flyout_colors(&self.settings);
        let cards: Vec<Option<(crate::widgets::Card, f32)>> = self
            .items
            .iter()
            .map(|item| match item.kind {
                ItemKind::Widget(id) => self.widgets.get(id).map(|w| {
                    (
                        w.card(dark),
                        (f64::from(w.radius(self.widget_radius())) * scale) as f32,
                    )
                }),
                _ => None,
            })
            .collect();
        let rim = (scale as f32).round().max(1.0);
        let mut backdrops_created = false;
        let result: WinResult<()> = (|| {
            for index in 0..self.visuals.items.len().min(self.items.len()) {
                let wanted = self.has_shell_surface(index);
                let is_pin = matches!(self.items[index].kind, ItemKind::TrayPin(_));
                let is_start = matches!(self.items[index].kind, ItemKind::Start);
                let card_and_radius = cards.get(index).copied().flatten();
                let card = card_and_radius.map(|(c, _)| c);
                let visual = &mut self.visuals.items[index];
                if !wanted {
                    if let Some(surface) = visual.shell.take() {
                        let children = visual.container.Children()?;
                        children.Remove(&surface.hover)?;
                        children.Remove(&surface.pressed)?;
                        if let Some((shape, _, _)) = &surface.card {
                            children.Remove(shape)?;
                        }
                        if let Some(backdrop) = &surface.backdrop {
                            children.Remove(backdrop)?;
                        }
                    }
                    continue;
                }
                if visual.shell.is_none() {
                    let layer = || -> WinResult<(ShapeVisual, CompositionRoundedRectangleGeometry, CompositionColorBrush)> {
                        let shape = compositor.CreateShapeVisual()?;
                        let geometry = compositor.CreateRoundedRectangleGeometry()?;
                        let brush = compositor.CreateColorBrush()?;
                        let sprite = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
                        sprite.SetFillBrush(&brush)?;
                        shape.Shapes()?.Append(&sprite)?;
                        shape.SetOpacity(0.0)?;
                        Ok((shape, geometry, brush))
                    };
                    let (hover, hover_geometry, hover_brush) = layer()?;
                    let (pressed, pressed_geometry, pressed_brush) = layer()?;
                    // Under the glyph: hover lowest, pressed above it.
                    let children = visual.container.Children()?;
                    children.InsertAtBottom(&pressed)?;
                    children.InsertAtBottom(&hover)?;
                    visual.shell = Some(ShellSurface {
                        hover,
                        hover_geometry,
                        hover_brush,
                        pressed,
                        pressed_geometry,
                        pressed_brush,
                        hovered: false,
                        pressed_on: false,
                        card: None,
                        backdrop: None,
                    });
                }
                let container = visual.container.Size()?;
                // A pinned tray icon's button is a square of its own size, centered across the bar; a widget's
                // layers cover its card.
                let (offset, size) = if card.is_some() {
                    let o = visual.icon.Offset()?;
                    (
                        Vector3 {
                            X: o.X,
                            Y: o.Y,
                            Z: 0.0,
                        },
                        visual.icon.Size()?,
                    )
                } else if is_pin {
                    let across = if vertical { container.X } else { container.Y };
                    let inset = ((across - pin) / 2.0).round();
                    let offset = if vertical {
                        Vector3 {
                            X: inset,
                            Y: 0.0,
                            Z: 0.0,
                        }
                    } else {
                        Vector3 {
                            X: 0.0,
                            Y: inset,
                            Z: 0.0,
                        }
                    };
                    (offset, Vector2 { X: pin, Y: pin })
                } else {
                    (
                        Vector3 {
                            X: 0.0,
                            Y: 0.0,
                            Z: 0.0,
                        },
                        container,
                    )
                };
                let r = card_and_radius
                    .map_or(radius, |(_, r)| r)
                    .min(size.X.min(size.Y) / 2.0);
                if let Some(surface) = &visual.shell {
                    for (shape, geometry) in [
                        (&surface.hover, &surface.hover_geometry),
                        (&surface.pressed, &surface.pressed_geometry),
                    ] {
                        shape.SetOffset(offset)?;
                        shape.SetSize(size)?;
                        geometry.SetSize(size)?;
                        geometry.SetCornerRadius(Vector2 { X: r, Y: r })?;
                    }
                    if card.is_some() {
                        surface.hover_brush.SetColor(widget_colors.widget_hover)?;
                        surface
                            .pressed_brush
                            .SetColor(widget_colors.widget_pressed)?;
                    } else if let (true, Some((hover, pressed))) = (is_start, start_colors) {
                        surface.hover_brush.SetColor(hover)?;
                        surface.pressed_brush.SetColor(pressed)?;
                    } else {
                        surface.hover_brush.SetColor(hover_color)?;
                        surface.pressed_brush.SetColor(pressed_color)?;
                    }
                }
                // The card itself: fill and a 1 DIP rim inside its bounds, under the hover layers.
                if let (Some(card), Some(surface)) = (card, visual.shell.as_mut()) {
                    if surface.card.is_none() {
                        let shape = compositor.CreateShapeVisual()?;
                        let geometry = compositor.CreateRoundedRectangleGeometry()?;
                        let sprite = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
                        shape.Shapes()?.Append(&sprite)?;
                        let children = visual.container.Children()?;
                        children.InsertAtBottom(&shape)?;
                        let backdrop = compositor.CreateSpriteVisual()?;
                        children.InsertAbove(&backdrop, &shape)?;
                        surface.card = Some((shape, geometry, sprite));
                        surface.backdrop = Some(backdrop);
                        backdrops_created = true;
                    }
                    if let Some(backdrop) = &surface.backdrop {
                        backdrop.SetOffset(offset)?;
                        backdrop.SetSize(size)?;
                    }
                    if let Some((shape, geometry, sprite)) = &surface.card {
                        shape.SetOffset(offset)?;
                        shape.SetSize(size)?;
                        geometry.SetOffset(Vector2 {
                            X: rim / 2.0,
                            Y: rim / 2.0,
                        })?;
                        geometry.SetSize(Vector2 {
                            X: size.X - rim,
                            Y: size.Y - rim,
                        })?;
                        let cr = (r - rim / 2.0).max(0.0);
                        geometry.SetCornerRadius(Vector2 { X: cr, Y: cr })?;
                        match card.fill {
                            crate::widgets::Fill::Solid(c) => {
                                sprite.SetFillBrush(&compositor.CreateColorBrushWithColor(c)?)?
                            }
                            crate::widgets::Fill::Diagonal(a, b) => {
                                let brush = compositor.CreateLinearGradientBrush()?;
                                brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
                                brush.SetEndPoint(Vector2 { X: 1.0, Y: 1.0 })?;
                                let stops = brush.ColorStops()?;
                                stops.Append(
                                    &compositor
                                        .CreateColorGradientStopWithOffsetAndColor(0.0, a)?,
                                )?;
                                stops.Append(
                                    &compositor
                                        .CreateColorGradientStopWithOffsetAndColor(1.0, b)?,
                                )?;
                                sprite.SetFillBrush(&brush)?;
                            }
                        }
                        sprite
                            .SetStrokeBrush(&compositor.CreateColorBrushWithColor(card.stroke)?)?;
                        sprite.SetStrokeThickness(rim)?;
                    }
                } else if let Some(surface) = visual.shell.as_mut() {
                    // The visual held a widget before the items shifted (a hidden widget, say) and now holds a
                    // shell button: its card and backdrop go, or they would stay behind the button.
                    let children = visual.container.Children()?;
                    if let Some((shape, _, _)) = surface.card.take() {
                        children.Remove(&shape)?;
                    }
                    if let Some(backdrop) = surface.backdrop.take() {
                        children.Remove(&backdrop)?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Shell button surfaces failed: {error}"));
        }
        if backdrops_created {
            // New widget backdrops: draw them now (the content keys are reset so the pair is drawn together).
            for index in 0..self.items.len() {
                if matches!(self.items[index].kind, ItemKind::Widget(_)) {
                    self.visuals.items[index].icon_key = None;
                }
            }
            self.update_icons();
        }
        // Indexes may now point at other items: the pointer decides again.
        let hovered = self.last_mouse.and_then(|(x, y)| self.hit_test(x, y));
        self.shell_hover(hovered);
    }

    /// The pointer is over `hovered` (or nothing).
    fn shell_hover(&mut self, hovered: Option<usize>) {
        self.shell_hovered = hovered.filter(|&i| self.has_shell_surface(i));
        self.update_shell_surfaces();
    }

    /// Fades the layers toward the current state: hover in 120 ms / out 180 ms, pressed (button down over the
    /// hovered button) in 80 ms / out 150 ms, linear, as the template's storyboards.
    fn update_shell_surfaces(&mut self) {
        let hovered = self.shell_hovered;
        let pressed = self
            .press
            .map(|(index, _, _)| index)
            .filter(|&i| Some(i) == hovered);
        let animate = self.animations_allowed();
        let compositor = self.renderer.compositor.clone();
        let fade = |shape: &ShapeVisual, to: f32, ms: u32| -> WinResult<()> {
            if !animate {
                shape.StopAnimation(&HSTRING::from("Opacity"))?;
                return shape.SetOpacity(to);
            }
            let animation = compositor.CreateScalarKeyFrameAnimation()?;
            // No 0.0 key: the fade starts from the current opacity, like a WPF animation with only `To`.
            animation.InsertKeyFrameWithEasingFunction(
                1.0,
                to,
                &compositor.CreateLinearEasingFunction()?,
            )?;
            animation.SetDuration(milliseconds(ms))?;
            shape.StartAnimation(&HSTRING::from("Opacity"), &animation)
        };
        let fade_eased = |shape: &ShapeVisual, to: f32, ms: u32| -> WinResult<()> {
            if !animate {
                shape.StopAnimation(&HSTRING::from("Opacity"))?;
                return shape.SetOpacity(to);
            }
            let animation = compositor.CreateScalarKeyFrameAnimation()?;
            animation.InsertKeyFrameWithEasingFunction(1.0, to, &ease_out(&compositor)?)?;
            animation.SetDuration(milliseconds(ms))?;
            shape.StartAnimation(&HSTRING::from("Opacity"), &animation)
        };
        let result: WinResult<()> = (|| {
            for (index, visual) in self.visuals.items.iter_mut().enumerate() {
                let Some(surface) = visual.shell.as_mut() else {
                    continue;
                };
                if surface.card.is_some() {
                    // `WidgetCardButton`: hover in/out 160 ms, press in 80 ms (the hover host fades out with
                    // it) and out 120 ms, cubic ease-out.
                    let want_hover = Some(index) == hovered;
                    let want_pressed = Some(index) == pressed;
                    if surface.hovered != want_hover || surface.pressed_on != want_pressed {
                        let hover_to = if want_hover && !want_pressed {
                            1.0
                        } else {
                            0.0
                        };
                        let ms = if want_pressed {
                            80
                        } else if surface.pressed_on {
                            120
                        } else {
                            160
                        };
                        fade_eased(&surface.hover, hover_to, ms)?;
                        if surface.pressed_on != want_pressed {
                            fade_eased(
                                &surface.pressed,
                                if want_pressed { 1.0 } else { 0.0 },
                                if want_pressed { 80 } else { 120 },
                            )?;
                        }
                        surface.hovered = want_hover;
                        surface.pressed_on = want_pressed;
                    }
                    continue;
                }
                let want_hover = Some(index) == hovered;
                if surface.hovered != want_hover {
                    surface.hovered = want_hover;
                    if want_hover {
                        fade(&surface.hover, 1.0, 120)?;
                    } else {
                        fade(&surface.hover, 0.0, 180)?;
                    }
                }
                let want_pressed = Some(index) == pressed;
                if surface.pressed_on != want_pressed {
                    surface.pressed_on = want_pressed;
                    if want_pressed {
                        fade(&surface.pressed, 1.0, 80)?;
                    } else {
                        fade(&surface.pressed, 0.0, 150)?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Shell button hover failed: {error}"));
        }
    }

    fn show_focus(&mut self) {
        let focused = self
            .keyboard
            .as_ref()
            .map(|nav| nav.focused.min(self.items.len().saturating_sub(1)));
        let target = focused.and_then(|i| self.visuals.items.get(i).map(|v| v.container.clone()));
        let result: WinResult<()> = (|| {
            if let Some(visual) = &mut self.focus_visual
                && let Some(parent) = visual.parent.take()
            {
                parent.Children()?.Remove(&visual.shape)?;
            }
            let Some(container) = target else {
                return Ok(());
            };
            if self.focus_visual.is_none() {
                let c = &self.renderer.compositor;
                let shape = c.CreateShapeVisual()?;
                let geometry = c.CreateRoundedRectangleGeometry()?;
                let brush = c.CreateColorBrush()?;
                let sprite = c.CreateSpriteShapeWithGeometry(&geometry)?;
                sprite.SetFillBrush(&brush)?;
                shape.Shapes()?.Append(&sprite)?;
                self.focus_visual = Some(FocusVisual {
                    shape,
                    geometry,
                    brush,
                    parent: None,
                });
            }
            let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
            let radius = (12.0 * self.scale()) as f32;
            let Some(visual) = self.focus_visual.as_mut() else {
                return Ok(());
            };
            let size = container.Size()?;
            visual.shape.SetSize(size)?;
            visual.geometry.SetSize(size)?;
            let r = radius.min(size.X.min(size.Y) / 2.0);
            visual.geometry.SetCornerRadius(Vector2 { X: r, Y: r })?;
            visual.brush.SetColor(theme::keyboard_focus_color(dark))?;
            container.Children()?.InsertAtBottom(&visual.shape)?;
            visual.parent = Some(container);
            Ok(())
        })();
        if let Err(error) = result {
            log::warn(format!("Keyboard focus drawing failed: {error}"));
        }
    }

    /// Enter or Space on an item: the same as a click (Shift starts a new instance, Ctrl+Shift elevated).
    fn keyboard_click(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let is_app = matches!(item.kind, ItemKind::App { .. });
        if is_app || matches!(item.kind, ItemKind::RecycleBin) {
            self.bounce(index);
        }
        // SAFETY: plain Win32 calls.
        let (shift, control) = unsafe {
            (
                GetKeyState(i32::from(VK_SHIFT)) < 0,
                GetKeyState(i32::from(VK_CONTROL)) < 0,
            )
        };
        // The dock holds the foreground: no app counts as "in front" for the minimize toggle.
        self.foreground_at_press = None;
        if is_app && shift {
            self.launch_new(index, control);
        } else {
            self.click(index);
        }
    }

    /// The context menu key (or Shift+F10) on an item: its menu, at the item.
    fn keyboard_menu(&mut self, index: usize) {
        let (Some((left, top, right, bottom)), Some(origin)) = (
            self.item_screen_rect(index),
            ot_shell::win::window_rect(self.hwnd),
        ) else {
            return;
        };
        let (x, y) = (
            (left + right) / 2 - origin.left,
            (top + bottom) / 2 - origin.top,
        );
        self.open_menu_at(x, y, Some((left, top)));
    }

    // ---------- Background apps (tray) ----------

    /// The live tray icons as flyout rows (`BuildLiveContent`): visible app icons in registration order.
    fn tray_entries(&self) -> Vec<tray_flyout::Entry> {
        let fallback = Catalog::get("TrayApps", &self.settings.language).into_owned();
        let pinned = self.settings.pinned_tray_icons.clone().unwrap_or_default();
        if !ot_shell::tray::is_running() {
            // `BuildContent`: the registry list (by name) when there is no live tray.
            return self
                .tray_apps
                .as_ref()
                .map(|source| source.apps())
                .unwrap_or_default()
                .iter()
                .map(|app| {
                    let matched = traypin::find_matching_path_key(&pinned, Some(&app.path));
                    tray_flyout::Entry {
                        hwnd: 0,
                        uid: app.uid,
                        name: app.name.clone(),
                        path: Some(app.path.clone()),
                        pinned: matched.is_some(),
                        pin_key: matched.unwrap_or_else(|| traypin::key_for_path(Some(&app.path))),
                        icon: app.icon.clone(),
                        fallback: true,
                    }
                })
                .collect();
        }
        ot_shell::tray::with_icons(|icons| {
            icons
                .iter()
                .filter(|icon| icon.is_app_icon())
                .map(|icon| {
                    let path = icon.executable_path.as_deref();
                    let canonical = traypin::key_for(&icon.guid, path, icon.uid);
                    let matched = traypin::find_matching_key(&pinned, &icon.guid, path, icon.uid);
                    tray_flyout::Entry {
                        hwnd: icon.hwnd,
                        uid: icon.uid,
                        name: traypin::clean_title(&icon.tip, path, &fallback),
                        path: icon.executable_path.clone(),
                        pinned: matched.is_some(),
                        pin_key: matched.unwrap_or(canonical),
                        icon: icon
                            .icon
                            .as_ref()
                            .and_then(|i| {
                                icons::from_hicon(windows::Win32::UI::WindowsAndMessaging::HICON(
                                    i.0 as *mut _,
                                ))
                            })
                            .map(Arc::new),
                        fallback: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// `OnFullScreenChanged`: a full-screen app covers the dock's monitor (the session hid the dock); nothing of the
    /// dock may stay over it: previews, tooltips, menus, the tray flyout and the magnification close.
    fn full_screen_entered(&mut self) {
        self.preview_hide();
        self.tip_hide();
        menu::close();
        tray_flyout::close();
        crate::panel::close();
        if self.magnifier.engaged {
            self.finish_magnification();
        }
    }

    // ---------- Widgets ----------

    /// Creates or drops widget instances for the settings (`WidgetCoordinator.Apply`), reads them once and starts
    /// or stops the shared tick.
    fn sync_widgets(&mut self) {
        crate::widgets::set_wake_window(self.hwnd);
        self.widgets.sync(&crate::widgets::enabled(&self.settings));
        self.widget_widths
            .retain(|id, _| self.widgets.get(id).is_some());
        if self.widgets.is_empty() {
            // SAFETY: timer on our own window.
            unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_WIDGETS) };
            return;
        }
        if self.widget_painter.is_none() {
            let inter = self
                .renderer
                .font_collection(&[
                    menu::INTER_REGULAR,
                    menu::INTER_MEDIUM,
                    menu::INTER_SEMIBOLD,
                    menu::INTER_BOLD,
                ])
                .ok();
            self.widget_painter = Some(crate::widgets::Painter::new(
                self.renderer.dwrite().clone(),
                inter,
            ));
        }
        self.update_widgets(true);
        self.schedule_widget_tick();
    }

    /// The next tick at the start of the next second, so minutes turn over on time (a plain 1 s timer, as in
    /// OpenTaskbar, drifts up to a second late).
    fn schedule_widget_tick(&self) {
        if self.widgets.is_empty() || self.autohide.hidden {
            return;
        }
        let now = crate::locale::now();
        let ms = 1000 - u32::from(now.wMilliseconds) + 5;
        // SAFETY: timer on our own window.
        unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_WIDGETS, ms, None) };
    }

    fn widget_tick(&mut self) {
        if self.autohide.hidden {
            return;
        }
        self.update_widgets(false);
        self.schedule_widget_tick();
    }

    /// Updates every widget; redraws the changed ones, relaying out the bar only when a card's width changed.
    fn update_widgets(&mut self, initial: bool) {
        if self.widgets.is_empty() {
            return;
        }
        let locale = crate::locale::Locale::new(self.settings.language.culture_name());
        let env = crate::widgets::Env {
            now: crate::locale::now(),
            locale: &locale,
            settings: &self.settings,
            language: &self.settings.language,
        };
        let shown = self.widgets.visible_ids();
        let changed = self.widgets.update(&env);
        let visibility_changed = self.widgets.visible_ids() != shown;
        if changed.is_empty() && !initial {
            return;
        }
        let mut resized = false;
        for id in self.widgets.ids() {
            if !initial && !changed.contains(&id) {
                continue;
            }
            *self.widget_generation.entry(id).or_insert(0) += 1;
            if !initial {
                // An open flyout of this widget follows it (the calendar's live time).
                crate::panel::refresh(id);
            }
            let width = match (self.widget_painter.as_ref(), self.widgets.get(id)) {
                (Some(painter), Some(widget)) => widget.width(painter).unwrap_or(80.0),
                _ => 80.0,
            };
            if self.widget_widths.insert(id, width) != Some(width) {
                resized = true;
            }
        }
        if initial {
            return;
        }
        if visibility_changed {
            // A battery appeared or went away: the bar gains or loses the card.
            self.rebuild_items();
            return;
        }
        if resized {
            if self.magnifier.engaged {
                self.widget_relayout_pending = true;
            } else {
                self.relayout();
            }
            return;
        }
        self.update_icons();
        self.sync_shell_surfaces();
    }

    /// Draws a widget's content into a surface the size of its card.
    fn draw_widget(&mut self, index: usize, id: &'static str) -> WinResult<()> {
        let generation = self.widget_generation.get(id).copied().unwrap_or(0);
        let size = self.visuals.items[index].icon.Size()?;
        let (w, h) = (size.X.round() as u32, size.Y.round() as u32);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let key = (format!("widget:{id}:{generation}:{w}x{h}"), 0);
        if self.visuals.items[index].icon_key.as_ref() == Some(&key) {
            return Ok(());
        }
        let (Some(painter), Some(widget)) = (self.widget_painter.as_ref(), self.widgets.get(id))
        else {
            return Ok(());
        };
        let scale = self.scale() as f32;
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        let card_h = h as f32 / scale;
        let card_w = w as f32 / scale;
        let radius = widget.radius(self.widget_radius());
        let setup = |context: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext| {
            // SAFETY: drawing inside BeginDraw/EndDraw.
            unsafe {
                let mut origin = ot_render::Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&ot_render::Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: origin.M31,
                    M32: origin.M32,
                });
                context.SetTextAntialiasMode(
                    windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                );
            }
        };
        if let Some(backdrop) = self.visuals.items[index]
            .shell
            .as_ref()
            .and_then(|s| s.backdrop.clone())
        {
            let mut drew = false;
            let brush = self.renderer.draw(w, h, |context| {
                setup(context);
                let canvas = crate::widgets::Canvas {
                    context,
                    painter,
                    width: card_w,
                    height: card_h,
                    scale,
                    dark,
                    radius,
                };
                drew = widget.draw_backdrop(&canvas)?;
                Ok(())
            })?;
            if drew {
                backdrop.SetBrush(&brush)?;
            } else {
                backdrop.SetBrush(None::<&ot_render::Composition::CompositionBrush>)?;
            }
        }
        let brush = self.renderer.draw(w, h, |context| {
            // SAFETY: drawing inside BeginDraw/EndDraw.
            unsafe {
                // The surface's own offset in its atlas stays; DIPs are scaled to pixels on top of it.
                let mut origin = ot_render::Matrix3x2::default();
                context.GetTransform(&mut origin);
                context.SetTransform(&ot_render::Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: origin.M31,
                    M32: origin.M32,
                });
                context.SetTextAntialiasMode(
                    windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                );
            }
            let canvas = crate::widgets::Canvas {
                context,
                painter,
                width: card_w,
                height: card_h,
                scale,
                dark,
                radius,
            };
            widget.draw(&canvas)
        })?;
        let visual = &mut self.visuals.items[index];
        visual.icon.SetBrush(&brush)?;
        visual.rest_brush = Some(brush);
        visual.hover_brush = None;
        visual.hover_key = None;
        visual.icon_key = Some(key);
        Ok(())
    }

    /// `WidgetRadius` in DIPs: the dock radius less 8, square on an edge-to-edge dock.
    fn widget_radius(&self) -> f32 {
        if self.placement.is_some_and(|p| p.square_corners) {
            return 0.0;
        }
        let dock = self
            .settings
            .dock_corner_radius
            .unwrap_or(self.profile.radius as i32)
            .clamp(0, 48);
        (dock - 8).max(0) as f32
    }

    /// `RemoveDockWidget`: drops the widget from the dock's settings and saves them.
    fn remove_widget(&mut self, id: &'static str) {
        log::info(format!("Widget removed from the dock: {id}."));
        let mut settings = self.settings.clone();
        let other = |w: &String| !w.eq_ignore_ascii_case(id);
        let token = format!("widget:{id}");
        settings.widgets = settings
            .widgets
            .map(|l| l.into_iter().filter(other).collect());
        settings.installed_widgets = settings
            .installed_widgets
            .map(|l| l.into_iter().filter(other).collect());
        settings.widget_order = settings
            .widget_order
            .map(|l| l.into_iter().filter(other).collect());
        settings.dock_item_order = settings.dock_item_order.map(|l| {
            l.into_iter()
                .filter(|t| !t.eq_ignore_ascii_case(&token))
                .collect()
        });
        persist(&settings);
        self.apply(settings);
    }

    /// `PinnedTrayStrip.UpdateFromIcons`: running icons matching a pin, in pin order (then registration order).
    fn collect_tray_pins(&self) -> Vec<TrayPin> {
        let pinned = self.settings.pinned_tray_icons.clone().unwrap_or_default();
        if !self.settings.show_tray_button || pinned.is_empty() {
            return Vec::new();
        }
        let fallback = Catalog::get("TrayApps", &self.settings.language).into_owned();
        let mut found: Vec<(usize, usize, TrayPin)> = ot_shell::tray::with_icons(|icons| {
            icons
                .iter()
                .filter(|icon| icon.is_app_icon())
                .enumerate()
                .filter_map(|(source, icon)| {
                    let path = icon.executable_path.as_deref();
                    let order = traypin::find_pin_index(&pinned, &icon.guid, path, icon.uid)?;
                    let key = traypin::find_matching_key(&pinned, &icon.guid, path, icon.uid)
                        .unwrap_or_else(|| traypin::key_for(&icon.guid, path, icon.uid));
                    let image = icon.icon.as_ref().and_then(|i| {
                        icons::from_hicon(windows::Win32::UI::WindowsAndMessaging::HICON(
                            i.0 as *mut _,
                        ))
                    });
                    let icon_hash = image.as_ref().map_or(0, |i| {
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        (i.width, i.height, &i.pixels).hash(&mut hasher);
                        hasher.finish()
                    });
                    Some((
                        order,
                        source,
                        TrayPin {
                            key,
                            title: traypin::clean_title(&icon.tip, path, &fallback),
                            hwnd: icon.hwnd,
                            uid: icon.uid,
                            path: icon.executable_path.clone(),
                            icon: image.map(Arc::new),
                            icon_hash,
                            fallback: false,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
        // Pins whose app runs without a live icon come from the merged snapshot (`TrayIconSource.Merge`: live apps,
        // then registry apps of other paths), after every live icon.
        let live_pinned: std::collections::HashSet<String> = found
            .iter()
            .filter_map(|(_, _, pin)| pin.path.as_deref().map(|p| p.trim().to_lowercase()))
            .collect();
        let live_count =
            ot_shell::tray::with_icons(|icons| icons.iter().filter(|i| i.is_app_icon()).count())
                .unwrap_or(0);
        for (index, app) in self.tray_snapshot().into_iter().enumerate() {
            if app.path.trim().is_empty() || live_pinned.contains(&app.path.trim().to_lowercase()) {
                continue;
            }
            let Some(key) = traypin::find_matching_path_key(&pinned, Some(&app.path)) else {
                continue;
            };
            let Some(order) = pinned.iter().position(|k| k.eq_ignore_ascii_case(&key)) else {
                continue;
            };
            found.push((
                order,
                live_count + index,
                TrayPin {
                    key,
                    title: app.name,
                    hwnd: 0,
                    uid: app.uid,
                    path: Some(app.path),
                    icon_hash: app.icon_hash,
                    icon: app.icon,
                    fallback: true,
                },
            ));
        }
        found.sort_by_key(|(order, source, _)| (*order, *source));
        found.into_iter().map(|(_, _, pin)| pin).collect()
    }

    /// `TrayIconSource` snapshot: live icons with a path (one per identity), then running registry apps of other
    /// paths, sorted by name.
    fn tray_snapshot(&self) -> Vec<crate::trayapps::TrayApp> {
        use crate::trayapps::{TrayApp, identity};
        let mut seen = std::collections::HashSet::new();
        let mut live_paths = std::collections::HashSet::new();
        let mut apps: Vec<TrayApp> = ot_shell::tray::with_icons(|icons| {
            icons
                .iter()
                .filter(|icon| icon.is_app_icon())
                .filter_map(|icon| {
                    let path = icon.executable_path.as_deref()?.trim();
                    if path.is_empty() || !seen.insert(identity(&icon.guid, path, icon.uid)) {
                        return None;
                    }
                    live_paths.insert(path.to_lowercase());
                    let stem = std::path::Path::new(path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let image = icon.icon.as_ref().and_then(|i| {
                        icons::from_hicon(windows::Win32::UI::WindowsAndMessaging::HICON(
                            i.0 as *mut _,
                        ))
                    });
                    Some(TrayApp {
                        name: traypin::clean_title(&icon.tip, Some(path), &stem),
                        path: path.to_owned(),
                        icon_hash: image.as_ref().map_or(0, crate::trayapps::icon_hash),
                        icon: image.map(Arc::new),
                        guid: icon.guid.clone(),
                        uid: icon.uid,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
        if let Some(source) = &self.tray_apps {
            for app in source.apps().iter() {
                let path = app.path.trim();
                if path.is_empty()
                    || live_paths.contains(&path.to_lowercase())
                    || !seen.insert(identity(&app.guid, path, app.uid))
                {
                    continue;
                }
                apps.push(app.clone());
            }
        }
        apps.sort_by_cached_key(|a| a.name.to_lowercase());
        apps
    }

    /// Tray icons or pins changed: the strip is rebuilt only when what it shows changed.
    fn tray_changed(&mut self) {
        // `OnSharedSnapshotChanged`: without a live tray, an open flyout shows the new registry list.
        if tray_flyout::is_open() && !ot_shell::tray::is_running() {
            tray_flyout::refresh(self.tray_entries());
        }
        let pins = self.collect_tray_pins();
        if pins != self.tray_pins {
            self.tray_pins = pins;
            self.rebuild_items();
        }
    }

    /// A pinned tray icon's left click: path pins activate the app's window or launch it (`TrayIcons.Open`), others
    /// forward the click to the icon.
    fn open_tray_pin(&self, pin: &TrayPin) {
        let path = pin.path.as_deref().map(str::trim);
        if !pin.fallback && (!traypin::opens_by_path(&pin.key) || !traypin::can_open_by_path(path))
        {
            ot_shell::tray::click(pin.hwnd, pin.uid, ot_shell::tray::Button::Left);
            return;
        }
        if let Some(path) = path {
            open_by_path(path);
        }
    }

    /// The context menu of a background app in the fallback flyout: Close application, End process (at the
    /// pointer, as a WPF ContextMenu opens). The flyout stays open while it is shown.
    fn background_app_menu(&mut self, entry: tray_flyout::Entry) {
        let Some(path) = entry.path else {
            return;
        };
        let Some(cursor) = autohide::cursor() else {
            return;
        };
        let language = self.settings.language.clone();
        let text = |key: &str| Catalog::get(key, &language).into_owned();
        let entries = vec![
            menu::Entry::Item {
                command: MenuCommand::CloseApp as u32,
                label: text("CloseApplication"),
                icon: Some("Close"),
                destructive: false,
                enabled: true,
            },
            menu::Entry::Item {
                command: MenuCommand::Terminate as u32,
                label: text("Terminate"),
                icon: Some("Terminate"),
                destructive: true,
                enabled: true,
            },
        ];
        self.menu_target = Some(MenuTarget {
            name: entry.name,
            executable: path,
            pin: None,
            windows: Vec::new(),
            owners: Vec::new(),
            foreground: false,
            background: true,
        });
        let s = &self.settings;
        let colors = theme::menu_colors(
            theme::tone(s.theme),
            s.menu_background,
            s.menu_opacity,
            s.menu_border_thickness,
            s.menu_border_color.as_deref(),
        );
        menu::open(
            &self.renderer,
            self.hwnd,
            cursor,
            self.scale() as f32,
            entries,
            0,
            colors,
        );
    }

    /// The tray button: opens the flyout, or closes it when it is open.
    fn toggle_tray_flyout(&mut self, index: usize) {
        if tray_flyout::is_open() {
            tray_flyout::close();
            return;
        }
        let Some(target) = self.item_screen_rect(index) else {
            return;
        };
        self.preview_hide();
        crate::panel::close();
        self.tip_hide();
        let language = self.settings.language.clone();
        let text = |key: &str| Catalog::get(key, &language).into_owned();
        tray_flyout::show(
            &self.renderer,
            tray_flyout::Request {
                owner: self.hwnd,
                target,
                edge: self.settings.edge,
                gap_dips: self.flyout_gap(),
                scale: self.scale() as f32,
                colors: theme::flyout_colors(&self.settings),
                strings: tray_flyout::Strings {
                    heading: text("TrayApps"),
                    empty: text("TrayEmpty"),
                    pin: text("PinTrayIcon"),
                    unpin: text("UnpinTrayIcon"),
                    terminate: text("Terminate"),
                },
                entries: self.tray_entries(),
                animate: self.animations_allowed(),
            },
        );
    }

    /// A widget's flyout (`DockFlyout.Toggle`): opens it beside the card, or closes it when it is open.
    fn toggle_widget_flyout(&mut self, index: usize, id: &'static str) {
        if crate::panel::is_open(Some(id)) {
            crate::panel::close();
            return;
        }
        // `PlacementTarget` is the card button itself, not its slot.
        let Some(target) = self.widget_card_screen_rect(index) else {
            return;
        };
        let Some(painter) = self.widget_painter.clone() else {
            return;
        };
        let body: Box<dyn crate::panel::Content> = match id {
            "Audio" => Box::new(crate::widgets::audio_flyout::AudioFlyout::new(
                self.settings.language.clone(),
            )),
            "Clock" => Box::new(crate::widgets::calendar_flyout::CalendarFlyout::new(
                &self.settings,
            )),
            _ => return,
        };
        self.preview_hide();
        self.tip_hide();
        crate::panel::show(
            &self.renderer,
            id,
            body,
            crate::panel::Request {
                owner: self.hwnd,
                target,
                edge: self.settings.edge,
                gap_dips: self.flyout_gap(),
                scale: self.scale() as f32,
                colors: theme::flyout_colors(&self.settings),
                palette: theme::panel_palette(&self.settings),
                painter,
                animate: self.animations_allowed(),
            },
        );
    }

    /// Row actions from the flyout.
    fn tray_flyout_action(&mut self, action: usize, index: usize) {
        if action == tray_flyout::ACTION_CLOSED {
            self.autohide_evaluate();
            return;
        }
        let Some(entry) = tray_flyout::entry(index) else {
            return;
        };
        match action {
            tray_flyout::ACTION_OPEN if entry.fallback => {
                if let Some(path) = &entry.path {
                    open_by_path(path);
                }
            }
            tray_flyout::ACTION_OPEN => {
                ot_shell::tray::click(entry.hwnd, entry.uid, ot_shell::tray::Button::Left)
            }
            tray_flyout::ACTION_MENU if entry.fallback => self.background_app_menu(entry),
            tray_flyout::ACTION_MENU => {
                ot_shell::tray::click(entry.hwnd, entry.uid, ot_shell::tray::Button::Right)
            }
            tray_flyout::ACTION_PIN => {
                if entry.pin_key.is_empty() {
                    return;
                }
                self.settings.pinned_tray_icons =
                    traypin::toggle(self.settings.pinned_tray_icons.as_deref(), &entry.pin_key);
                persist(&self.settings);
                tray_flyout::refresh(self.tray_entries());
                self.tray_changed();
            }
            tray_flyout::ACTION_QUIT => {
                let Some(path) = entry.path else {
                    return;
                };
                self.terminate(&MenuTarget {
                    name: entry.name,
                    executable: path,
                    pin: None,
                    windows: Vec::new(),
                    owners: Vec::new(),
                    foreground: false,
                    background: true,
                });
            }
            _ => {}
        }
    }

    // ---------- Window previews ----------

    /// `DockMetrics.ComputeFlyoutGap`: anchor inset + 7 - 4, kept within 5..10 DIP.
    fn flyout_gap(&self) -> f32 {
        (self.profile.vertical_padding + 1.0 + 7.0 - 4.0).clamp(5.0, 10.0) as f32
    }

    /// The item's button on screen: its slot inside the bar, as laid out now (magnified or not).
    fn item_screen_rect(&self, index: usize) -> Option<(i32, i32, i32, i32)> {
        let visual = self.visuals.items.get(index)?;
        let origin = ot_shell::win::window_rect(self.hwnd)?;
        let bar = self.visuals.bar.Offset().unwrap_or_default();
        let offset = visual.container.Offset().unwrap_or_default();
        let size = visual.container.Size().unwrap_or_default();
        let left = origin.left + (bar.X + offset.X) as i32;
        let top = origin.top + (bar.Y + offset.Y) as i32;
        Some((left, top, left + size.X as i32, top + size.Y as i32))
    }

    /// A widget's card on screen (inside its slot).
    fn widget_card_screen_rect(&self, index: usize) -> Option<(i32, i32, i32, i32)> {
        let (left, top, _, _) = self.item_screen_rect(index)?;
        let icon = &self.visuals.items.get(index)?.icon;
        let offset = icon.Offset().ok()?;
        let size = icon.Size().ok()?;
        let (l, t) = (
            left + offset.X.round() as i32,
            top + offset.Y.round() as i32,
        );
        Some((l, t, l + size.X.round() as i32, t + size.Y.round() as i32))
    }

    /// `DockApp_MouseEnter` / `DockApp_MouseLeave`: leaving an app starts the 500 ms close delay; entering one with
    /// windows starts the open delay (300 ms, 140 ms while a preview is up).
    fn preview_hover(&mut self, hovered: Option<usize>) {
        let app = hovered.filter(|&i| {
            matches!(
                self.items.get(i).map(|item| &item.kind),
                Some(ItemKind::App { .. })
            )
        });
        if app == self.preview_item {
            return;
        }
        let hwnd = self.hwnd as HWND;
        if self.preview_item.is_some() {
            self.preview_pending = None;
            // SAFETY: timers on our own window.
            unsafe {
                wm::KillTimer(hwnd, TIMER_PREVIEW_OPEN);
                wm::SetTimer(hwnd, TIMER_PREVIEW_CLOSE, preview::CLOSE_DELAY_MS, None);
            }
        }
        self.preview_item = app;
        let Some(index) = app else {
            return;
        };
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(hwnd, TIMER_PREVIEW_CLOSE);
            wm::KillTimer(hwnd, TIMER_PREVIEW_OPEN);
        }
        self.preview_pending = None;
        if !self.settings.previews_enabled || self.items[index].windows.is_empty() {
            preview::hide();
            self.preview_key = None;
            return;
        }
        self.preview_pending = Some(index);
        let delay = if preview::is_visible() {
            preview::SWITCH_DELAY_MS
        } else {
            preview::OPEN_DELAY_MS
        };
        // SAFETY: timer on our own window.
        unsafe { wm::SetTimer(hwnd, TIMER_PREVIEW_OPEN, delay, None) };
    }

    fn preview_open_elapsed(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_OPEN) };
        let Some(index) = self.preview_pending.take() else {
            return;
        };
        if self.preview_item != Some(index)
            || !self.settings.previews_enabled
            || menu::is_open()
            || self.drag.is_some()
            || self.autohide.hidden
        {
            return;
        }
        if self.items[index].windows.is_empty() {
            preview::hide();
            self.preview_key = None;
            return;
        }
        self.show_preview(index, true);
    }

    fn preview_close_elapsed(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_CLOSE) };
        preview::hide();
        self.preview_key = None;
        self.autohide_evaluate();
    }

    /// Closes the preview and forgets pending opens (menus, drags, settings changes).
    fn preview_hide(&mut self) {
        // SAFETY: timers on our own window.
        unsafe {
            wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_OPEN);
            wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_CLOSE);
        }
        self.preview_pending = None;
        preview::hide();
        self.preview_key = None;
    }

    fn recycle_bin_name(&mut self) -> Option<String> {
        self.recycle_bin_name
            .get_or_insert_with(|| crate::app_name::shell_display_name(icons::RECYCLE_BIN))
            .clone()
    }

    /// `PreviewHoverTimer_Tick`: the item's windows with their titles; Explorer windows showing the Recycle Bin get
    /// its name and icon when several Explorer windows are open (`BuildThumbnailOverrides`).
    fn show_preview(&mut self, index: usize, allow_entrance: bool) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let ItemKind::App {
            executable_path, ..
        } = &item.kind
        else {
            return;
        };
        let handles = item.windows.clone();
        let key = item_key(item);
        let explorer = handles.len() >= 2
            && std::path::Path::new(executable_path)
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe"));
        let snapshot = self
            .tracker
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default();
        let icon = self
            .visuals
            .items
            .get(index)
            .and_then(|v| v.rest_brush.clone());
        let recycle = if explorer {
            self.recycle_bin_name().map(|name| {
                let brush = self
                    .items
                    .iter()
                    .position(|i| i.kind == ItemKind::RecycleBin)
                    .and_then(|i| self.visuals.items.get(i))
                    .and_then(|v| v.rest_brush.clone());
                (name, brush)
            })
        } else {
            None
        };
        let cards = handles
            .iter()
            .map(|&handle| {
                let title = snapshot
                    .iter()
                    .find(|w| w.handle == handle)
                    .map(|w| w.title.clone())
                    .unwrap_or_default();
                match &recycle {
                    Some((name, brush)) if title.to_lowercase().contains(&name.to_lowercase()) => {
                        preview::Card {
                            handle,
                            title: name.clone(),
                            icon: brush.clone().or_else(|| icon.clone()),
                        }
                    }
                    _ => preview::Card {
                        handle,
                        title,
                        icon: icon.clone(),
                    },
                }
            })
            .collect();
        let Some(anchor) = self.item_screen_rect(index) else {
            return;
        };
        let extra_gap_dips = if self.settings.magnification_enabled {
            fisheye::preview_clearance(
                self.profile.icon,
                self.magnifier.effective_max(),
                preview::GAP_DIPS,
            )
        } else {
            0.0
        };
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        let language = &self.settings.language;
        let animations = self.animations_allowed();
        let shown = preview::show(
            &self.renderer,
            preview::Request {
                owner: self.hwnd,
                cards,
                anchor,
                edge: self.settings.edge,
                extra_gap_dips,
                scale: self.scale(),
                colors: preview::colors(dark, theme::windows_accent()),
                placeholder: Catalog::get("PreviewUnavailable", language).into_owned(),
                close_label: Catalog::get("CloseWindow", language).into_owned(),
                animate: allow_entrance && animations,
                animations,
                tip_gap_dips: self.flyout_gap(),
            },
        );
        self.preview_key = shown.then_some(key);
    }

    /// Messages from the preview window.
    fn preview_event(&mut self, event: usize, handle: LPARAM) {
        let hwnd = self.hwnd as HWND;
        match event {
            preview::EVENT_ENTER => {
                // SAFETY: timer on our own window.
                unsafe { wm::KillTimer(hwnd, TIMER_PREVIEW_CLOSE) };
            }
            preview::EVENT_LEAVE => {
                // SAFETY: timer on our own window.
                unsafe { wm::SetTimer(hwnd, TIMER_PREVIEW_CLOSE, preview::CLOSE_DELAY_MS, None) };
            }
            preview::EVENT_ACTIVATE => {
                // SAFETY: timer on our own window.
                unsafe { wm::KillTimer(hwnd, TIMER_PREVIEW_CLOSE) };
                preview::hide();
                self.preview_key = None;
                self.autohide_evaluate();
                if handle == 0 || !tracker::activate(handle) {
                    log::warn("ErrorPreviewActivate: the previewed window could not be activated.");
                }
            }
            preview::EVENT_CLOSE => {
                // The preview stays: the window list update removes the card once the window is gone.
                // SAFETY: timer on our own window.
                unsafe { wm::KillTimer(hwnd, TIMER_PREVIEW_CLOSE) };
                if handle == 0 || !tracker::request_close(handle) {
                    log::warn("ErrorPreviewClose: no close request reached the previewed window.");
                }
            }
            preview::EVENT_REBUILD => {
                let index = self
                    .preview_key
                    .as_ref()
                    .and_then(|key| self.items.iter().position(|item| &item_key(item) == key));
                match index {
                    Some(index)
                        if preview::is_visible() && !self.items[index].windows.is_empty() =>
                    {
                        self.show_preview(index, false);
                    }
                    _ => {
                        preview::hide();
                        self.preview_key = None;
                        self.autohide_evaluate();
                    }
                }
            }
            _ => {}
        }
    }

    /// The window list changed: cards of windows that closed fade out and the preview shows the rest.
    fn preview_windows_changed(&mut self) {
        if !preview::is_visible() {
            return;
        }
        let snapshot = self
            .tracker
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default();
        let closed: Vec<isize> = preview::handles()
            .into_iter()
            .filter(|h| snapshot.iter().all(|w| w.handle != *h))
            .collect();
        if !closed.is_empty() {
            preview::remove_closed(&closed, self.animations_allowed());
        }
    }

    // ---------- Context menus ----------

    /// Right-click: the app menu over an app (`AppContextMenu`), the dock menu anywhere else (`DockContextMenu`).
    fn open_menu(&mut self, x: i32, y: i32) {
        tray_flyout::close();
        crate::panel::close();
        let cursor = autohide::cursor();
        self.open_menu_at(x, y, cursor);
    }

    /// The context menu for the item at client point (`x`, `y`), opened at screen point `at`.
    fn open_menu_at(&mut self, x: i32, y: i32, at: Option<(i32, i32)>) {
        // A widget's menu: Remove widget (`WidgetCoordinator.CreateDefaultWidgetMenu`).
        if let Some(index) = self.hit_test(x, y)
            && let ItemKind::Widget(id) = self.items[index].kind
        {
            self.shell_hover(None);
            self.tip_press();
            self.preview_hide();
            self.release_magnification();
            let Some(cursor) = at else {
                return;
            };
            self.menu_widget = Some(id);
            let s = &self.settings;
            let colors = theme::menu_colors(
                theme::tone(s.theme),
                s.menu_background,
                s.menu_opacity,
                s.menu_border_thickness,
                s.menu_border_color.as_deref(),
            );
            let entries = vec![menu::Entry::Item {
                command: MenuCommand::RemoveWidget as u32,
                label: Catalog::get("RemoveWidget", &self.settings.language).into_owned(),
                icon: Some("Trash"),
                destructive: false,
                enabled: true,
            }];
            menu::open(
                &self.renderer,
                self.hwnd,
                cursor,
                self.scale() as f32,
                entries,
                0,
                colors,
            );
            return;
        }
        // The open menu takes the mouse; the button under it is no longer "under the mouse" (WPF IsMouseOver).
        self.shell_hover(None);
        // Pinned tray icons answer right-clicks themselves (`ContextMenuOpening` handled).
        if let Some(index) = self.hit_test(x, y)
            && let ItemKind::TrayPin(pin) = &self.items[index].kind
        {
            let pin = pin.clone();
            self.tip_press();
            if pin.fallback {
                // A registry app has no icon to ask; the right button opens it as well.
                self.open_tray_pin(&pin);
            } else {
                ot_shell::tray::click(pin.hwnd, pin.uid, ot_shell::tray::Button::Right);
            }
            return;
        }
        self.tip_press();
        self.preview_hide();
        // `ResetMagnification` in OpenTaskbar's DockContextMenu_Opened: the icons spring back.
        self.release_magnification();
        let language = self.settings.language.clone();
        let text = |key: &str| Catalog::get(key, &language).into_owned();
        let item = |command: MenuCommand, key: &str, icon: &'static str| menu::Entry::Item {
            command: command as u32,
            label: text(key),
            icon: Some(icon),
            destructive: false,
            enabled: true,
        };
        let target = self.hit_test(x, y).and_then(|index| {
            let item = self.items.get(index)?;
            let ItemKind::App {
                pin,
                executable_path,
                ..
            } = &item.kind
            else {
                return None;
            };
            Some(MenuTarget {
                // `app.Name`: the pin's name, else the display name the dock shows (FileDescription, packaged
                // name), which also names a new pin's shortcut.
                name: self.item_name(index),
                executable: executable_path.clone(),
                pin: pin.as_deref().cloned(),
                windows: item.windows.clone(),
                owners: item
                    .windows
                    .iter()
                    .map(|&w| ot_shell::win::process_id(w))
                    .collect(),
                foreground: item.foreground,
                background: false,
            })
        });
        let mut entries = match &target {
            Some(app) => {
                let running = !app.windows.is_empty();
                let open_key = if !running {
                    "OpenApplication"
                } else if app.foreground {
                    "MinimizeWindow"
                } else {
                    "ActivateWindow"
                };
                // Hide in profile and Add to group arrive with profiles and groups; jump lists with their own step.
                let mut entries = vec![item(MenuCommand::Activate, open_key, "Open")];
                entries.push(if app.pin.is_some() {
                    item(MenuCommand::Unpin, "Unpin", "Unpin")
                } else {
                    item(MenuCommand::Pin, "Pin", "Pin")
                });
                entries.extend([
                    item(MenuCommand::NewInstance, "NewInstance", "Plus"),
                    item(MenuCommand::RunAsAdmin, "RunAsAdmin", "Shield"),
                ]);
                if running {
                    entries.push(menu::Entry::Separator);
                    entries.push(item(MenuCommand::CloseApp, "CloseApplication", "Close"));
                    entries.push(menu::Entry::Item {
                        command: MenuCommand::Terminate as u32,
                        label: text("Terminate"),
                        icon: Some("Terminate"),
                        destructive: true,
                        enabled: true,
                    });
                }
                entries
            }
            // Launcher, stacks, groups, profiles, focus mode, updates and Settings arrive with those features.
            None => vec![
                item(MenuCommand::TaskManager, "TaskManager", "TaskManager"),
                menu::Entry::Separator,
                item(MenuCommand::FileExplorer, "OpenFileExplorer", "Folder"),
                item(MenuCommand::Run, "OpenRunDialog", "Search"),
                item(MenuCommand::Terminal, "OpenTerminal", "Terminal"),
                item(MenuCommand::TerminalAdmin, "OpenTerminalAdmin", "Terminal"),
                item(MenuCommand::DeviceManager, "OpenDeviceManager", "Cpu"),
                item(
                    MenuCommand::DiskManagement,
                    "OpenDiskManagement",
                    "HardDrive",
                ),
                item(
                    MenuCommand::ComputerManagement,
                    "OpenComputerManagement",
                    "Monitor",
                ),
                item(MenuCommand::EventViewer, "OpenEventViewer", "Clock"),
                item(
                    MenuCommand::NetworkConnections,
                    "OpenNetworkConnections",
                    "Wifi",
                ),
                menu::Entry::Separator,
                item(MenuCommand::Lock, "LockWorkstation", "Exit"),
                item(MenuCommand::SignOut, "SignOut", "Exit"),
                item(MenuCommand::Restart, "RestartComputer", "Refresh"),
                item(MenuCommand::ShutDown, "ShutDownComputer", "Exit"),
                menu::Entry::Separator,
                item(MenuCommand::Settings, "Settings", "Settings"),
                menu::Entry::Separator,
                item(MenuCommand::Exit, "Exit", "Exit"),
            ],
        };
        // `LoadJumpListAsync`: the app's jump list fills the section above the commands once it is read.
        let leading = if let Some(app) = &target {
            let window = if self.foreground != 0 && app.windows.contains(&self.foreground) {
                self.foreground
            } else {
                app.windows.first().copied().unwrap_or(0)
            };
            let shortcut = app
                .pin
                .as_ref()
                .map(|p| p.shortcut_path.to_string_lossy().into_owned());
            let titles = jumplist::Titles {
                pinned: text("PinnedDestinations"),
                frequent: text("Frequent"),
                recent: text("Recent"),
                tasks: text("Tasks"),
            };
            if let Some(previous) = self.jump_session.take() {
                self.jump_lists.release(previous);
            }
            let icon_px = application_icon_pixels(self.profile.icon, self.scale());
            self.jump_session = Some(self.jump_lists.read(window, shortcut, titles, icon_px));
            self.jump_loading = true;
            // SAFETY: timer on our own window.
            unsafe { wm::SetTimer(self.hwnd as HWND, TIMER_JUMPLIST, JUMPLIST_TIMEOUT_MS, None) };
            let leading = vec![status_entry(text("TasksLoading")), menu::Entry::Separator];
            let count = leading.len();
            entries.splice(0..0, leading);
            count
        } else {
            0
        };
        self.menu_target = target;
        let s = &self.settings;
        let colors = theme::menu_colors(
            theme::tone(s.theme),
            s.menu_background,
            s.menu_opacity,
            s.menu_border_thickness,
            s.menu_border_color.as_deref(),
        );
        let Some(cursor) = at else {
            return;
        };
        menu::open(
            &self.renderer,
            self.hwnd,
            cursor,
            self.scale() as f32,
            entries,
            leading,
            colors,
        );
    }

    /// Finished jump list work from the worker thread.
    fn jump_lists_ready(&mut self) {
        for outcome in self.jump_lists.drain() {
            match outcome {
                jumplist::Outcome::Read(list) => {
                    if self.jump_session == Some(list.session)
                        && self.jump_loading
                        && menu::is_open()
                    {
                        self.jump_loading = false;
                        // SAFETY: timer on our own window.
                        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_JUMPLIST) };
                        menu::set_leading(self.jump_entries(&list));
                    } else {
                        self.jump_lists.release(list.session);
                    }
                }
                jumplist::Outcome::ServiceFailed(session) => {
                    log::warn("Shell worker failed: COM could not start on the jump list thread.");
                    if self.jump_session == Some(session) && self.jump_loading {
                        self.jump_loading = false;
                        // SAFETY: timer on our own window.
                        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_JUMPLIST) };
                        let message = Catalog::get("TasksServiceError", &self.settings.language);
                        menu::set_leading(vec![
                            status_entry(message.into_owned()),
                            menu::Entry::Separator,
                        ]);
                    }
                }
                jumplist::Outcome::Invoked { ok, .. } => {
                    if !ok {
                        log::report_error("ErrorAppTaskRun", &self.settings.language, None);
                    }
                }
            }
        }
    }

    /// The menu section for a read jump list: each group under its heading, groups apart by dividers, a status
    /// line when there is nothing to show, then the divider above the app's commands.
    fn jump_entries(&self, list: &jumplist::JumpList) -> Vec<menu::Entry> {
        let mut entries = Vec::new();
        for group in &list.groups {
            if !entries.is_empty() {
                entries.push(menu::Entry::Separator);
            }
            entries.push(menu::Entry::Heading(group.title.clone()));
            for entry in &group.entries {
                entries.push(match entry {
                    jumplist::Entry::Separator => menu::Entry::Separator,
                    jumplist::Entry::Item {
                        command,
                        title,
                        icon,
                    } => menu::Entry::Task {
                        command: JUMPLIST_COMMAND + command,
                        label: title.clone(),
                        image: icon.clone(),
                    },
                });
            }
        }
        if list.status != jumplist::Status::Ready {
            let key = match list.status {
                jumplist::Status::Empty => "TasksEmpty",
                jumplist::Status::IdentityUnavailable => "TasksIdentityUnavailable",
                jumplist::Status::Unsupported => "TasksUnsupported",
                _ => "TasksReadError",
            };
            entries.push(status_entry(
                Catalog::get(key, &self.settings.language).into_owned(),
            ));
        }
        entries.push(menu::Entry::Separator);
        entries
    }

    fn jump_list_timeout(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_JUMPLIST) };
        if !self.jump_loading {
            return;
        }
        self.jump_loading = false;
        self.jump_lists.cancel();
        if menu::is_open() {
            let message = Catalog::get("TasksTimeout", &self.settings.language);
            menu::set_leading(vec![
                status_entry(message.into_owned()),
                menu::Entry::Separator,
            ]);
        }
    }

    /// The menu closed: a read in progress is dropped and the entries' Shell objects are released.
    fn menu_closed(&mut self) {
        // SAFETY: timer on our own window.
        unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_JUMPLIST) };
        self.jump_loading = false;
        self.jump_lists.cancel();
        if let Some(session) = self.jump_session.take() {
            self.jump_lists.release(session);
        }
        self.autohide_evaluate();
    }

    fn menu_command(&mut self, command: usize) {
        if command >= JUMPLIST_COMMAND as usize {
            if let Some(session) = self.jump_session {
                let entry = command as u32 - JUMPLIST_COMMAND;
                self.jump_lists.invoke(session, entry, self.hwnd);
            }
            return;
        }
        let Some(command) = MenuCommand::from(command) else {
            return;
        };
        let target = self.menu_target.take();
        let language = self.settings.language.clone();
        if command == MenuCommand::RemoveWidget {
            if let Some(id) = self.menu_widget.take() {
                self.remove_widget(id);
            }
            return;
        }
        match command {
            MenuCommand::Activate => {
                let Some(app) = target else { return };
                let in_front = (self.foreground != 0 && app.windows.contains(&self.foreground))
                    .then_some(self.foreground);
                match in_front.or_else(|| app.windows.first().copied()) {
                    Some(window) => {
                        tracker::toggle_activation(
                            window,
                            Some(&app.executable),
                            app.foreground.then_some(window),
                        );
                    }
                    None => match &app.pin {
                        Some(pin) => actions::launch_pin(pin),
                        None => actions::launch_executable(&app.executable),
                    },
                }
            }
            MenuCommand::NewInstance => {
                let Some(app) = target else { return };
                match &app.pin {
                    Some(pin) => actions::launch_pin(pin),
                    None => actions::launch_executable(&app.executable),
                }
            }
            MenuCommand::RunAsAdmin => {
                let Some(app) = target else { return };
                let launch = app.pin.as_ref().map_or(app.executable.clone(), |p| {
                    p.shortcut_path.to_string_lossy().into_owned()
                });
                actions::launch_elevated(&launch);
            }
            MenuCommand::CloseApp => {
                let Some(app) = target else { return };
                if app.background {
                    tray_flyout::close();
                    let (closed, failed) = actions::close_main_windows(&app.executable);
                    if closed == 0 {
                        log::report_error("ErrorAppGracefulClose", &language, None);
                    } else if failed > 0 {
                        log::report_error("ErrorAppPartialGracefulClose", &language, None);
                    }
                } else if actions::close_windows(&app.windows, &app.owners) == 0 {
                    log::report_error("ErrorAppGracefulClose", &language, None);
                }
            }
            MenuCommand::Terminate => {
                let Some(app) = target else { return };
                if app.background {
                    tray_flyout::close();
                }
                self.terminate(&app);
            }
            MenuCommand::TaskManager => actions::open_task_manager(),
            MenuCommand::Settings => {
                // SAFETY: posting to our own window.
                unsafe { wm::PostMessageW(self.hwnd as HWND, WM_OPEN_SETTINGS, 0, 0) };
            }
            MenuCommand::FileExplorer => actions::open_system_tool("explorer.exe", None, false),
            MenuCommand::Run => {
                actions::open_run_dialog();
            }
            MenuCommand::Terminal => actions::open_system_tool("wt.exe", None, false),
            MenuCommand::TerminalAdmin => actions::open_system_tool("wt.exe", None, true),
            MenuCommand::DeviceManager => actions::open_system_tool("devmgmt.msc", None, false),
            MenuCommand::DiskManagement => actions::open_system_tool("diskmgmt.msc", None, false),
            MenuCommand::ComputerManagement => {
                actions::open_system_tool("compmgmt.msc", None, false)
            }
            MenuCommand::EventViewer => actions::open_system_tool("eventvwr.msc", None, false),
            MenuCommand::NetworkConnections => {
                actions::open_system_tool("control.exe", Some("netconnections"), false)
            }
            MenuCommand::Lock => {
                actions::open_system_tool("rundll32.exe", Some("user32.dll,LockWorkStation"), false)
            }
            MenuCommand::SignOut => actions::open_system_tool("shutdown.exe", Some("/l"), false),
            MenuCommand::Restart => {
                actions::open_system_tool("shutdown.exe", Some("/r /t 0"), false)
            }
            MenuCommand::ShutDown => {
                actions::open_system_tool("shutdown.exe", Some("/s /t 0"), false)
            }
            MenuCommand::RemoveWidget => {}
            MenuCommand::Pin => {
                let Some(app) = target else { return };
                self.pin_app(&app.name, &app.executable, None);
            }
            MenuCommand::Unpin => {
                let Some(pin) = target.and_then(|app| app.pin) else {
                    return;
                };
                self.unpin_app(&pin);
            }
            MenuCommand::Exit => {
                // SAFETY: our own window; closing it restores Explorer's taskbar.
                unsafe { wm::PostMessageW(self.hwnd as HWND, wm::WM_CLOSE, 0, 0) };
            }
        }
    }

    /// "Terminate…": confirms, then ends every process of the app (`TerminateApplication`).
    fn terminate(&self, app: &MenuTarget) {
        let language = &self.settings.language;
        let (pids, inaccessible) = actions::processes_of(&app.executable);
        if pids.is_empty() {
            let key = if inaccessible == 0 {
                "ErrorAppNoProcesses"
            } else {
                "ErrorAppProcessesInaccessible"
            };
            log::report_error(key, language, None);
            return;
        }
        let prompt = Catalog::get("TerminatePrompt", language)
            .replace("{0}", &app.name)
            .replace("{1}", &pids.len().to_string());
        let title = Catalog::get("TerminateTitle", language);
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
        let (prompt, title) = (wide(&prompt), wide(&title));
        // SAFETY: a modal message box with null-terminated strings (MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2).
        let answer = unsafe {
            wm::MessageBoxW(
                std::ptr::null_mut(),
                prompt.as_ptr(),
                title.as_ptr(),
                wm::MB_YESNO | wm::MB_ICONWARNING | wm::MB_DEFBUTTON2 | wm::MB_SETFOREGROUND,
            )
        };
        if answer != wm::IDYES {
            return;
        }
        // The prompt was modal: a process may have ended and its id been reused meanwhile.
        let ended = actions::terminate(&pids, &app.executable);
        if ended == 0 {
            log::report_error("ErrorAppTerminateFailed", language, None);
        }
    }

    fn widget_preview(&mut self, id: &str, scale: f32) -> Option<Vec<u8>> {
        if self.widget_painter.is_none() {
            let inter = self
                .renderer
                .font_collection(&[
                    menu::INTER_REGULAR,
                    menu::INTER_MEDIUM,
                    menu::INTER_SEMIBOLD,
                    menu::INTER_BOLD,
                ])
                .ok();
            self.widget_painter = Some(crate::widgets::Painter::new(
                self.renderer.dwrite().clone(),
                inter,
            ));
        }
        let painter = self.widget_painter.as_ref()?;
        let dark = theme::tone(self.settings.theme) == theme::Tone::Dark;
        let height = crate::widgets::card_height(self.profile.cell_height) as f32;
        let radius_base = self.widget_radius();
        let fresh;
        let widget: &dyn crate::widgets::Widget = match self.widgets.get(id) {
            Some(live) => live,
            None => {
                let mut created = crate::widgets::create(id)?;
                let locale = crate::locale::Locale::new(self.settings.language.culture_name());
                let env = crate::widgets::Env {
                    now: crate::locale::now(),
                    locale: &locale,
                    settings: &self.settings,
                    language: &self.settings.language,
                };
                created.update(&env);
                fresh = created;
                fresh.as_ref()
            }
        };
        let width = widget.width(painter).ok()?;
        let radius = widget.radius(radius_base);
        match crate::widget_preview::render(widget, painter, width, height, radius, dark, scale) {
            Ok(png) => Some(png),
            Err(error) => {
                log::warn(format!("Widget preview for {id} failed: {error}"));
                None
            }
        }
    }

    /// `DockApp_MouseWheel` / `DockSurface_MouseWheel`: over an app the wheel cycles through its windows (down: next,
    /// up: previous, in handle order); anywhere else on the dock it changes the volume when `WheelVolumeEnabled`
    /// is on, 2 % per notch.
    fn wheel(&mut self, x: i32, y: i32, delta: i32) {
        if let Some(index) = self.hit_test(x, y)
            && matches!(self.items[index].kind, ItemKind::App { .. })
        {
            let item = &self.items[index];
            let mut windows = item.windows.clone();
            if windows.is_empty() {
                return;
            }
            windows.sort_unstable();
            let key = item_key(item);
            let current = windows
                .iter()
                .position(|&w| w == self.foreground)
                .or_else(|| {
                    let last = self.last_selected.get(&key)?;
                    windows.iter().position(|w| w == last)
                });
            let step: i64 = if delta < 0 { 1 } else { -1 };
            let n = windows.len() as i64;
            let base = current.map_or(if step > 0 { -1 } else { 0 }, |c| c as i64);
            let next = windows[(base + step + n).rem_euclid(n) as usize];
            self.preview_hide();
            if tracker::activate(next) {
                self.last_selected.insert(key, next);
            }
            return;
        }
        if !self.settings.wheel_volume_enabled {
            return;
        }
        self.wheel_remainder += delta;
        let notches = self.wheel_remainder / 120;
        if notches == 0 {
            return;
        }
        self.wheel_remainder -= notches * 120;
        if !crate::widgets::audio::nudge_volume(notches as f32 * 0.02) {
            log::report_error("ErrorVolumeChange", &self.settings.language, None);
        }
    }

    /// `FindNextWindowForExecutable`: the app's window that was in front at the press (or is in front now);
    /// otherwise the next one after the last chosen, in handle order, so repeated clicks cycle through them.
    fn next_window(&self, index: usize) -> Option<Hwnd> {
        let item = self.items.get(index)?;
        let windows = &item.windows;
        if windows.is_empty() {
            return None;
        }
        if let Some(pressed) = self.foreground_at_press.filter(|w| windows.contains(w)) {
            return Some(pressed);
        }
        if self.foreground != 0 && windows.contains(&self.foreground) {
            return Some(self.foreground);
        }
        let mut sorted = windows.clone();
        sorted.sort_unstable();
        let previous = self
            .last_selected
            .get(&item_key(item))
            .and_then(|last| sorted.iter().position(|w| w == last));
        Some(sorted[previous.map_or(0, |p| p + 1) % sorted.len()])
    }

    /// The app in front when the pointer went down. The click itself may already have cleared the foreground (it
    /// moves the foreground input queue to the dock's thread), so an empty value falls back to the last app known
    /// to be in front. `None`: a window of this process (a menu) is in front.
    fn press_foreground(&mut self) -> Option<Hwnd> {
        let before = self
            .foreground_before_click
            .take()
            .unwrap_or_else(tracker::foreground_application_window);
        match before {
            Some(0) if self.foreground != 0 => Some(self.foreground),
            other => other,
        }
    }

    /// A click that chose nothing (an empty spot, a drag) gives the foreground back to the app that had it: clicks
    /// move the foreground input queue to the dock's thread, which has no window to activate.
    fn restore_foreground(&self) {
        let Some(previous) = self.foreground_at_press.filter(|&w| w != 0) else {
            return;
        };
        if ot_shell::win::is_window(previous)
            && tracker::foreground_application_window().is_none_or(|w| w == 0)
        {
            // SAFETY: plain Win32 call; the click gave this process the right to set the foreground.
            unsafe { wm::SetForegroundWindow(previous as HWND) };
        }
    }

    fn click(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        if matches!(item.kind, ItemKind::Start | ItemKind::App { .. }) {
            // SAFETY: timer on our own window.
            unsafe { wm::KillTimer(self.hwnd as HWND, TIMER_PREVIEW_CLOSE) };
            preview::hide();
            self.preview_key = None;
        }
        let Some(item) = self.items.get(index) else {
            return;
        };
        match &item.kind {
            // The clock's calendar and the other widgets' flyouts arrive with their widgets' later steps.
            ItemKind::Widget(id) => match self.widgets.get(id).and_then(|w| w.click()) {
                Some(crate::widgets::Action::OpenUri(uri)) => actions::launch_executable(uri),
                Some(crate::widgets::Action::Flyout) => self.toggle_widget_flyout(index, id),
                None => {}
            },
            ItemKind::Start => {
                // A double click would open Start and close it again (`START_TOGGLE_DEBOUNCE`, 300 ms).
                let now = Instant::now();
                let recent = LAST_START
                    .with(|last| last.replace(Some(now)))
                    .is_some_and(|at| {
                        now.duration_since(at) < std::time::Duration::from_millis(300)
                    });
                if !recent {
                    actions::open_start();
                }
            }
            ItemKind::Search => {
                actions::open_search();
            }
            ItemKind::TaskView => {
                actions::open_task_view();
            }
            ItemKind::ShowDesktop => {
                actions::show_desktop();
            }
            ItemKind::Tray => {
                self.toggle_tray_flyout(index);
            }
            ItemKind::TrayPin(pin) => {
                let pin = pin.clone();
                self.open_tray_pin(&pin);
            }
            ItemKind::RecycleBin => {
                actions::open_recycle_bin();
            }
            ItemKind::App {
                pin,
                executable_path,
                ..
            } => match self.next_window(index) {
                Some(window) => {
                    let key = item_key(item);
                    let started = Instant::now();
                    let activated = tracker::toggle_activation(
                        window,
                        Some(executable_path),
                        self.foreground_at_press,
                    );
                    let elapsed = started.elapsed().as_millis();
                    if activated {
                        self.last_selected.insert(key, window);
                    } else {
                        log::report_error("ErrorWindowActivate", &self.settings.language, None);
                    }
                    if elapsed > 30 {
                        log::warn(format!(
                            "Activation of window 0x{window:X} took {elapsed} ms (ok={activated})."
                        ));
                    }
                }
                None => match pin {
                    Some(pin) => actions::launch_pin(pin),
                    None => actions::launch_executable(executable_path),
                },
            },
        }
    }
}

impl Visuals {
    fn new(renderer: &Renderer, colors: &DockColors) -> WinResult<Self> {
        let c = &renderer.compositor;
        let bar = c.CreateContainerVisual()?;
        renderer.root.Children()?.InsertAtTop(&bar)?;

        // Shadow: an invisible sprite whose DropShadow is masked by a rounded nine-grid, computed by DWM.
        let shadow = c.CreateSpriteVisual()?;
        if colors.shadow {
            let drop = c.CreateDropShadow()?;
            drop.SetBlurRadius(SHADOW_BLUR_DIPS)?;
            drop.SetOffset(Vector3 {
                X: 0.0,
                Y: SHADOW_DEPTH_DIPS,
                Z: 0.0,
            })?;
            drop.SetOpacity(SHADOW_OPACITY)?;
            drop.SetColor(ot_render::color(255, 0, 0, 0))?;
            drop.SetMask(&renderer.rounded_nine_grid(16.0)?)?;
            shadow.SetShadow(&drop)?;
        }
        bar.Children()?.InsertAtBottom(&shadow)?;

        // Bar: one rounded shape with the palette gradient fill and rim.
        let shape = c.CreateShapeVisual()?;
        let geometry = c.CreateRoundedRectangleGeometry()?;
        let fill = c.CreateSpriteShapeWithGeometry(&geometry)?;
        if let Some(stops) = &colors.fill {
            fill.SetFillBrush(&gradient(renderer, stops)?)?;
        }
        if let Some(stops) = &colors.rim {
            fill.SetStrokeBrush(&gradient(renderer, stops)?)?;
        }
        shape.Shapes()?.Append(&fill)?;
        bar.Children()?.InsertAtTop(&shape)?;

        Ok(Self {
            bar,
            shadow,
            shape,
            fill,
            geometry,
            separators: Vec::new(),
            items: Vec::new(),
            indicator_colors: (colors.indicator, colors.indicator_foreground),
            indicator_dots: None,
            separator_brush: c.CreateColorBrushWithColor(colors.separator)?,
            glyphs: HashMap::new(),
            badge_brush: None,
        })
    }
}

fn gradient(
    renderer: &Renderer,
    stops: &[(f32, Color)],
) -> WinResult<ot_render::Composition::CompositionLinearGradientBrush> {
    let c = &renderer.compositor;
    let brush = c.CreateLinearGradientBrush()?;
    brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
    brush.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
    let list = brush.ColorStops()?;
    for (offset, color) in stops {
        list.Append(&c.CreateColorGradientStopWithOffsetAndColor(*offset, *color)?)?;
    }
    Ok(brush)
}

/// A disabled status line in the jump list section (`TasksLoading`, `TasksEmpty`, …).
fn status_entry(label: String) -> menu::Entry {
    menu::Entry::Item {
        command: 0,
        label,
        icon: None,
        destructive: false,
        enabled: false,
    }
}

fn to_d2d(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(color.R) / 255.0,
        g: f32::from(color.G) / 255.0,
        b: f32::from(color.B) / 255.0,
        a: f32::from(color.A) / 255.0,
    }
}

/// The four-pane Windows logo in the Windows 11 blue.
/// `TrayIcons.Open`: the app's existing window if it has one, else a new start of the executable.
fn open_by_path(path: &str) {
    if let Some(window) = ot_shell::win::main_window_for(path)
        && ot_shell::tracker::activate(window)
    {
        return;
    }
    actions::launch_executable(path);
}

/// Items drawn with the `DockButton` template (apps, the Recycle Bin), whose content sits above the indicator row;
/// shell buttons (`ShellDockButtonTemplate`) center their glyph in the whole button.
fn has_indicator_row(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::App { .. } | ItemKind::RecycleBin)
}

/// `ActiveIndicator` size in device pixels.
fn indicator_px(dips: f64, scale: f64) -> f32 {
    (dips * scale).round().max(1.0) as f32
}

/// `ActiveIndicator` in `DockButton`: a 4 DIP dot (5 DIP for the app in front), centered along the slot, 1 DIP
/// above the bottom of the button on a bottom dock and 2 DIP from the screen edge on a side dock. Returns its size
/// and offset inside the item's container.
fn indicator_geometry(
    edge: DockEdge,
    scale: f64,
    slot: f32,
    cell_cross: f32,
    foreground: bool,
) -> (f32, f32, f32) {
    let size = indicator_px(if foreground { 5.0 } else { 4.0 }, scale);
    // WPF's layout rounding (`Math.Round`, halves to even): a 4 px dot in a 45 px button starts at 20, not 21.
    let centered = ((slot - size) / 2.0).round_ties_even();
    let (x, y) = match edge {
        DockEdge::Bottom => (centered, cell_cross - size - (scale as f32).round()),
        DockEdge::Left => ((2.0 * scale as f32).round(), centered),
        DockEdge::Right => (cell_cross - size - (2.0 * scale as f32).round(), centered),
    };
    // The sprite also holds the outline around the dot.
    let ring = indicator_ring_px(scale);
    (size + 2.0 * ring, x - ring, y - ring)
}

/// The indicator's black outline width: 1 DIP.
fn indicator_ring_px(scale: f64) -> f32 {
    scale.round().max(1.0) as f32
}

/// The Start logo (`ApplyStartIcon`): Windows' own `@WLOGO_96x96.png` (`ThemePacks.GetNativeWindowsLogo`), scaled
/// to the icon size with high-quality filtering; else the 22-unit logo path stretched over the icon box.
fn windows_logo(
    renderer: &Renderer,
    size: u32,
) -> WinResult<ot_render::Composition::CompositionSurfaceBrush> {
    if let Some(logo) = native_windows_logo(size) {
        return renderer.bitmap_brush(logo.width, logo.height, &logo.pixels, size);
    }
    renderer.draw(size, size, |context| {
        let unit = size as f32 / 22.0;
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let brush = context
                .CreateSolidColorBrush(&to_d2d(ot_render::color(255, 0x00, 0x78, 0xD4)), None)?;
            for (col, row) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let left = col * 12.0 * unit;
                let top = row * 12.0 * unit;
                context.FillRectangle(
                    &D2D_RECT_F {
                        left,
                        top,
                        right: left + 10.0 * unit,
                        bottom: top + 10.0 * unit,
                    },
                    &brush,
                );
            }
        }
        Ok(())
    })
}

/// `@WLOGO_96x96.png` from System32, else from the OOBE component in WinSxS, decoded at `size` pixels.
fn native_windows_logo(size: u32) -> Option<crate::icons::Icon> {
    let windows = std::env::var_os("WINDIR").map(std::path::PathBuf::from)?;
    let mut candidates = vec![windows.join("System32").join("@WLOGO_96x96.png")];
    let side_by_side = windows.join("WinSxS");
    if !candidates[0].exists()
        && let Ok(entries) = std::fs::read_dir(&side_by_side)
    {
        candidates.extend(entries.flatten().filter_map(|entry| {
            let name = entry.file_name();
            name.to_string_lossy()
                .starts_with("amd64_microsoft-windows-oobe-user_")
                .then(|| entry.path().join("@WLOGO_96x96.png"))
        }));
    }
    candidates
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .find_map(|bytes| crate::icons::decode_image(&bytes, Some((size, size))))
}

fn create_window() -> Option<Hwnd> {
    let class: Vec<u16> = "OpenTaskBarRust.Dock"
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
            // No WS_EX_NOACTIVATE: clicks must count as this process's input so it may bring the chosen app to the
            // front (Windows refused that for a minimized app while another app was in front). WM_MOUSEACTIVATE keeps
            // the dock from taking the activation, as in OpenTaskbar.
            wm::WS_EX_NOREDIRECTIONBITMAP | wm::WS_EX_TOOLWINDOW | wm::WS_EX_TOPMOST,
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
        (!hwnd.is_null()).then_some(hwnd as Hwnd)
    }
}

/// Windows' "Animation effects" (SPI_GETCLIENTAREAANIMATION), which OpenTaskbar honours for magnification.
fn client_area_animation() -> bool {
    let mut enabled: i32 = 1;
    // SAFETY: a BOOL out parameter.
    let ok = unsafe {
        wm::SystemParametersInfoW(
            wm::SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        )
    };
    ok == 0 || enabled != 0
}

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam as u16 as i16),
        i32::from((lparam >> 16) as u16 as i16),
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        wm::WM_MOUSEACTIVATE => {
            let before = tracker::foreground_application_window();
            with_dock(|dock| dock.foreground_before_click = Some(before));
            return wm::MA_NOACTIVATE as LRESULT;
        }
        wm::WM_TIMER if wparam == TIMER_HIDE => {
            with_dock(Dock::hide_timer_elapsed);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_GUARD => {
            with_dock(Dock::autohide_evaluate);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_HIDDEN => {
            with_dock(Dock::hidden_slide_done);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_END => {
            // SAFETY: our own window.
            unsafe { wm::DestroyWindow(hwnd) };
            return 0;
        }
        wm::WM_GETOBJECT => {
            let model = UIA.with(|slot| slot.borrow().clone());
            if let Some(result) = model.and_then(|m| uia::handle_get_object(&m, wparam, lparam)) {
                return result;
            }
        }
        WM_INVOKE => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                dock.foreground_at_press = tracker::foreground_application_window();
                dock.click(wparam);
                dock.foreground_at_press = None;
            });
            return 0;
        }
        WM_WTSSESSION_CHANGE => {
            match wparam as u32 {
                WTS_SESSION_LOCK => {
                    with_dock_or_defer(hwnd, message, wparam, lparam, Dock::session_locked);
                }
                WTS_SESSION_UNLOCK => {
                    with_dock_or_defer(hwnd, message, wparam, lparam, Dock::session_unlocked);
                }
                _ => {}
            }
            return 0;
        }
        // WM_POWERBROADCAST / PBT_APMRESUMEAUTOMATIC: the computer woke from sleep.
        0x0218 if wparam == 0x12 => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                dock.widgets.resumed();
                if !dock.autohide.hidden {
                    dock.update_widgets(false);
                }
            });
            return 1;
        }
        wm::WM_DESTROY => {
            // SAFETY: our own window, registered in `Dock::new`.
            unsafe {
                windows_sys::Win32::System::RemoteDesktop::WTSUnRegisterSessionNotification(hwnd)
            };
            uia::disconnect(hwnd as isize);
            UIA.with(|slot| slot.borrow_mut().take());
            if QUIT_ON_CLOSE.with(std::cell::Cell::get) {
                // SAFETY: ends this thread's message loop.
                unsafe { wm::PostQuitMessage(0) };
            }
            return 0;
        }
        wm::WM_SIZE => {
            with_dock(Dock::relayout);
            return 0;
        }
        WM_TRACKER => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                if wparam == 1
                    && let Some(foreground) =
                        tracker::foreground_application_window().filter(|&w| w != 0)
                {
                    // An empty foreground is a passing state (a click on the dock clears it): keep the last app,
                    // as OpenTaskbar keeps `_foregroundWindow`.
                    dock.foreground = foreground;
                }
                if dock.drag.is_some() {
                    dock.rebuild_pending = true;
                } else {
                    dock.rebuild_items();
                }
                if wparam == 0 {
                    dock.preview_windows_changed();
                }
                // Window events drive Smart auto-hide: a window moved over the dock or away from it.
                let foreground = ot_shell::win::foreground();
                dock.note_foreground(foreground);
                dock.refresh_overlap();
                dock.autohide_evaluate();
            });
            return 0;
        }
        WM_ICONS => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                if dock.icons.drain() {
                    dock.update_icons();
                    if dock.hover_icons_requested {
                        dock.load_hover_icons();
                    }
                    memory_mark("after icons");
                }
            });
            return 0;
        }
        wm::WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with_dock(|dock| {
                if !dock.drag_motion(x, y, wparam) {
                    dock.mouse_moved(x, y);
                }
            });
            return 0;
        }
        // WM_CAPTURECHANGED: another window took the pointer during a drag.
        0x0215 => {
            if lparam != hwnd as LPARAM {
                with_dock(Dock::cancel_drag);
            }
            return 0;
        }
        WM_MOUSELEAVE => {
            with_dock(Dock::mouse_left);
            return 0;
        }
        WM_FRAME => {
            with_dock_or_defer(hwnd, message, wparam, lparam, Dock::frame);
            return 0;
        }
        wm::WM_LBUTTONDOWN => {
            // A click on the dock does not take the activation from an open menu: close it here.
            menu::close();
            let (x, y) = point(lparam);
            with_dock(|dock| {
                dock.tip_press();
                dock.foreground_at_press = dock.press_foreground();
                dock.press(x, y);
                dock.update_shell_surfaces();
            });
            return 0;
        }
        wm::WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            with_dock(|dock| {
                dock.press = None;
                dock.update_shell_surfaces();
                if dock.drag.is_some() {
                    dock.drop_drag(x, y);
                    dock.restore_foreground();
                } else if let Some(index) = dock.hit_test(x, y) {
                    let is_app = matches!(dock.items[index].kind, ItemKind::App { .. });
                    if is_app || matches!(dock.items[index].kind, ItemKind::RecycleBin) {
                        dock.bounce(index);
                    }
                    // SAFETY: plain Win32 calls.
                    let (shift, control) = unsafe {
                        (
                            GetKeyState(i32::from(VK_SHIFT)) < 0,
                            GetKeyState(i32::from(VK_CONTROL)) < 0,
                        )
                    };
                    if is_app && shift {
                        dock.launch_new(index, control);
                    } else {
                        dock.click(index);
                    }
                } else {
                    dock.restore_foreground();
                }
                dock.foreground_at_press = None;
            });
            return 0;
        }
        wm::WM_MOUSEWHEEL => {
            // Screen coordinates: into the dock's client area for the hit test.
            let mut p = POINT {
                x: i32::from(lparam as u16 as i16),
                y: i32::from((lparam >> 16) as u16 as i16),
            };
            // SAFETY: our own window.
            unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p) };
            let delta = i32::from((wparam >> 16) as u16 as i16);
            with_dock(|dock| dock.wheel(p.x, p.y, delta));
            return 0;
        }
        wm::WM_MBUTTONUP => {
            let (x, y) = point(lparam);
            with_dock(|dock| {
                if let Some(index) = dock.hit_test(x, y)
                    && matches!(dock.items[index].kind, ItemKind::App { .. })
                {
                    // SAFETY: timer on our own window.
                    unsafe { wm::KillTimer(dock.hwnd as HWND, TIMER_PREVIEW_CLOSE) };
                    preview::hide();
                    dock.preview_key = None;
                    dock.bounce(index);
                    dock.launch_new(index, false);
                } else {
                    dock.foreground_at_press = dock.press_foreground();
                    dock.restore_foreground();
                    dock.foreground_at_press = None;
                }
            });
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_TIP_SHOW => {
            with_dock(Dock::tip_show);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_TIP_HIDE => {
            with_dock(Dock::tip_hide);
            return 0;
        }
        WM_HOTKEY => {
            with_dock(|dock| dock.hotkey(wparam as i32));
            return 0;
        }
        wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN | wm::WM_KEYUP | wm::WM_SYSKEYUP => {
            let down = message == wm::WM_KEYDOWN || message == wm::WM_SYSKEYDOWN;
            if with_dock(|dock| dock.keyboard_key(wparam as u16, down)).unwrap_or(false) {
                return 0;
            }
        }
        wm::WM_ACTIVATE if (wparam & 0xFFFF) as u32 == wm::WA_INACTIVE => {
            // Another app took the foreground (a click, an activated window): navigation ends where it is. The
            // dock's own menu taking the activation does not end it.
            let other = lparam;
            if other == 0 || ot_shell::win::process_id(other) != std::process::id() {
                with_dock(|dock| dock.keyboard_end(false));
            }
        }
        wm::WM_TIMER if wparam == TIMER_PREVIEW_OPEN => {
            with_dock(Dock::preview_open_elapsed);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_PREVIEW_CLOSE => {
            with_dock(Dock::preview_close_elapsed);
            return 0;
        }
        preview::WM_PREVIEW => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                dock.preview_event(wparam, lparam)
            });
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_REMOVE => {
            with_dock(Dock::removal_elapsed);
            return 0;
        }
        wm::WM_RBUTTONDOWN => {
            menu::close();
            return 0;
        }
        wm::WM_RBUTTONUP => {
            let (x, y) = point(lparam);
            with_dock(|dock| dock.open_menu(x, y));
            return 0;
        }
        menu::WM_MENU_COMMAND => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                dock.menu_command(wparam)
            });
            return 0;
        }
        menu::WM_MENU_CLOSED => {
            with_dock_or_defer(hwnd, message, wparam, lparam, Dock::menu_closed);
            return 0;
        }
        WM_JUMPLIST => {
            with_dock_or_defer(hwnd, message, wparam, lparam, Dock::jump_lists_ready);
            return 0;
        }
        crate::widgets::WM_WIDGETS_WAKE => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                if !dock.autohide.hidden {
                    dock.update_widgets(false);
                }
            });
            return 0;
        }
        WM_FULL_SCREEN => {
            if wparam != 0 {
                with_dock_or_defer(hwnd, message, wparam, lparam, Dock::full_screen_entered);
            }
            return 0;
        }
        WM_TRAY_CHANGED => {
            // The strip follows the icons; the open flyout keeps its list until it is opened again (as in
            // OpenTaskbar).
            with_dock_or_defer(hwnd, message, wparam, lparam, Dock::tray_changed);
            return 0;
        }
        WM_OPEN_SETTINGS => {
            open_settings();
            return 0;
        }
        crate::panel::WM_PANEL => {
            with_dock_or_defer(hwnd, message, wparam, lparam, Dock::autohide_evaluate);
            return 0;
        }
        tray_flyout::WM_FLYOUT => {
            with_dock_or_defer(hwnd, message, wparam, lparam, |dock| {
                dock.tray_flyout_action(wparam, lparam as usize)
            });
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_WIDGETS => {
            with_dock(Dock::widget_tick);
            return 0;
        }
        wm::WM_TIMER if wparam == TIMER_JUMPLIST => {
            with_dock(Dock::jump_list_timeout);
            return 0;
        }
        _ => {}
    }
    let handled = DOCK.with(|slot| match slot.try_borrow_mut() {
        Ok(mut dock) => dock
            .as_mut()
            .is_some_and(|d| d.session.handle_message(message, wparam, lparam)),
        Err(_) => session::defer_reentrant(hwnd as isize, message, wparam),
    });
    if handled {
        return 0;
    }
    // SAFETY: default processing.
    unsafe { wm::DefWindowProcW(hwnd, message, wparam, lparam) }
}
