//! Compatibility with OpenTaskbar's `settings.json` (C# `DockSettingsStore`).

use std::path::PathBuf;

use ot_core::hotkey::HotkeyBinding;
use ot_core::settings::*;
use ot_core::{AppLanguage, Catalog, SettingsError, SettingsStore};
use serde_json::{Value, json};

/// Property order of `Models/DockSettings.cs` at cef7de1 (96 properties).
const CSHARP_PROPERTIES: &[&str] = &[
    "Position",
    "PinOrder",
    "Theme",
    "Language",
    "AnimationsEnabled",
    "PreviewsEnabled",
    "NotificationBadgesEnabled",
    "AutoHide",
    "SurfaceOpacity",
    "DockBackground",
    "DockOpacity",
    "DockBorderThickness",
    "DockBorderColor",
    "MenuBackground",
    "MenuOpacity",
    "MenuBorderThickness",
    "MenuBorderColor",
    "WidgetBackground",
    "WidgetOpacity",
    "WidgetBorderThickness",
    "WidgetBorderColor",
    "SettingsBackground",
    "SettingsOpacity",
    "SettingsBorderThickness",
    "SettingsBorderColor",
    "Widgets",
    "InstalledWidgets",
    "Layout",
    "ContentAlignment",
    "Size",
    "MagnificationEnabled",
    "ShowClock",
    "ShowMedia",
    "ShowSystemStats",
    "ShowFocus",
    "ShowAudio",
    "ShowQuickSettings",
    "FocusMinutes",
    "FocusBreakMinutes",
    "FocusSessions",
    "ShowRecycleBin",
    "LauncherHotkey",
    "DockHotkey",
    "StartAction",
    "StartPlacement",
    "StartWidgetIndex",
    "WheelVolumeEnabled",
    "CheckUpdates",
    "ShowWeather",
    "WeatherCity",
    "CurrentDesktopOnly",
    "ShowOnAllMonitors",
    "TargetDisplay",
    "SecondaryApps",
    "SecondaryWidgets",
    "Edge",
    "StackFolders",
    "Profiles",
    "ActiveProfile",
    "DockScale",
    "WidgetOrder",
    "Clock24Hour",
    "ClockShowSeconds",
    "DateStyle",
    "StatsMeters",
    "ThemePack",
    "ThemeApplicationIcons",
    "GlobalWidgetDensity",
    "WidgetDensityOverrides",
    "DockItemOrder",
    "DockCornerRadius",
    "AiUsageProviders",
    "AiUsageRing",
    "AntigravityGroups",
    "AppGroups",
    "ShowSearchButton",
    "ShowTaskViewButton",
    "ShowTrayButton",
    "ShowDesktopButton",
    "DockWidthMode",
    "DockWidthPixels",
    "DockWidthPercent",
    "DockTopGapUnit",
    "DockTopGapPixels",
    "DockTopGapPercent",
    "DockBottomGapUnit",
    "DockBottomGapPixels",
    "DockBottomGapPercent",
    "DockIconPadding",
    "FocusModeEnabled",
    "CustomLauncherHotkey",
    "CustomDockHotkey",
    "PinnedTrayIcons",
    "DockHeightMode",
    "DockHeightPixels",
    "DockHeightPercent",
];

fn store() -> SettingsStore {
    SettingsStore::new(
        PathBuf::from("unused.json"),
        AppLanguage::new("de-DE").unwrap(),
    )
}

fn parse(value: Value) -> Result<DockSettings, SettingsError> {
    store().parse(&value.to_string())
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ot-core-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("settings.json")
}

#[test]
fn serialized_properties_match_csharp_order() {
    let value = serde_json::to_value(DockSettings::default()).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(CSHARP_PROPERTIES.len(), 96);
    assert_eq!(&keys[..96], CSHARP_PROPERTIES);
    assert_eq!(&keys[96..], ["OnboardingVersion"]);
}

#[test]
fn defaults_round_trip_and_validate() {
    let defaults = store().defaults();
    assert!(
        ot_core::validate::is_valid(&defaults),
        "{:?}",
        ot_core::validate::first_violation(&defaults)
    );
    let text = serde_json::to_string_pretty(&defaults).unwrap();
    // Loading normalizes the legacy "no InstalledWidgets" form to an empty list, as C# does.
    let expected = DockSettings {
        installed_widgets: Some(vec![]),
        ..defaults.clone()
    };
    assert_eq!(store().parse(&text).unwrap(), expected);
    let resaved = serde_json::to_string_pretty(&expected).unwrap();
    assert_eq!(
        store().parse(&resaved).unwrap(),
        expected,
        "second round trip is stable"
    );
    assert_eq!(defaults.language.culture_name(), "de-DE");
    assert_eq!(defaults.active_profile.as_deref(), Some("Default"));
}

