//! Upgrades older `settings.json` files, step for step as `DockSettingsStore.Load` does in OpenTaskbar.

use serde_json::{Map, Value};

use crate::settings::{AiProviders, DockSettings, SystemMeters};
use crate::tray_pin::{self, MAX_KEY_LENGTH, MAX_PINNED_ICONS};
use crate::widgets::{AI_USAGE_WIDGETS, LEGACY_AI_USAGE_ID, REMOVED_IDS};

/// Legacy properties consumed by migrations. They are removed after loading so a later load cannot
/// re-apply them over newer values.
pub const CONSUMED_LEGACY_KEYS: &[&str] = &[
    "DockBottomGap",
    "DockIconPaddingX",
    "DockIconPaddingY",
    "WheelVolume",
];

const MAX_ICON_PADDING: i32 = 12;

/// Rewrites retired v0.2.x tokens in the raw JSON before deserialization (`NormalizeLegacyJson`).
pub fn normalize_legacy_json(root: &mut Map<String, Value>) {
    match root.get("StatsMeters") {
        Some(Value::String(text)) => {
            let kept: Vec<&str> = text
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .filter(|t| {
                    !t.eq_ignore_ascii_case("CpuTemperature") && !t.eq_ignore_ascii_case("Battery")
                })
                .collect();
            let replacement = if kept.is_empty() {
                "None".to_owned()
            } else {
                kept.join(", ")
            };
            root.insert("StatsMeters".into(), Value::String(replacement));
        }
        Some(Value::Number(number)) => {
            if let Some(bits) = number.as_i64() {
                let masked = bits & i64::from(SystemMeters::All.bits());
                root.insert("StatsMeters".into(), Value::from(masked));
            }
        }
        _ => {}
    }

    if !root.contains_key("DockBottomGapPixels")
        && let Some(gap) = root.get("DockBottomGap").and_then(Value::as_i64)
        && (0..=200).contains(&gap)
    {
        root.insert("DockBottomGapPixels".into(), Value::from(gap));
    }
}

/// Applies every post-deserialization migration in OpenTaskbar's order. `root` is the normalized JSON.
pub fn apply(
    mut settings: DockSettings,
    root: &Map<String, Value>,
    default_language: &crate::AppLanguage,
) -> DockSettings {
    // Older files without a language follow the same system-language rule as a first launch.
    if !root.contains_key("Language") {
        settings.language = default_language.clone();
    }
    // Normalize the legacy enabled-only format before enforcing the installed-subset invariant.
    if settings.installed_widgets.is_none() {
        settings.installed_widgets = Some(settings.widgets.clone().unwrap_or_default());
    }
    settings = removed_widgets(settings);
    settings = legacy_ai_usage(settings);
    settings = surface_styles(settings, root);
    settings = icon_padding(settings, root);
    settings = pinned_tray_icons(settings);
    settings.stats_meters = settings.stats_meters & SystemMeters::All;
    for key in CONSUMED_LEGACY_KEYS {
        settings.extra.shift_remove(*key);
    }
    crate::profiles::ensure_default_profile(settings)
}

fn contains(ids: Option<&Vec<String>>, value: &str) -> bool {
    ids.is_some_and(|ids| ids.iter().any(|id| id.eq_ignore_ascii_case(value)))
}

fn without(ids: Option<Vec<String>>, removed: &[String]) -> Option<Vec<String>> {
    ids.map(|ids| {
        ids.into_iter()
            .filter(|id| !removed.iter().any(|r| id.eq_ignore_ascii_case(r)))
            .collect()
    })
}

/// Drops retired widget IDs everywhere; Search, Desktops and Tray carry their enabled state to the buttons.
pub fn removed_widgets(mut s: DockSettings) -> DockSettings {
    let removed: Vec<String> = REMOVED_IDS.iter().map(|id| (*id).to_owned()).collect();
    let removed_tokens: Vec<String> = REMOVED_IDS
        .iter()
        .map(|id| format!("widget:{id}"))
        .collect();
    let mention = |s: &DockSettings, id: &str| {
        let enabled = contains(s.widgets.as_ref(), id);
        let mentioned = enabled
            || contains(s.installed_widgets.as_ref(), id)
            || contains(s.widget_order.as_ref(), id)
            || contains(s.dock_item_order.as_ref(), &format!("widget:{id}"));
        (enabled, mentioned)
    };
    let (search_on, search_seen) = mention(&s, "Search");
    let (desktops_on, desktops_seen) = mention(&s, "Desktops");
    let (tray_on, tray_seen) = mention(&s, "Tray");

    if let Some(densities) = s.widget_density_overrides.as_mut() {
        densities.retain(|key, _| !REMOVED_IDS.iter().any(|r| key.eq_ignore_ascii_case(r)));
    }
    if let Some(profiles) = s.profiles.as_mut() {
        for profile in profiles {
            profile.widgets = without(profile.widgets.take(), &removed);
            profile.installed_widgets = without(profile.installed_widgets.take(), &removed);
            profile.widget_order = without(profile.widget_order.take(), &removed);
            profile.dock_item_order = without(profile.dock_item_order.take(), &removed_tokens);
        }
    }
    s.widgets = without(s.widgets.take(), &removed);
    s.installed_widgets = without(s.installed_widgets.take(), &removed);
    s.widget_order = without(s.widget_order.take(), &removed);
    s.dock_item_order = without(s.dock_item_order.take(), &removed_tokens);
    if search_seen {
        s.show_search_button = search_on;
    }
    if desktops_seen {
        s.show_task_view_button = desktops_on;
    }
    if tray_seen {
        s.show_tray_button = tray_on;
    }
    s
}

