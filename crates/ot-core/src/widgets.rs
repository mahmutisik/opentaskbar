//! Widget identifiers known to the settings model (`DockWidgetService` in OpenTaskbar).

use crate::settings::AiProviders;

/// One widget per AI provider, in catalog order.
pub const AI_USAGE_WIDGETS: &[(&str, AiProviders)] = &[
    ("AiUsageClaude", AiProviders::Claude),
    ("AiUsageCodex", AiProviders::Codex),
    ("AiUsageAntigravity", AiProviders::Antigravity),
    ("AiUsageOpenCode", AiProviders::OpenCode),
    ("AiUsageCopilot", AiProviders::Copilot),
    ("AiUsageDeepSeek", AiProviders::DeepSeek),
    ("AiUsageOpenRouter", AiProviders::OpenRouter),
    ("AiUsageCursor", AiProviders::Cursor),
];

/// Every ID settings may reference, including the hidden legacy "Date" alias. Compared case-sensitively.
pub const SUPPORTED_IDS: &[&str] = &[
    "Clock",
    "Weather",
    "DayProgress",
    "Stopwatch",
    "Countdown",
    "SystemStats",
    "NetSpeed",
    "Battery",
    "BluetoothBattery",
    "Storage",
    "AiUsageClaude",
    "AiUsageCodex",
    "AiUsageAntigravity",
    "AiUsageOpenCode",
    "AiUsageCopilot",
    "AiUsageDeepSeek",
    "AiUsageOpenRouter",
    "AiUsageCursor",
    "Media",
    "Audio",
    "QuickSettings",
    "Focus",
    "Date",
];

pub fn is_supported(id: &str) -> bool {
    SUPPORTED_IDS.contains(&id)
}

/// Retired widgets dropped on load. Search became a dock button, Desktops the Task View button, Tray the tray button.
pub const REMOVED_IDS: &[&str] = &["Memory", "Search", "Desktops", "Tray"];
pub const LEGACY_AI_USAGE_ID: &str = "AiUsage";
