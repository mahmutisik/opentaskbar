//! The persisted settings model. Field names, order, defaults and enum spellings match OpenTaskbar's
//! `Models/DockSettings.cs` so a `settings.json` written by either application is readable by the other.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::language::AppLanguage;

string_enum! { pub enum DockPosition { Left = 0, Center = 1, Right = 2 } default = Center }
string_enum! {
    /// How dock contents are aligned inside a full-width dock bar.
    pub enum DockContentAlignment { Left = 0, Center = 1, Right = 2 } default = Center
}
string_enum! { pub enum AppTheme { Dark = 0, Light = 1, System = 2 } default = System }
string_enum! { pub enum AutoHideMode { Disabled = 0, Smart = 1, Always = 2 } default = Disabled }
string_enum! { pub enum DockLayout { Floating = 0, FullWidth = 1 } default = Floating }
string_enum! { pub enum DockSize { Compact = 0, Medium = 1, Large = 2 } default = Compact }
string_enum! { pub enum DockEdge { Bottom = 0, Left = 1, Right = 2 } default = Bottom }
string_enum! {
    pub enum HotkeyChoice { Disabled = 0, AltSpace = 1, CtrlSpace = 2, CtrlAltSpace = 3, CtrlAltD = 4, WinAltD = 5 }
    default = Disabled
}
string_enum! { pub enum StartButtonAction { WindowsStart = 0, Launcher = 1 } default = WindowsStart }
string_enum! { pub enum StartButtonPlacement { Start = 0, End = 1, Hidden = 2, InWidgets = 3 } default = Start }
string_enum! {
    pub enum ClockDateStyle { DayAndShortDate = 0, ShortDateOnly = 1, NumericDate = 2, Hidden = 3 }
    default = DayAndShortDate
}
string_enum! { pub enum SecondaryAppScope { CurrentMonitorOnly = 0, AllMonitors = 1 } default = CurrentMonitorOnly }
string_enum! { pub enum WidgetDensityMode { Auto = 0, Compact = 1, Full = 2 } default = Auto }
string_enum! {
    /// How the dock's width is determined: fit content, fixed pixels, share of the monitor, or edge to edge.
    pub enum DockWidthMode { Auto = 0, Pixels = 1, Percent = 2, Fill = 3 } default = Auto
}
string_enum! {
    /// How the horizontal dock's thickness is determined.
    pub enum DockHeightMode { Auto = 0, Pixels = 1, Percent = 2, Fill = 3 } default = Auto
}
string_enum! { pub enum DockGapUnit { Pixels = 0, Percent = 1 } default = Pixels }
string_enum! {
    /// How a translucent surface is filled. None removes the fill, its rim and its shadow.
    pub enum SurfaceBackground { Glass = 0, Solid = 1, None = 2 } default = Glass
}
string_enum! {
    /// Which quota window the AI usage ring shows.
    pub enum AiUsageRing { Highest = 0, Session = 1, Weekly = 2 } default = Highest
}

flags_enum! {
    /// Meters of the System meters widget. 8 was CpuTemperature and stays unused.
    pub struct SystemMeters { None = 0, Cpu = 1, Memory = 2, Battery = 4, Gpu = 16, GpuTemperature = 32, All = 51 }
}
flags_enum! {
    pub struct AiProviders {
        None = 0, Claude = 1, Codex = 2, Antigravity = 4, OpenCode = 8, Copilot = 16, DeepSeek = 32,
        OpenRouter = 64, Cursor = 128, All = 255,
    }
}
flags_enum! {
    /// Antigravity model groups shown on the dock widget card.
    pub struct AntigravityGroups { None = 0, Gemini = 1, Claude = 2, Argon = 4, All = 7 }
}

/// A named dock configuration snapshot (widgets, layout, appearance, hotkeys and hidden pinned apps).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockProfile {
    pub name: String,
    pub hidden_apps: Option<Vec<String>>,
    pub widgets: Option<Vec<String>>,
    pub installed_widgets: Option<Vec<String>>,
    pub widget_order: Option<Vec<String>>,
    pub dock_item_order: Option<Vec<String>>,
    pub dock_width_mode: Option<DockWidthMode>,
    pub dock_width_pixels: Option<i32>,
    pub dock_width_percent: Option<i32>,
    pub theme: Option<AppTheme>,
    pub theme_pack: Option<String>,
    pub theme_application_icons: Option<bool>,
    pub launcher_hotkey: Option<HotkeyChoice>,
    pub dock_hotkey: Option<HotkeyChoice>,
    pub auto_hide: Option<AutoHideMode>,
    pub custom_launcher_hotkey: Option<String>,
    pub custom_dock_hotkey: Option<String>,
}

/// A user-defined group of applications shown as a single dock icon with a flyout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockAppGroup {
    pub id: String,
    pub name: String,
    pub executable_paths: Option<Vec<String>>,
}

