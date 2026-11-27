//! Loads a settings file through the Rust store and saves it to another path.
//! Used to check that OpenTaskbar (C#) accepts files written by OpenTaskBar-Rust:
//! `cargo run -p ot-core --example resave -- <input.json|-> <output.json>` ("-" writes defaults).

use std::path::PathBuf;

use ot_core::{AppLanguage, SettingsStore};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: resave <input.json|-> <output.json>");
        std::process::exit(2);
    };
    let language = AppLanguage::from_culture(&ot_core::language::system_ui_culture());
    let settings = if input == "-" {
        SettingsStore::new(PathBuf::from(&output), language.clone()).defaults()
    } else {
        let outcome = SettingsStore::new(PathBuf::from(&input), language.clone()).load();
        if let Some(error) = outcome.error {
            eprintln!("load failed: {}", error.key());
            std::process::exit(1);
        }
        outcome.settings
    };
    match SettingsStore::new(PathBuf::from(&output), language).save(&settings) {
        Ok(_) => println!("wrote {output}"),
        Err(error) => {
            eprintln!("save failed: {}", error.key());
            std::process::exit(1);
        }
    }
}
