//! Settings validation, rule for rule as `DockSettingsStore.IsValid` in OpenTaskbar. A file that fails is
//! replaced by defaults on load and refused on save.

use crate::hotkey::{HotkeyBinding, conflicts};
use crate::settings::{
    AiProviders, AntigravityGroups, DockAppGroup, DockProfile, DockSettings, SystemMeters,
};
use crate::tray_pin::{MAX_KEY_LENGTH, MAX_PINNED_ICONS};
use crate::widgets;

/// Returns the name of the first rule that fails, or `None` when the settings are valid.
pub fn first_violation(s: &DockSettings) -> Option<&'static str> {
    let rules: [(&'static str, bool); 34] = [
        ("Language", s.language.is_supported()),
        (
            "DockWidthPixels",
            (100..=5000).contains(&s.dock_width_pixels),
        ),
        (
            "DockWidthPercent",
            (10..=100).contains(&s.dock_width_percent),
        ),
        (
            "DockHeightPixels",
            (24..=400).contains(&s.dock_height_pixels),
        ),
        (
            "DockHeightPercent",
            (2..=25).contains(&s.dock_height_percent),
        ),
        (
            "DockGaps",
            (0..=200).contains(&s.dock_top_gap_pixels)
                && (0..=100).contains(&s.dock_top_gap_percent)
                && (0..=200).contains(&s.dock_bottom_gap_pixels)
                && (0..=100).contains(&s.dock_bottom_gap_percent),
        ),
        ("FocusMinutes", (1..=90).contains(&s.focus_minutes)),
        (
            "FocusBreakMinutes",
            (1..=30).contains(&s.focus_break_minutes),
        ),
        ("FocusSessions", (1..=8).contains(&s.focus_sessions)),
        ("SurfaceOpacity", in_range(s.surface_opacity, 0.6, 1.0)),
        (
            "SurfaceOpacities",
            in_range(s.dock_opacity, 0.0, 1.0)
                && in_range(s.menu_opacity, 0.0, 1.0)
                && in_range(s.widget_opacity, 0.0, 1.0)
                && in_range(s.settings_opacity, 0.0, 1.0),
        ),
        (
            "SurfaceBorders",
            border_valid(s.dock_border_thickness, s.dock_border_color.as_deref())
                && border_valid(s.menu_border_thickness, s.menu_border_color.as_deref())
                && border_valid(s.widget_border_thickness, s.widget_border_color.as_deref())
                && border_valid(
                    s.settings_border_thickness,
                    s.settings_border_color.as_deref(),
                ),
        ),
        ("DockScale", in_range(s.dock_scale, 0.75, 1.5)),
        (
            "PinOrder",
            s.pin_order.as_ref().is_none_or(|order| {
                !order.iter().any(|p| p.trim().is_empty()) && distinct_ignore_case(order.iter())
            }),
        ),
        ("Widgets", widget_list_valid(s.widgets.as_deref())),
        (
            "InstalledWidgets",
            widget_list_valid(s.installed_widgets.as_deref()),
        ),
        (
            "WidgetsInstalled",
            s.widgets.as_ref().is_none_or(|enabled| {
                enabled.iter().all(|id| {
                    s.installed_widgets
                        .as_ref()
                        .is_some_and(|installed| installed.contains(id))
                })
            }),
        ),
        (
            "CustomLauncherHotkey",
            HotkeyBinding::is_valid_custom(s.custom_launcher_hotkey.as_deref()),
        ),
        (
            "CustomDockHotkey",
            HotkeyBinding::is_valid_custom(s.custom_dock_hotkey.as_deref()),
        ),
        (
            "StatsMeters",
            s.stats_meters.bits() & !SystemMeters::All.bits() == 0,
        ),
        (
            "AiUsageProviders",
            s.ai_usage_providers.bits() & !AiProviders::All.bits() == 0,
        ),
        (
            "AntigravityGroups",
            s.antigravity_groups != AntigravityGroups::None
                && s.antigravity_groups.bits() & !AntigravityGroups::All.bits() == 0,
        ),
        ("WeatherCity", s.weather_city.encode_utf16().count() <= 80),
        (
            "StackFolders",
            s.stack_folders
                .as_ref()
                .is_none_or(|folders| !folders.iter().any(|f| f.trim().is_empty())),
        ),
        (
            "WidgetOrder",
            s.widget_order.as_ref().is_none_or(|order| {
                !order.iter().any(|id| id.trim().is_empty()) && distinct_ignore_case(order.iter())
            }),
        ),
        (
            "Profiles",
            s.profiles
                .as_ref()
                .is_none_or(|profiles| profiles.iter().all(profile_valid)),
        ),
        (
            "ProfileNames",
            s.profiles
                .as_ref()
                .is_none_or(|profiles| distinct_ignore_case(profiles.iter().map(|p| &p.name))),
        ),
        (
            "AppGroups",
            s.app_groups
                .as_ref()
                .is_none_or(|groups| groups.iter().all(group_valid)),
        ),
        (
            "AppGroupIds",
            s.app_groups
                .as_ref()
                .is_none_or(|groups| distinct_ignore_case(groups.iter().map(|g| &g.id))),
        ),
        (
            "AppGroupNames",
            s.app_groups
                .as_ref()
                .is_none_or(|groups| distinct_ignore_case(groups.iter().map(|g| &g.name))),
        ),
        (
            "AppGroupMembers",
            s.app_groups.as_ref().is_none_or(|groups| {
                distinct_ignore_case(
                    groups
                        .iter()
                        .flat_map(|g| g.executable_paths.iter().flatten()),
                )
            }),
        ),
        (
            "DockCornerRadius",
            s.dock_corner_radius.is_none_or(|r| (0..=48).contains(&r)),
        ),
        (
            "PinnedTrayIcons",
            s.pinned_tray_icons.as_ref().is_none_or(|pins| {
                pins.len() <= MAX_PINNED_ICONS
                    && !pins
                        .iter()
                        .any(|p| p.trim().is_empty() || p.encode_utf16().count() > MAX_KEY_LENGTH)
            }),
        ),
        (
            "HotkeyConflict",
            !conflicts(
                HotkeyBinding::resolve(s.launcher_hotkey, s.custom_launcher_hotkey.as_deref()),
                HotkeyBinding::resolve(s.dock_hotkey, s.custom_dock_hotkey.as_deref()),
            ),
        ),
    ];
    rules.into_iter().find(|(_, ok)| !ok).map(|(name, _)| name)
}

