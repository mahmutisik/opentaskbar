//! Measures catalog parsing: every bundled language (what OpenTaskbar does at startup) versus only the
//! active language and English (what OpenTaskBar-Rust does). Run in release: `cargo run -p ot-core --release --example catalog_cost`.

use std::time::Instant;

use ot_core::AppLanguage;

fn main() {
    // Parse every catalog from the embedded sources the way the C# app does (all 14, eagerly).
    let started = Instant::now();
    let mut entries = 0usize;
    for language in AppLanguage::bundled() {
        let path = format!(
            "{}/lang/{}.json",
            env!("CARGO_MANIFEST_DIR"),
            language.culture_name()
        );
        let text = std::fs::read_to_string(path).expect("catalog file");
        let map: std::collections::HashMap<String, String> =
            serde_json::from_str(&text).expect("catalog json");
        entries += map.len();
    }
    let all = started.elapsed();

    // Lazy path: first lookup in Turkish parses tr-TR; a key Turkish lacks would also parse en-US.
    let started = Instant::now();
    let turkish = AppLanguage::new("tr-TR").unwrap();
    let _ = ot_core::Catalog::get("SettingsTitle", &turkish);
    let lazy = started.elapsed();

    println!(
        "all 14 catalogs: {:.2} ms ({entries} strings)",
        all.as_secs_f64() * 1000.0
    );
    println!("active catalog only: {:.2} ms", lazy.as_secs_f64() * 1000.0);
}
