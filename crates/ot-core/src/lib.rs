//! Core of OpenTaskBar-Rust: the persisted settings model (compatible with OpenTaskbar's `settings.json`),
//! its validation and legacy migrations, first-run detection, profiles and localization.

#[macro_use]
mod enums;

pub mod hotkey;
mod hotkey_keys;
pub mod language;
pub mod log;
pub mod migrate;
pub mod profiles;
pub mod settings;
pub mod startup;
pub mod store;
pub mod theme_pack;
pub mod tray_pin;
pub mod validate;
pub mod widgets;

pub use language::{AppLanguage, Catalog};
pub use settings::*;
pub use store::{LoadOutcome, SettingsError, SettingsStore};