pub fn is_valid(settings: &DockSettings) -> bool {
    first_violation(settings).is_none()
}

/// Brings out-of-range numbers back into their rule's range (a value that is not a number takes the default) and
/// returns the names of the settings it changed. Used on load only, so one bad number no longer replaces the whole
/// file with defaults (OpenTaskbar rejects the file; the upstream Rust port clamps the same way). Saving stays strict.
pub fn clamp_numbers(s: &mut DockSettings) -> Vec<&'static str> {
    let defaults = DockSettings::default();
    let mut changed = Vec::new();
    let mut int = |name: &'static str, value: &mut i32, min: i32, max: i32| {
        let clamped = (*value).clamp(min, max);
        if clamped != *value {
            *value = clamped;
            changed.push(name);
        }
    };
    int("DockWidthPixels", &mut s.dock_width_pixels, 100, 5000);
    int("DockWidthPercent", &mut s.dock_width_percent, 10, 100);
    int("DockHeightPixels", &mut s.dock_height_pixels, 24, 400);
    int("DockHeightPercent", &mut s.dock_height_percent, 2, 25);
    int("DockTopGapPixels", &mut s.dock_top_gap_pixels, 0, 200);
    int("DockTopGapPercent", &mut s.dock_top_gap_percent, 0, 100);
    int("DockBottomGapPixels", &mut s.dock_bottom_gap_pixels, 0, 200);
    int(
        "DockBottomGapPercent",
        &mut s.dock_bottom_gap_percent,
        0,
        100,
    );
    int("FocusMinutes", &mut s.focus_minutes, 1, 90);
    int("FocusBreakMinutes", &mut s.focus_break_minutes, 1, 30);
    int("FocusSessions", &mut s.focus_sessions, 1, 8);
    if let Some(radius) = s.dock_corner_radius.as_mut() {
        int("DockCornerRadius", radius, 0, 48);
    }
    let mut float = |name: &'static str, value: &mut f64, default: f64, min: f64, max: f64| {
        let clamped = if value.is_finite() {
            value.clamp(min, max)
        } else {
            default
        };
        if clamped.to_bits() != value.to_bits() {
            *value = clamped;
            changed.push(name);
        }
    };
    float(
        "SurfaceOpacity",
        &mut s.surface_opacity,
        defaults.surface_opacity,
        0.6,
        1.0,
    );
    float(
        "DockOpacity",
        &mut s.dock_opacity,
        defaults.dock_opacity,
        0.0,
        1.0,
    );
    float(
        "MenuOpacity",
        &mut s.menu_opacity,
        defaults.menu_opacity,
        0.0,
        1.0,
    );
    float(
        "WidgetOpacity",
        &mut s.widget_opacity,
        defaults.widget_opacity,
        0.0,
        1.0,
    );
    float(
        "SettingsOpacity",
        &mut s.settings_opacity,
        defaults.settings_opacity,
        0.0,
        1.0,
    );
    float(
        "DockBorderThickness",
        &mut s.dock_border_thickness,
        defaults.dock_border_thickness,
        0.0,
        12.0,
    );
    float(
        "MenuBorderThickness",
        &mut s.menu_border_thickness,
        defaults.menu_border_thickness,
        0.0,
        12.0,
    );
    float(
        "WidgetBorderThickness",
        &mut s.widget_border_thickness,
        defaults.widget_border_thickness,
        0.0,
        12.0,
    );
    float(
        "SettingsBorderThickness",
        &mut s.settings_border_thickness,
        defaults.settings_border_thickness,
        0.0,
        12.0,
    );
    float(
        "DockScale",
        &mut s.dock_scale,
        defaults.dock_scale,
        0.75,
        1.5,
    );
    changed
}