/// Every persisted setting. Missing properties take these defaults, so older files stay valid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DockSettings {
    pub position: DockPosition,
    pub pin_order: Option<Vec<String>>,
    pub theme: AppTheme,
    pub language: AppLanguage,
    pub animations_enabled: bool,
    pub previews_enabled: bool,
    pub notification_badges_enabled: bool,
    pub auto_hide: AutoHideMode,
    /// Legacy: migrated into the four per-surface styles on load, no longer edited by the UI.
    pub surface_opacity: f64,
    pub dock_background: SurfaceBackground,
    pub dock_opacity: f64,
    pub dock_border_thickness: f64,
    pub dock_border_color: Option<String>,
    pub menu_background: SurfaceBackground,
    pub menu_opacity: f64,
    pub menu_border_thickness: f64,
    pub menu_border_color: Option<String>,
    pub widget_background: SurfaceBackground,
    pub widget_opacity: f64,
    pub widget_border_thickness: f64,
    pub widget_border_color: Option<String>,
    pub settings_background: SurfaceBackground,
    pub settings_opacity: f64,
    pub settings_border_thickness: f64,
    pub settings_border_color: Option<String>,
    pub widgets: Option<Vec<String>>,
    pub installed_widgets: Option<Vec<String>>,
    pub layout: DockLayout,
    pub content_alignment: DockContentAlignment,
    pub size: DockSize,
    pub magnification_enabled: bool,
    pub show_clock: bool,
    pub show_media: bool,
    pub show_system_stats: bool,
    pub show_focus: bool,
    pub show_audio: bool,
    pub show_quick_settings: bool,
    pub focus_minutes: i32,
    pub focus_break_minutes: i32,
    pub focus_sessions: i32,
    pub show_recycle_bin: bool,
    pub launcher_hotkey: HotkeyChoice,
    pub dock_hotkey: HotkeyChoice,
    pub start_action: StartButtonAction,
    pub start_placement: StartButtonPlacement,
    pub start_widget_index: i32,
    pub wheel_volume_enabled: bool,
    pub check_updates: bool,
    pub show_weather: bool,
    pub weather_city: String,
    pub current_desktop_only: bool,
    pub show_on_all_monitors: bool,
    pub target_display: Option<String>,
    pub secondary_apps: SecondaryAppScope,
    pub secondary_widgets: bool,
    pub edge: DockEdge,
    /// `None` means "Downloads only".
    pub stack_folders: Option<Vec<String>>,
    #[serde(deserialize_with = "skip_null_entries")]
    pub profiles: Option<Vec<DockProfile>>,
    pub active_profile: Option<String>,
    pub dock_scale: f64,
    pub widget_order: Option<Vec<String>>,
    pub clock24_hour: bool,
    pub clock_show_seconds: bool,
    pub date_style: ClockDateStyle,
    pub stats_meters: SystemMeters,
    /// Folder name under the themes directory; `None` keeps the built-in look.
    pub theme_pack: Option<String>,
    pub theme_application_icons: bool,
    pub global_widget_density: WidgetDensityMode,
    pub widget_density_overrides: Option<IndexMap<String, WidgetDensityMode>>,
    /// Free-form dock order: "start", "search", "taskview", "tray", "peek", "app:<path>", "stack:<path>",
    /// "widget:<id>", "bin". `None` keeps the legacy grouped layout.
    pub dock_item_order: Option<Vec<String>>,
    /// Corner radius override; `None` follows the size profile.
    pub dock_corner_radius: Option<i32>,
    /// Legacy: only read to migrate the retired single AI usage widget.
    pub ai_usage_providers: AiProviders,
    pub ai_usage_ring: AiUsageRing,
    pub antigravity_groups: AntigravityGroups,
    #[serde(deserialize_with = "skip_null_entries")]
    pub app_groups: Option<Vec<DockAppGroup>>,
    pub show_search_button: bool,
    pub show_task_view_button: bool,
    pub show_tray_button: bool,
    pub show_desktop_button: bool,
    pub dock_width_mode: DockWidthMode,
    pub dock_width_pixels: i32,
    pub dock_width_percent: i32,
    pub dock_top_gap_unit: DockGapUnit,
    pub dock_top_gap_pixels: i32,
    pub dock_top_gap_percent: i32,
    pub dock_bottom_gap_unit: DockGapUnit,
    pub dock_bottom_gap_pixels: i32,
    pub dock_bottom_gap_percent: i32,
    pub dock_icon_padding: i32,
    pub focus_mode_enabled: bool,
    pub custom_launcher_hotkey: Option<String>,
    pub custom_dock_hotkey: Option<String>,
    /// Stable tray icon keys shown beside the tray chevron while running. Entries may be legacy strings or
    /// structured objects on disk; loading normalizes them to strings.
    #[serde(deserialize_with = "pinned_tray_entries")]
    pub pinned_tray_icons: Option<Vec<String>>,
    pub dock_height_mode: DockHeightMode,
    pub dock_height_pixels: i32,
    pub dock_height_percent: i32,
    /// Rust-only: the setup wizard version this user completed (0 = never). Ignored by the C# application.
    pub onboarding_version: u32,
    /// Properties this version does not know about, kept so saving never drops another version's data.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DockSettings {
    fn default() -> Self {
        Self {
            position: DockPosition::Center,
            pin_order: None,
            theme: AppTheme::System,
            language: AppLanguage::english(),
            animations_enabled: true,
            previews_enabled: true,
            notification_badges_enabled: true,
            auto_hide: AutoHideMode::Disabled,
            surface_opacity: 0.9,
            dock_background: SurfaceBackground::Glass,
            dock_opacity: 0.9,
            dock_border_thickness: 1.0,
            dock_border_color: None,
            menu_background: SurfaceBackground::Glass,
            menu_opacity: 0.9,
            menu_border_thickness: 1.0,
            menu_border_color: None,
            widget_background: SurfaceBackground::Glass,
            widget_opacity: 0.9,
            widget_border_thickness: 1.0,
            widget_border_color: None,
            settings_background: SurfaceBackground::Glass,
            settings_opacity: 0.9,
            settings_border_thickness: 1.0,
            settings_border_color: None,
            widgets: None,
            installed_widgets: None,
            layout: DockLayout::Floating,
            content_alignment: DockContentAlignment::Center,
            size: DockSize::Compact,
            magnification_enabled: true,
            show_clock: false,
            show_media: false,
            show_system_stats: false,
            show_focus: false,
            show_audio: false,
            show_quick_settings: false,
            focus_minutes: 25,
            focus_break_minutes: 5,
            focus_sessions: 4,
            show_recycle_bin: true,
            launcher_hotkey: HotkeyChoice::AltSpace,
            dock_hotkey: HotkeyChoice::CtrlAltD,
            start_action: StartButtonAction::WindowsStart,
            start_placement: StartButtonPlacement::Start,
            start_widget_index: 0,
            wheel_volume_enabled: false,
            check_updates: true,
            show_weather: false,
            weather_city: String::new(),
            current_desktop_only: false,
            show_on_all_monitors: true,
            target_display: None,
            secondary_apps: SecondaryAppScope::CurrentMonitorOnly,
            secondary_widgets: true,
            edge: DockEdge::Bottom,
            stack_folders: None,
            profiles: None,
            active_profile: None,
            dock_scale: 1.0,
            widget_order: None,
            clock24_hour: true,
            clock_show_seconds: false,
            date_style: ClockDateStyle::DayAndShortDate,
            stats_meters: SystemMeters::All,
            theme_pack: None,
            theme_application_icons: true,
            global_widget_density: WidgetDensityMode::Auto,
            widget_density_overrides: None,
            dock_item_order: None,
            dock_corner_radius: None,
            ai_usage_providers: AiProviders::None,
            ai_usage_ring: AiUsageRing::Highest,
            antigravity_groups: AntigravityGroups::All,
            app_groups: None,
            show_search_button: true,
            show_task_view_button: true,
            show_tray_button: true,
            show_desktop_button: true,
            dock_width_mode: DockWidthMode::Auto,
            dock_width_pixels: 900,
            dock_width_percent: 60,
            dock_top_gap_unit: DockGapUnit::Pixels,
            dock_top_gap_pixels: 0,
            dock_top_gap_percent: 0,
            dock_bottom_gap_unit: DockGapUnit::Pixels,
            dock_bottom_gap_pixels: 6,
            dock_bottom_gap_percent: 0,
            dock_icon_padding: 0,
            focus_mode_enabled: false,
            custom_launcher_hotkey: None,
            custom_dock_hotkey: None,
            pinned_tray_icons: None,
            dock_height_mode: DockHeightMode::Auto,
            dock_height_pixels: 48,
            dock_height_percent: 7,
            onboarding_version: 0,
            extra: Map::new(),
        }
    }
}

impl DockSettings {
    /// Animations, suppressed while focus mode is active.
    pub fn effective_animations_enabled(&self) -> bool {
        self.animations_enabled && !self.focus_mode_enabled
    }

    /// Notification badges, suppressed while focus mode is active.
    pub fn effective_notification_badges_enabled(&self) -> bool {
        self.notification_badges_enabled && !self.focus_mode_enabled
    }
}

/// Reads a list whose `null` entries are dropped instead of failing the whole file
/// (OpenTaskbar crashed on `"Profiles":[null]`; upstream PR #142 skips them too).
fn skip_null_entries<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let list = Option::<Vec<Option<T>>>::deserialize(deserializer)?;
    Ok(list.map(|items| items.into_iter().flatten().collect()))
}

/// Pinned tray icons are normalized by `migrate::pinned_tray_icons`; here every entry is accepted as raw JSON
/// and anything that is not a usable key is dropped later.
fn pinned_tray_entries<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    let raw = Option::<Vec<Value>>::deserialize(deserializer)?;
    Ok(raw.map(|items| {
        items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text),
                Value::Object(_) => Some(item.to_string()),
                _ => None,
            })
            .collect()
    }))
}