fn replace(ids: Option<Vec<String>>, legacy: &str, replacement: &[String]) -> Option<Vec<String>> {
    let ids = ids?;
    if !ids.iter().any(|id| id.eq_ignore_ascii_case(legacy)) {
        return Some(ids);
    }
    let mut result: Vec<String> = Vec::new();
    for id in ids {
        let items: Vec<String> = if id.eq_ignore_ascii_case(legacy) {
            replacement.to_vec()
        } else {
            vec![id]
        };
        for item in items {
            if !result.iter().any(|r| r.eq_ignore_ascii_case(&item)) {
                result.push(item);
            }
        }
    }
    Some(result)
}

/// Splits the retired single AI usage widget into one widget per provider.
pub fn legacy_ai_usage(mut s: DockSettings) -> DockSettings {
    let all: Vec<String> = AI_USAGE_WIDGETS
        .iter()
        .map(|(id, _)| (*id).to_owned())
        .collect();
    let enabled: Vec<String> = AI_USAGE_WIDGETS
        .iter()
        .filter(|(_, provider)| s.ai_usage_providers.contains(*provider))
        .map(|(id, _)| (*id).to_owned())
        .collect();
    let all_tokens: Vec<String> = all.iter().map(|id| format!("widget:{id}")).collect();
    let legacy_token = format!("widget:{LEGACY_AI_USAGE_ID}");

    if let Some(densities) = s.widget_density_overrides.as_mut()
        && let Some(density) = densities
            .iter()
            .find(|(key, _)| key.as_str() == LEGACY_AI_USAGE_ID)
            .map(|(_, d)| *d)
    {
        densities.retain(|key, _| !key.eq_ignore_ascii_case(LEGACY_AI_USAGE_ID));
        for id in &all {
            densities.insert(id.clone(), density);
        }
    }
    if let Some(profiles) = s.profiles.as_mut() {
        for p in profiles {
            p.widgets = replace(p.widgets.take(), LEGACY_AI_USAGE_ID, &enabled);
            p.installed_widgets = replace(p.installed_widgets.take(), LEGACY_AI_USAGE_ID, &all);
            p.widget_order = replace(p.widget_order.take(), LEGACY_AI_USAGE_ID, &all);
            p.dock_item_order = replace(p.dock_item_order.take(), &legacy_token, &all_tokens);
        }
    }
    s.widgets = replace(s.widgets.take(), LEGACY_AI_USAGE_ID, &enabled);
    s.installed_widgets = replace(s.installed_widgets.take(), LEGACY_AI_USAGE_ID, &all);
    s.widget_order = replace(s.widget_order.take(), LEGACY_AI_USAGE_ID, &all);
    s.dock_item_order = replace(s.dock_item_order.take(), &legacy_token, &all_tokens);
    s.ai_usage_providers = AiProviders::None;
    s
}

/// The retired single SurfaceOpacity slider fed every surface; copy it into the four styles once.
fn surface_styles(mut s: DockSettings, root: &Map<String, Value>) -> DockSettings {
    if root.contains_key("DockBackground") {
        return s;
    }
    if let Some(value) = root.get("SurfaceOpacity").and_then(Value::as_f64)
        && value.is_finite()
        && (0.6..=1.0).contains(&value)
    {
        s.dock_opacity = value;
        s.menu_opacity = value;
        s.widget_opacity = value;
        s.settings_opacity = value;
    }
    s
}

/// DockIconPaddingX/Y existed briefly; collapse them back to the single horizontal DockIconPadding.
fn icon_padding(mut s: DockSettings, root: &Map<String, Value>) -> DockSettings {
    if let Some(value) = root.get("DockIconPaddingX").and_then(Value::as_i64)
        && let Ok(value) = i32::try_from(value)
    {
        s.dock_icon_padding = value.clamp(0, MAX_ICON_PADDING);
    }
    s
}

/// Keeps pinned tray icons a plain, de-duplicated string list; structured objects become their key and
/// invalid entries are dropped instead of invalidating the whole file.
pub fn pinned_tray_icons(mut s: DockSettings) -> DockSettings {
    let Some(entries) = s.pinned_tray_icons.take() else {
        return s;
    };
    let mut migrated: Vec<String> = Vec::new();
    for entry in entries {
        let key = if entry.trim_start().starts_with('{') {
            tray_pin::key_from_object(&entry)
        } else {
            let key = entry.trim();
            (!key.is_empty() && key.chars().count() <= MAX_KEY_LENGTH).then(|| key.to_owned())
        };
        let Some(key) = key else { continue };
        if migrated.len() >= MAX_PINNED_ICONS
            || migrated.iter().any(|k| k.eq_ignore_ascii_case(&key))
        {
            continue;
        }
        migrated.push(key);
    }
    s.pinned_tray_icons = (!migrated.is_empty()).then_some(migrated);
    s
}
