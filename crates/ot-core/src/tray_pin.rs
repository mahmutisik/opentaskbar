//! Persisted identities of pinned tray icons (`TrayPinIdentity` / `TrayIconRef` in OpenTaskbar).
//! Keys are `guid:<lowercase D format>` or `path:<lowercase path>|<uid>`; structured JSON objects
//! (`{"guid": ..., "path": ..., "uid": ...}`) are accepted and converted to their key.

use serde_json::Value;

pub const MAX_PINNED_ICONS: usize = 64;
pub const MAX_KEY_LENGTH: usize = 512;

/// The canonical key for a structured pin object, or `None` when it identifies nothing.
pub fn key_from_object(json: &str) -> Option<String> {
    let Value::Object(map) = serde_json::from_str::<Value>(json).ok()? else {
        return None;
    };
    let get = |name: &str| {
        map.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    let guid = get("guid").and_then(Value::as_str).and_then(parse_guid);
    let path = get("path")
        .and_then(Value::as_str)
        .map(normalize_path)
        .unwrap_or_default();
    let mut uid = 0u32;
    match get("uid") {
        Some(Value::String(text)) if text.chars().all(|c| c.is_ascii_digit()) => {
            uid = text.parse().unwrap_or(0)
        }
        Some(Value::Number(number)) => {
            if let Some(value) = number.as_i64().and_then(|v| u32::try_from(v).ok()) {
                uid = value;
            }
        }
        _ => {}
    }
    match guid {
        Some(guid) => Some(format!("guid:{guid}")),
        None if !path.is_empty() => Some(format!("path:{path}|{uid}")),
        None => None,
    }
}

/// Trimmed, lowercase path used for identity comparisons.
pub fn normalize_path(path: &str) -> String {
    path.trim().to_lowercase()
}

/// Parses a GUID in any common .NET form ("N", "D", "B", "P") into lowercase "D" format; the empty GUID is `None`.
pub fn parse_guid(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .or_else(|| text.strip_prefix('(').and_then(|t| t.strip_suffix(')')))
        .unwrap_or(text);
    let hex: String = match inner.len() {
        32 => inner.to_owned(),
        36 => {
            let bytes = inner.as_bytes();
            if [8, 13, 18, 23].iter().any(|&i| bytes[i] != b'-') {
                return None;
            }
            inner.replace('-', "")
        }
        _ => return None,
    };
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    if hex.chars().all(|c| c == '0') {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}