#[test]
fn empty_object_takes_defaults_and_system_language() {
    let settings = parse(json!({})).unwrap();
    assert_eq!(settings.language.culture_name(), "de-DE");
    assert_eq!(settings.dock_bottom_gap_pixels, 6);
    assert_eq!(settings.installed_widgets, Some(vec![]));
    assert_eq!(settings.profiles.as_ref().map(Vec::len), Some(1));
}

#[test]
fn enums_read_names_any_case_and_numbers() {
    let s =
        parse(json!({ "Theme": "dark", "Position": 2, "AutoHide": "1", "Layout": "FullWidth" }))
            .unwrap();
    assert_eq!(s.theme, AppTheme::Dark);
    assert_eq!(s.position, DockPosition::Right);
    assert_eq!(s.auto_hide, AutoHideMode::Smart);
    assert_eq!(s.layout, DockLayout::FullWidth);
    assert_eq!(
        parse(json!({ "Theme": "Purple" })),
        Err(SettingsError::Read)
    );
    assert_eq!(parse(json!({ "Theme": 7 })), Err(SettingsError::Read));
}

#[test]
fn flags_use_dotnet_format() {
    assert_eq!(SystemMeters::All.format(), "All");
    assert_eq!((SystemMeters::Cpu | SystemMeters::Gpu).format(), "Cpu, Gpu");
    assert_eq!(SystemMeters::None.format(), "None");
    assert_eq!(
        (AntigravityGroups::Gemini | AntigravityGroups::Claude).format(),
        "Gemini, Claude"
    );
    assert_eq!(AiProviders(256).format(), "256");
    assert_eq!(SystemMeters::parse("cpu, memory"), Some(SystemMeters(3)));
    assert_eq!(SystemMeters::parse("19"), Some(SystemMeters(19)));
    for names in [
        SystemMeters::NAMES,
        AiProviders::NAMES,
        AntigravityGroups::NAMES,
    ] {
        assert!(
            names.windows(2).all(|pair| pair[0].1 < pair[1].1),
            "flags must be declared ascending"
        );
    }
}

#[test]
fn legacy_stats_meters_are_cleaned() {
    let s = parse(json!({ "StatsMeters": "Cpu, CpuTemperature, Battery, Memory" })).unwrap();
    assert_eq!(s.stats_meters, SystemMeters::Cpu | SystemMeters::Memory);
    let s = parse(json!({ "StatsMeters": "CpuTemperature" })).unwrap();
    assert_eq!(s.stats_meters, SystemMeters::None);
    let s = parse(json!({ "StatsMeters": 63 })).unwrap();
    assert_eq!(s.stats_meters, SystemMeters::All);
}

#[test]
fn legacy_language_values() {
    assert_eq!(
        parse(json!({ "Language": 0 }))
            .unwrap()
            .language
            .culture_name(),
        "tr-TR"
    );
    assert_eq!(
        parse(json!({ "Language": "Japanese" }))
            .unwrap()
            .language
            .culture_name(),
        "ja-JP"
    );
    assert_eq!(
        parse(json!({ "Language": "tr-tr" }))
            .unwrap()
            .language
            .culture_name(),
        "tr-TR"
    );
    assert_eq!(
        parse(json!({ "Language": "de-AT" }))
            .unwrap()
            .language
            .culture_name(),
        "de-AT"
    );
    assert_eq!(
        parse(json!({ "Language": "sv-SE" })),
        Err(SettingsError::Read)
    );
}

#[test]
fn first_launch_language_follows_system_culture() {
    assert_eq!(AppLanguage::from_culture("tr-TR").culture_name(), "tr-TR");
    assert_eq!(AppLanguage::from_culture("de-AT").culture_name(), "de-AT");
    assert_eq!(AppLanguage::from_culture("pt").culture_name(), "pt-BR");
    assert_eq!(AppLanguage::from_culture("sv-SE").culture_name(), "en-US");
}

#[test]
fn legacy_gap_padding_and_surface_opacity() {
    let s = parse(json!({ "DockBottomGap": 12, "DockIconPaddingX": 40, "SurfaceOpacity": 0.7 }))
        .unwrap();
    assert_eq!(s.dock_bottom_gap_pixels, 12);
    assert_eq!(s.dock_icon_padding, 12);
    assert_eq!(
        (
            s.dock_opacity,
            s.menu_opacity,
            s.widget_opacity,
            s.settings_opacity
        ),
        (0.7, 0.7, 0.7, 0.7)
    );
    assert!(!s.extra.contains_key("DockBottomGap") && !s.extra.contains_key("DockIconPaddingX"));

    // Explicit new values win over legacy ones.
    let s = parse(json!({ "DockBottomGap": 12, "DockBottomGapPixels": 3, "DockBackground": "Solid", "SurfaceOpacity": 0.7 }))
        .unwrap();
    assert_eq!(s.dock_bottom_gap_pixels, 3);
    assert_eq!(s.dock_opacity, 0.9);
}

