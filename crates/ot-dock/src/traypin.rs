//! Identities of tray icons pinned to the dock (OpenTaskbar's `TrayPinIdentity`): `guid:<guid>` when the icon has a
//! GUID, otherwise `path:<lowercase path>|<uid>` so several icons of one executable stay apart. Registry-only entries
//! use `path:<lowercase path>` and match every live icon of that executable.

pub const MAX_PINNED_ICONS: usize = 64;
pub const MAX_KEY_LENGTH: usize = 512;

pub fn key_for_path(path: Option<&str>) -> String {
    match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => format!("path:{}", path.to_lowercase()),
        None => String::new(),
    }
}

/// `KeyFor`: the canonical key of a live icon, or empty when it cannot be pinned.
pub fn key_for(guid: &str, path: Option<&str>, uid: u32) -> String {
    if !guid.is_empty() {
        return format!("guid:{guid}");
    }
    let path_key = key_for_path(path);
    if path_key.is_empty() {
        String::new()
    } else {
        format!("{path_key}|{uid}")
    }
}

/// `Matches`: the exact key, `path:<p>|<uid>`, or the path-only fallbacks `path:<p>` and `path:<p>|*`.
pub fn matches(key: &str, guid: &str, path: Option<&str>, uid: u32) -> bool {
    let key = key.trim();
    if key.is_empty() {
        return false;
    }
    let exact = key_for(guid, path, uid);
    if !exact.is_empty() && key.eq_ignore_ascii_case(&exact) {
        return true;
    }
    let path_key = key_for_path(path);
    if path_key.is_empty() {
        return false;
    }
    key.eq_ignore_ascii_case(&format!("{path_key}|{uid}"))
        || key.eq_ignore_ascii_case(&path_key)
        || key.eq_ignore_ascii_case(&format!("{path_key}|*"))
}

/// `FindMatchingKey`: the stored key for a live icon (an exact match first), so unpinning removes that entry.
pub fn find_matching_key(
    pinned: &[String],
    guid: &str,
    path: Option<&str>,
    uid: u32,
) -> Option<String> {
    let exact = key_for(guid, path, uid);
    if !exact.is_empty()
        && let Some(found) = pinned
            .iter()
            .find(|k| k.trim().eq_ignore_ascii_case(&exact))
    {
        return Some(found.clone());
    }
    pinned.iter().find(|k| matches(k, guid, path, uid)).cloned()
}

/// `MatchesPath`: a stored key for a registry app's path: `path:<p>` or `path:<p>|<anything>`.
pub fn matches_path(key: &str, path: Option<&str>) -> bool {
    let path_key = key_for_path(path);
    if path_key.is_empty() {
        return false;
    }
    let key = key.trim();
    if key.eq_ignore_ascii_case(&path_key) {
        return true;
    }
    key.len() > path_key.len()
        && key.is_char_boundary(path_key.len())
        && key[..path_key.len()].eq_ignore_ascii_case(&path_key)
        && key.as_bytes()[path_key.len()] == b'|'
}

/// `FindMatchingPathKey`: the stored key for a registry app (the exact path key first).
pub fn find_matching_path_key(pinned: &[String], path: Option<&str>) -> Option<String> {
    let exact = key_for_path(path);
    if exact.is_empty() {
        return None;
    }
    pinned
        .iter()
        .find(|k| k.trim().eq_ignore_ascii_case(&exact))
        .or_else(|| pinned.iter().find(|k| matches_path(k, path)))
        .cloned()
}

/// `FindPinIndex`: the earliest stored key matching a live icon (the strip's order).
pub fn find_pin_index(
    pinned: &[String],
    guid: &str,
    path: Option<&str>,
    uid: u32,
) -> Option<usize> {
    pinned.iter().position(|k| matches(k, guid, path, uid))
}

/// `StrategyFor`: path pins open their executable (activate its window or launch it); GUID and unknown pins
/// forward a click to the icon.
pub fn opens_by_path(key: &str) -> bool {
    let key = key.trim();
    if key.len() < 5 || !key[..5].eq_ignore_ascii_case("path:") {
        return false;
    }
    let mut path = &key[5..];
    if let Some((stem, suffix)) = path.rsplit_once('|')
        && (suffix == "*" || (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())))
    {
        path = stem;
    }
    !path.trim().is_empty()
}

/// `CanOpenByPath`: a rooted path to an .exe.
pub fn can_open_by_path(path: Option<&str>) -> bool {
    path.map(str::trim).is_some_and(|p| {
        std::path::Path::new(p).has_root() && p.to_ascii_lowercase().ends_with(".exe")
    })
}

