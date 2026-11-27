//! Global hotkeys (OpenTaskbar's `HotkeyBinding`): the preset choices and custom bindings such as `Ctrl+Alt+D`,
//! validated the same way: Ctrl or Alt is required, the Windows key is never allowed (a Win key-up lost across a
//! lock or secure-desktop switch can leave Windows thinking Win is held), and Escape, Print Screen and bare modifiers
//! cannot be the key.

use ot_core::settings::HotkeyChoice;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub modifiers: u32,
    pub key: u32,
}

/// `HotkeyBinding.FromChoice`.
pub fn from_choice(choice: HotkeyChoice) -> Option<Binding> {
    let (modifiers, key) = match choice {
        HotkeyChoice::AltSpace => (MOD_ALT, 0x20),
        HotkeyChoice::CtrlSpace => (MOD_CONTROL, 0x20),
        HotkeyChoice::CtrlAltSpace => (MOD_CONTROL | MOD_ALT, 0x20),
        HotkeyChoice::CtrlAltD => (MOD_CONTROL | MOD_ALT, 0x44),
        _ => return None,
    };
    Some(Binding { modifiers, key })
}

/// `HotkeyBinding.Resolve`: a custom binding wins ("Disabled" turns the hotkey off; an invalid one disables it too),
/// otherwise the preset.
pub fn resolve(choice: HotkeyChoice, custom: Option<&str>) -> Option<Binding> {
    match custom.map(str::trim) {
        Some(text) if text.eq_ignore_ascii_case("Disabled") => None,
        Some(text) if !text.is_empty() => parse(text),
        _ => from_choice(choice),
    }
}

/// `HotkeyBinding.TryParse`: `Modifier+…+Key`, case-insensitive.
pub fn parse(text: &str) -> Option<Binding> {
    let tokens = tokenize(text.trim())?;
    if tokens.len() < 2 {
        return None;
    }
    let mut modifiers = 0u32;
    let mut key_token: Option<&str> = None;
    for token in &tokens {
        let flag = match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" | "lwin" | "rwin" => return None,
            _ => {
                if key_token.is_some() {
                    return None;
                }
                key_token = Some(token);
                continue;
            }
        };
        if modifiers & flag != 0 {
            return None;
        }
        modifiers |= flag;
    }
    if modifiers & (MOD_ALT | MOD_CONTROL) == 0 {
        return None;
    }
    let key = key_from_token(key_token?)?;
    valid_key(key).then_some(Binding { modifiers, key })
}

/// `TryTokenize`: splits on `+`; a final lone `+` is the plus key (`Ctrl++`).
fn tokenize(text: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            return None;
        }
        if i == chars.len() - 1 && chars[i] == '+' {
            tokens.push("+".to_owned());
            break;
        }
        match chars[i..].iter().position(|&c| c == '+') {
            None => {
                let part: String = chars[i..].iter().collect::<String>().trim().to_owned();
                if part.is_empty() {
                    return None;
                }
                tokens.push(part);
                break;
            }
            Some(offset) => {
                let token: String = chars[i..i + offset]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_owned();
                if token.is_empty() {
                    return None;
                }
                tokens.push(token);
                i += offset + 1;
                if i == chars.len() {
                    return None;
                }
            }
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}

/// The virtual key for a WPF `Key` name or a single character (`TryParseKeyToken`).
fn key_from_token(token: &str) -> Option<u32> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let c = c.to_ascii_uppercase();
        return match c {
            'A'..='Z' | '0'..='9' => Some(c as u32),
            '+' => Some(0xBB),
            '-' => Some(0xBD),
            ',' => Some(0xBC),
            '.' => Some(0xBE),
            '/' => Some(0xBF),
            ';' => Some(0xBA),
            '\'' => Some(0xDE),
            '[' => Some(0xDB),
            ']' => Some(0xDD),
            '\\' => Some(0xDC),
            '`' => Some(0xC0),
            _ => None,
        };
    }
    if token.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    let named = match lower.as_str() {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "backspace" | "back" => 0x08,
        "pageup" | "prior" => 0x21,
        "pagedown" | "next" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "plus" | "oemplus" => 0xBB,
        "minus" | "oemminus" => 0xBD,
        "comma" | "oemcomma" => 0xBC,
        "period" | "oemperiod" => 0xBE,
        "oemquestion" | "oem2" => 0xBF,
        "oemsemicolon" | "oem1" => 0xBA,
        "oemquotes" | "oem7" => 0xDE,
        "oemopenbrackets" | "oem4" => 0xDB,
        "oemclosebrackets" | "oem6" => 0xDD,
        "oempipe" | "oem5" => 0xDC,
        "oemtilde" | "oem3" => 0xC0,
        "multiply" => 0x6A,
        "add" => 0x6B,
        "subtract" => 0x6D,
        "decimal" => 0x6E,
        "divide" => 0x6F,
        "pause" => 0x13,
        "capital" | "capslock" => 0x14,
        "scroll" => 0x91,
        "numlock" => 0x90,
        "apps" => 0x5D,
        "escape" => 0x1B,
        "snapshot" | "printscreen" => 0x2C,
        _ => {
            if let Some(n) = lower
                .strip_prefix("numpad")
                .and_then(|n| n.parse::<u32>().ok())
                && n <= 9
            {
                return Some(0x60 + n);
            }
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
                && (1..=24).contains(&n)
            {
                return Some(0x6F + n);
            }
            if let Some(d) = lower.strip_prefix('d').and_then(|n| n.parse::<u32>().ok())
                && d <= 9
            {
                return Some(0x30 + d);
            }
            return None;
        }
    };
    Some(named)
}

/// `IsValidVirtualKey` and `IsDisallowedKey`.
fn valid_key(vk: u32) -> bool {
    (1..=0xFE).contains(&vk)
        && !matches!(vk, 0x1B | 0x2C | 0x10 | 0x11 | 0x12 | 0x5B | 0x5C)
        && !(0xA0..=0xA5).contains(&vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_opentaskbar() {
        assert_eq!(
            parse("Ctrl+Alt+D"),
            Some(Binding {
                modifiers: MOD_CONTROL | MOD_ALT,
                key: 0x44
            })
        );
        assert_eq!(
            parse("ctrl + shift + f5"),
            Some(Binding {
                modifiers: MOD_CONTROL | MOD_SHIFT,
                key: 0x74
            })
        );
        assert_eq!(parse("Ctrl++").map(|b| b.key), Some(0xBB));
        assert_eq!(parse("Alt+Space").map(|b| b.key), Some(0x20));
        assert_eq!(parse("Shift+A"), None, "Ctrl or Alt is required");
        assert_eq!(parse("Win+Alt+D"), None, "the Windows key is never allowed");
        assert_eq!(parse("Ctrl+Escape"), None);
        assert_eq!(parse("Ctrl+A+B"), None);
        assert_eq!(parse("Ctrl+Ctrl+A"), None);
        assert_eq!(parse("Ctrl+"), None);
        assert_eq!(parse("Ctrl+123"), None);
    }

    #[test]
    fn custom_text_overrides_the_preset() {
        assert_eq!(resolve(HotkeyChoice::CtrlAltD, Some("Disabled")), None);
        assert_eq!(
            resolve(HotkeyChoice::CtrlAltD, None),
            from_choice(HotkeyChoice::CtrlAltD)
        );
        assert_eq!(
            resolve(HotkeyChoice::CtrlAltD, Some("Alt+F1")).map(|b| b.key),
            Some(0x70)
        );
        assert_eq!(resolve(HotkeyChoice::CtrlAltD, Some("nonsense")), None);
    }
}