#[test]
fn removed_widgets_move_to_buttons() {
    let s = parse(json!({ "Widgets": ["Search", "Clock"], "InstalledWidgets": ["Search", "Clock", "Memory"] })).unwrap();
    assert_eq!(s.widgets, Some(vec!["Clock".to_owned()]));
    assert_eq!(s.installed_widgets, Some(vec!["Clock".to_owned()]));
    assert!(s.show_search_button);

    let s = parse(json!({ "InstalledWidgets": ["Search", "Desktops"], "DockItemOrder": ["start", "widget:Tray"] })).unwrap();
    assert!(!s.show_search_button && !s.show_task_view_button && !s.show_tray_button);
    assert_eq!(s.dock_item_order, Some(vec!["start".to_owned()]));
}

#[test]
fn legacy_ai_usage_widget_is_split_per_provider() {
    let s = parse(json!({
        "Widgets": ["AiUsage"],
        "InstalledWidgets": ["AiUsage", "Clock"],
        "AiUsageProviders": "Claude, Codex",
        "WidgetDensityOverrides": { "AiUsage": "Compact" }
    }))
    .unwrap();
    assert_eq!(
        s.widgets,
        Some(vec!["AiUsageClaude".to_owned(), "AiUsageCodex".to_owned()])
    );
    assert_eq!(s.installed_widgets.as_ref().unwrap().len(), 9);
    assert_eq!(s.ai_usage_providers, AiProviders::None);
    let densities = s.widget_density_overrides.unwrap();
    assert_eq!(densities.len(), 8);
    assert!(densities.values().all(|d| *d == WidgetDensityMode::Compact));
}

#[test]
fn pinned_tray_icons_are_normalized() {
    let s = parse(json!({ "PinnedTrayIcons": [
        "guid:7820ae78-23e3-4229-82c1-e41cb67d5b9c",
        "GUID:7820AE78-23E3-4229-82C1-E41CB67D5B9C",
        { "path": "C:\\Apps\\Tool.EXE", "uid": 3 },
        { "guid": "{2E1E3F4A-0000-0000-0000-00000000000A}" },
        "  ",
        42
    ]}))
    .unwrap();
    assert_eq!(
        s.pinned_tray_icons,
        Some(vec![
            "guid:7820ae78-23e3-4229-82c1-e41cb67d5b9c".to_owned(),
            "path:c:\\apps\\tool.exe|3".to_owned(),
            "guid:2e1e3f4a-0000-0000-0000-00000000000a".to_owned(),
        ])
    );
}

#[test]
fn null_profile_entries_are_skipped() {
    let s = parse(json!({ "Profiles": [null, { "Name": "Work", "HiddenApps": [] }], "ActiveProfile": "Work" })).unwrap();
    assert_eq!(s.profiles.unwrap().len(), 1);
}

#[test]
fn validation_rules_reject_bad_values() {
    for bad in [
        json!({ "DockBorderColor": "#GG0000" }),
        json!({ "Widgets": ["Clock"], "InstalledWidgets": [] }),
        json!({ "Widgets": ["Unknown"], "InstalledWidgets": ["Unknown"] }),
        json!({ "PinOrder": ["a", "A"] }),
        json!({ "AntigravityGroups": "None" }),
        json!({ "Profiles": [{ "Name": "A", "HiddenApps": [] }, { "Name": "a", "HiddenApps": [] }] }),
        json!({ "Profiles": [{ "Name": "A" }] }),
        json!({ "AppGroups": [{ "Id": "1", "Name": "G", "ExecutablePaths": ["x.exe"] },
                              { "Id": "2", "Name": "H", "ExecutablePaths": ["X.EXE"] }] }),
        json!({ "CustomLauncherHotkey": "Ctrl+Alt+D" }),
        json!({ "CustomDockHotkey": "Win+A" }),
    ] {
        assert_eq!(parse(bad.clone()), Err(SettingsError::Invalid), "{bad}");
    }
    assert!(parse(json!({ "DockBorderColor": "#80FF0000", "DockCornerRadius": 0 })).is_ok());
}

#[test]
fn out_of_range_numbers_are_clamped_on_load() {
    // One bad number no longer replaces the whole file with defaults: it is brought into range, the rest is kept.
    let s = parse(json!({
        "DockWidthPixels": 50, "DockOpacity": 1.5, "DockCornerRadius": 60, "DockScale": 3.0,
        "FocusSessions": 0, "Theme": "Light"
    }))
    .unwrap();
    assert_eq!(s.dock_width_pixels, 100);
    assert_eq!(s.dock_opacity, 1.0);
    assert_eq!(s.dock_corner_radius, Some(48));
    assert_eq!(s.dock_scale, 1.5);
    assert_eq!(s.focus_sessions, 1);
    assert_eq!(serde_json::to_value(&s).unwrap()["Theme"], json!("Light"));
}