/// `TogglePin`: adds or removes `key` (case-insensitively), dropping blank, too long and duplicate entries and
/// keeping at most 64. `None` when the list ends up empty.
pub fn toggle(pinned: Option<&[String]>, key: &str) -> Option<Vec<String>> {
    let mut list: Vec<String> = Vec::new();
    for entry in pinned.unwrap_or_default() {
        let entry = entry.trim();
        if entry.is_empty() || entry.len() > MAX_KEY_LENGTH {
            continue;
        }
        if !list.iter().any(|e| e.eq_ignore_ascii_case(entry)) && list.len() < MAX_PINNED_ICONS {
            list.push(entry.to_owned());
        }
    }
    let key = key.trim();
    if !key.is_empty() && key.len() <= MAX_KEY_LENGTH {
        match list.iter().position(|e| e.eq_ignore_ascii_case(key)) {
            Some(index) => {
                list.remove(index);
            }
            None if list.len() < MAX_PINNED_ICONS => list.push(key.to_owned()),
            None => {}
        }
    }
    (!list.is_empty()).then_some(list)
}

/// `CleanTitle`: the tooltip's first line, else the executable's name, else `fallback`.
pub fn clean_title(raw: &str, path: Option<&str>, fallback: &str) -> String {
    if let Some(line) = raw
        .split(['\r', '\n'])
        .map(str::trim)
        .find(|l| !l.is_empty())
    {
        return line.to_owned();
    }
    path.and_then(|p| std::path::Path::new(p).file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: Option<&str> = Some(r"C:\Apps\Tool.exe");

    #[test]
    fn keys_like_opentaskbar() {
        assert_eq!(key_for("ab-cd", EXE, 3), "guid:ab-cd");
        assert_eq!(key_for("", EXE, 3), r"path:c:\apps\tool.exe|3");
        assert_eq!(key_for("", None, 3), "");
        assert!(matches(r"PATH:C:\Apps\Tool.exe|3", "", EXE, 3));
        assert!(matches(r"path:c:\apps\tool.exe", "", EXE, 9));
        assert!(matches(r"path:c:\apps\tool.exe|*", "", EXE, 9));
        assert!(!matches(r"path:c:\apps\tool.exe|4", "", EXE, 3));
        assert!(!matches("guid:other", "ab-cd", EXE, 3));
    }

    #[test]
    fn registry_apps_match_path_keys() {
        assert!(matches_path(r"path:c:\apps\tool.exe|7", EXE));
        assert!(matches_path(r"PATH:C:\Apps\Tool.exe", EXE));
        assert!(!matches_path(r"path:c:\apps\tool.exex", EXE));
        assert!(!matches_path("guid:a", EXE));
        let pinned: Vec<String> = vec![
            r"path:c:\apps\tool.exe|7".into(),
            r"path:c:\apps\tool.exe".into(),
        ];
        assert_eq!(
            find_matching_path_key(&pinned, EXE).as_deref(),
            Some(r"path:c:\apps\tool.exe")
        );
        assert_eq!(
            find_matching_path_key(&pinned[..1], EXE).as_deref(),
            Some(r"path:c:\apps\tool.exe|7")
        );
    }

    #[test]
    fn strip_order_and_strategy() {
        let pinned: Vec<String> = vec!["guid:x".into(), r"path:c:\apps\tool.exe".into()];
        assert_eq!(find_pin_index(&pinned, "", EXE, 7), Some(1));
        assert_eq!(find_pin_index(&pinned, "x", None, 0), Some(0));
        assert_eq!(find_pin_index(&pinned, "y", None, 0), None);
        assert!(opens_by_path(r"PATH:c:\a.exe|3"));
        assert!(opens_by_path(r"path:c:\a.exe|*"));
        assert!(!opens_by_path("path:|3"));
        assert!(!opens_by_path("guid:x"));
        assert!(can_open_by_path(EXE));
        assert!(!can_open_by_path(Some("tool.exe")));
        assert!(!can_open_by_path(Some(r"C:\Apps\tool.lnk")));
    }

    #[test]
    fn toggling_dedupes_and_empties() {
        let list = toggle(None, "guid:a").unwrap();
        assert_eq!(list, vec!["guid:a"]);
        let list = toggle(
            Some(&[" guid:a ".into(), "GUID:A".into(), "".into()]),
            "guid:b",
        )
        .unwrap();
        assert_eq!(list, vec!["guid:a", "guid:b"]);
        assert_eq!(toggle(Some(&["guid:a".into()]), "GUID:A"), None);
    }

    #[test]
    fn titles_take_the_first_line() {
        assert_eq!(clean_title("  Name\r\nmore", EXE, "x"), "Name");
        assert_eq!(clean_title("", EXE, "x"), "Tool");
        assert_eq!(clean_title("", None, "x"), "x");
    }
}
