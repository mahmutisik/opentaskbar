//! The dock itself: pinned and running apps, shell buttons, layout, composition visuals and the dock window.
#![cfg(windows)]

pub mod actions;
pub mod app_name;
pub mod autohide;
pub mod dock;
pub mod fisheye;
pub mod frame_clock;
pub mod hotkey;
pub mod icon_loader;
pub mod icons;
pub mod jumplist;
pub mod layout;
pub mod locale;
pub mod magnify;
pub mod menu;
pub mod panel;
pub mod pins;
pub mod preview;
pub mod reorder;
pub mod theme;
pub mod token_defaults;
pub mod tokens;
pub mod tooltip;
pub mod tray_flyout;
pub mod trayapps;
pub mod traypin;
pub mod uia;
pub mod vector_icon;
pub mod widget_preview;
pub mod widgets;

pub use dock::{
    apply_and_save, apply_settings, current_settings, default_corner_radius, export_pins,
    import_pins, is_running, open_settings_on_start, reload_theme, restart_into_setup, run,
    set_onboarding_version, set_persist, set_settings_opener, setup_restart_requested, start, stop,
    widget_preview_png,
};
