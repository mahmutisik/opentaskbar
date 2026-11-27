//! The Settings window and the first-run setup wizard. The wizard: a borderless window with a WebView2 page (`ui/wizard`), talking to Rust through JSON
//! messages. After the install step every choice is applied to the running dock immediately.
#![cfg(windows)]

pub mod backup;
pub mod install;
pub mod settings;
mod theme_editor;
pub mod wizard;

pub use wizard::{WizardOptions, WizardOutcome, run};