fn in_range(value: f64, min: f64, max: f64) -> bool {
    value.is_finite() && value >= min && value <= max
}

fn border_valid(thickness: f64, color: Option<&str>) -> bool {
    in_range(thickness, 0.0, 12.0)
        && color.is_none_or(|c| c.trim().is_empty() || parse_color(c).is_some())
}

/// "#AARRGGBB" or "#RRGGBB" (leading '#' optional) as ARGB.
pub fn parse_color(text: &str) -> Option<[u8; 4]> {
    let value = text.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(value.get(i..i + 2)?, 16).ok();
    match value.len() {
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        6 => Some([255, byte(0)?, byte(2)?, byte(4)?]),
        _ => None,
    }
}

fn widget_list_valid(ids: Option<&[String]>) -> bool {
    ids.is_none_or(|ids| {
        ids.iter().all(|id| widgets::is_supported(id))
            && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id))
    })
}

fn distinct_ignore_case<'a>(items: impl Iterator<Item = &'a String>) -> bool {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        let key = item.to_lowercase();
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
    }
    true
}

fn profile_valid(p: &DockProfile) -> bool {
    !p.name.trim().is_empty()
        && p.hidden_apps.is_some()
        && widget_list_valid(p.widgets.as_deref())
        && widget_list_valid(p.installed_widgets.as_deref())
        && match (&p.widgets, &p.installed_widgets) {
            (Some(enabled), Some(installed)) => enabled.iter().all(|id| installed.contains(id)),
            _ => true,
        }
        && p.widget_order.as_ref().is_none_or(|order| {
            !order.iter().any(|id| id.trim().is_empty()) && distinct_ignore_case(order.iter())
        })
        && p.dock_width_pixels
            .is_none_or(|v| (100..=5000).contains(&v))
        && p.dock_width_percent.is_none_or(|v| (10..=100).contains(&v))
        && HotkeyBinding::is_valid_custom(p.custom_launcher_hotkey.as_deref())
        && HotkeyBinding::is_valid_custom(p.custom_dock_hotkey.as_deref())
}

fn group_valid(g: &DockAppGroup) -> bool {
    !g.id.trim().is_empty() && !g.name.trim().is_empty() && g.executable_paths.is_some()
}
