//! Dock profiles (`DockProfileService` in OpenTaskbar): the parts needed by load and save.

use crate::settings::{DockProfile, DockSettings};

pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// A profile snapshot of the profile-scoped settings.
pub fn capture(name: &str, settings: &DockSettings, hidden_apps: Vec<String>) -> DockProfile {
    let widgets = settings.widgets.clone().unwrap_or_default();
    DockProfile {
        name: name.to_owned(),
        hidden_apps: Some(hidden_apps),
        installed_widgets: Some(merge_installed(
            settings.installed_widgets.as_deref(),
            &widgets,
        )),
        widgets: Some(widgets),
        widget_order: settings.widget_order.clone(),
        dock_item_order: settings.dock_item_order.clone(),
        dock_width_mode: Some(settings.dock_width_mode),
        dock_width_pixels: Some(settings.dock_width_pixels),
        dock_width_percent: Some(settings.dock_width_percent),
        theme: Some(settings.theme),
        theme_pack: settings.theme_pack.clone(),
        theme_application_icons: Some(settings.theme_application_icons),
        launcher_hotkey: Some(settings.launcher_hotkey),
        dock_hotkey: Some(settings.dock_hotkey),
        auto_hide: Some(settings.auto_hide),
        custom_launcher_hotkey: settings.custom_launcher_hotkey.clone(),
        custom_dock_hotkey: settings.custom_dock_hotkey.clone(),
    }
}

/// Seeds a "Default" profile when none exist.
pub fn ensure_default_profile(mut settings: DockSettings) -> DockSettings {
    if settings.profiles.as_ref().is_some_and(|p| !p.is_empty()) {
        return settings;
    }
    settings.profiles = Some(vec![capture(DEFAULT_PROFILE_NAME, &settings, Vec::new())]);
    settings.active_profile = Some(DEFAULT_PROFILE_NAME.to_owned());
    settings
}

/// Copies the live values into the active profile slot (done before every save).
pub fn sync_active(mut settings: DockSettings) -> DockSettings {
    let Some(active) = settings
        .active_profile
        .clone()
        .filter(|name| !name.trim().is_empty())
    else {
        return settings;
    };
    let Some(index) = settings
        .profiles
        .as_ref()
        .and_then(|profiles| find(profiles, &active))
    else {
        return settings;
    };
    let current = &settings.profiles.as_ref().expect("checked above")[index];
    let snapshot = capture(
        &current.name,
        &settings,
        current.hidden_apps.clone().unwrap_or_default(),
    );
    settings.profiles.as_mut().expect("checked above")[index] = snapshot;
    settings
}

pub fn find(profiles: &[DockProfile], name: &str) -> Option<usize> {
    let name = name.to_lowercase();
    profiles
        .iter()
        .position(|profile| profile.name.to_lowercase() == name)
}

fn merge_installed(installed: Option<&[String]>, widgets: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for id in installed.unwrap_or_default().iter().chain(widgets) {
        if !merged.contains(id) {
            merged.push(id.clone());
        }
    }
    merged
}