#[test]
fn hotkeys_parse_like_csharp() {
    let parse = |text| HotkeyBinding::parse(text).map(|b| (b.modifiers, b.virtual_key));
    assert_eq!(parse("Ctrl+Alt+D"), Some((3, 0x44)));
    assert_eq!(parse("ctrl + shift + f5"), Some((6, 0x74)));
    assert_eq!(parse("Alt+Space"), Some((1, 0x20)));
    assert_eq!(parse("Ctrl++"), Some((2, 0xBB)));
    assert_eq!(parse("Ctrl+NumPad3"), Some((2, 0x63)));
    assert_eq!(parse("Ctrl+Enter"), Some((2, 0x0D)));
    for bad in [
        "Shift+A",
        "Ctrl+Ctrl+A",
        "Ctrl+A+B",
        "Win+Alt+D",
        "Ctrl+Escape",
        "Ctrl+65",
        "Ctrl+",
        "A",
        "Ctrl+LeftShift",
    ] {
        assert_eq!(parse(bad), None, "{bad}");
    }
    assert!(HotkeyBinding::is_valid_custom(Some("Disabled")));
    assert_eq!(HotkeyBinding::resolve(HotkeyChoice::WinAltD, None), None);
}

#[test]
fn unknown_properties_survive_a_save() {
    let path = temp_path("unknown");
    let store = SettingsStore::new(path.clone(), AppLanguage::english());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        json!({ "FutureSetting": { "a": 1 }, "WheelVolume": true }).to_string(),
    )
    .unwrap();
    let loaded = store.load();
    assert_eq!(loaded.error, None);
    store.save(&loaded.settings).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["FutureSetting"], json!({ "a": 1 }));
    assert!(saved.get("WheelVolume").is_none());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn first_run_until_the_file_is_written() {
    let path = temp_path("first-run");
    let store = SettingsStore::new(path.clone(), AppLanguage::new("tr-TR").unwrap());
    assert!(store.is_first_run());
    let outcome = store.load();
    assert!(outcome.first_run && outcome.error.is_none());
    assert_eq!(outcome.settings.language.culture_name(), "tr-TR");

    let mut settings = outcome.settings;
    settings.onboarding_version = 1;
    settings.theme = AppTheme::Dark;
    let saved = store.save(&settings).unwrap();
    assert_eq!(
        saved.profiles.as_ref().unwrap()[0].theme,
        Some(AppTheme::Dark),
        "save syncs the active profile"
    );

    let reloaded = store.load();
    assert!(!reloaded.first_run && reloaded.error.is_none());
    assert_eq!(reloaded.settings.onboarding_version, 1);
    assert_eq!(reloaded.settings.theme, AppTheme::Dark);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn unusable_file_is_backed_up_and_defaults_used() {
    let path = temp_path("broken");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let outcome = SettingsStore::new(path.clone(), AppLanguage::english()).load();
    assert_eq!(outcome.error, Some(SettingsError::Read));
    assert!(!outcome.first_run);
    let backup = outcome.backup.expect("backup written");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), "{ not json");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn invalid_save_is_refused() {
    let path = temp_path("refuse");
    let store = SettingsStore::new(path.clone(), AppLanguage::english());
    let settings = DockSettings {
        dock_scale: 3.0,
        ..store.defaults()
    };
    assert_eq!(store.save(&settings), Err(SettingsError::ValuesInvalid));
    assert!(!path.exists());
}

#[test]
fn catalogs_parse_and_fall_back() {
    for language in AppLanguage::bundled() {
        assert_ne!(
            Catalog::get("SettingsTitle", &language),
            "SettingsTitle",
            "{language} catalog missing or broken"
        );
    }
    assert_eq!(
        Catalog::get("Settings", &AppLanguage::new("tr-TR").unwrap()),
        "Ayarlar"
    );
    assert_eq!(
        Catalog::get("Settings", &AppLanguage::new("de-AT").unwrap()),
        Catalog::get("Settings", &AppLanguage::new("de-DE").unwrap())
    );
    assert_eq!(
        Catalog::get("NoSuchKey", &AppLanguage::english()),
        "NoSuchKey"
    );
}

/// Reads a real OpenTaskbar settings file when `OT_COMPAT_SETTINGS` points to one; skipped otherwise.
#[test]
fn real_opentaskbar_file_loads_when_provided() {
    let Some(path) = std::env::var_os("OT_COMPAT_SETTINGS") else {
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let settings = store().parse(&text).unwrap_or_else(|e| panic!("{e:?}"));
    let again = store()
        .parse(&serde_json::to_string(&settings).unwrap())
        .unwrap();
    assert_eq!(again, settings);
}
