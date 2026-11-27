//! Global hotkey bindings: the legacy fixed choices and custom "Ctrl+Alt+K" strings, parsed exactly like
//! OpenTaskbar's `HotkeyBinding.TryParse`.

use crate::hotkey_keys::WPF_KEY_VK;
use crate::settings::HotkeyChoice;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub modifiers: u32,
    pub virtual_key: u8,
}

impl HotkeyBinding {
    pub fn from_choice(choice: HotkeyChoice) -> Option<Self> {
        let (modifiers, virtual_key) = match choice {
            HotkeyChoice::AltSpace => (MOD_ALT, 0x20),
            HotkeyChoice::CtrlSpace => (MOD_CONTROL, 0x20),
            HotkeyChoice::CtrlAltSpace => (MOD_CONTROL | MOD_ALT, 0x20),
            HotkeyChoice::CtrlAltD => (MOD_CONTROL | MOD_ALT, 0x44),
            HotkeyChoice::Disabled | HotkeyChoice::WinAltD => return None,
        };
        Some(Self {
            modifiers,
            virtual_key,
        })
    }

    /// The effective binding: a custom string wins over the legacy choice; "Disabled" turns it off.
    pub fn resolve(choice: HotkeyChoice, custom: Option<&str>) -> Option<Self> {
        match custom {
            Some(text) if text.eq_ignore_ascii_case("Disabled") => None,
            Some(text) if !text.trim().is_empty() => Self::parse(text),
            _ => Self::from_choice(choice),
        }
    }

    pub fn is_valid_custom(custom: Option<&str>) -> bool {
        match custom {
            None => true,
            Some(text) => text.eq_ignore_ascii_case("Disabled") || Self::parse(text).is_some(),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Disabled") {
            return None;
        }
        let tokens = tokenize(trimmed)?;
        if tokens.len() < 2 {
            return None;
        }
        let mut modifiers = 0;
        let mut key_token = None;
        for token in tokens {
            let flag =
                if token.eq_ignore_ascii_case("Ctrl") || token.eq_ignore_ascii_case("Control") {
                    MOD_CONTROL
                } else if token.eq_ignore_ascii_case("Alt") {
                    MOD_ALT
                } else if token.eq_ignore_ascii_case("Shift") {
                    MOD_SHIFT
                } else {
                    if ["Win", "Windows", "LWin", "RWin"]
                        .iter()
                        .any(|w| token.eq_ignore_ascii_case(w))
                    {
                        return None;
                    }
                    if key_token.replace(token).is_some() {
                        return None; // two non-modifier keys
                    }
                    continue;
                };
            if modifiers & flag != 0 {
                return None;
            }
            modifiers |= flag;
        }
        if modifiers & (MOD_ALT | MOD_CONTROL) == 0 {
            return None;
        }
        let virtual_key = key_token.and_then(virtual_key_for)?;
        Some(Self {
            modifiers,
            virtual_key,
        })
    }
}

impl HotkeyBinding {
    /// `TryFromKeyEvent`: a key pressed while a hotkey is recorded. `Err(Some(key))` is a message to show,
    /// `Err(None)` a key to ignore silently (a lone modifier, Escape, Print Screen, an unnamed key).
    pub fn from_key_event(
        virtual_key: u8,
        ctrl: bool,
        alt: bool,
        shift: bool,
        win: bool,
    ) -> Result<Self, Option<&'static str>> {
        // Modifiers on their own, IME processing.
        if matches!(virtual_key, 0 | 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C | 0xE5) {
            return Err(None);
        }
        if win {
            return Err(Some("HotkeyWinNotAllowed"));
        }
        if !ctrl && !alt {
            return Err(Some("HotkeyModifierRequired"));
        }
        // Escape and Print Screen are never hotkeys.
        if matches!(virtual_key, 0x1B | 0x2C | 0xFF) {
            return Err(None);
        }
        let modifiers = if ctrl { MOD_CONTROL } else { 0 }
            | if alt { MOD_ALT } else { 0 }
            | if shift { MOD_SHIFT } else { 0 };
        let binding = Self {
            modifiers,
            virtual_key,
        };
        // Only keys the settings file can name again (`FormatKeyName` and back through `TryParse`).
        match Self::parse(&binding.display()) {
            Some(back) if back == binding => Ok(binding),
            _ => Err(None),
        }
    }

    /// `FormatDisplayText`: "Ctrl+Alt+D", "Alt+Space", "Ctrl+Shift++".
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if self.modifiers & MOD_CONTROL != 0 {
            parts.push("Ctrl".into());
        }
        if self.modifiers & MOD_ALT != 0 {
            parts.push("Alt".into());
        }
        if self.modifiers & MOD_SHIFT != 0 {
            parts.push("Shift".into());
        }
        parts.push(key_name(self.virtual_key));
        parts.join("+")
    }
}

/// `FormatKeyName`: the C# names for common keys, otherwise WPF's `Key` name for the virtual key.
fn key_name(vk: u8) -> String {
    let simple = match vk {
        b'A'..=b'Z' | b'0'..=b'9' => return (vk as char).to_string(),
        0x70..=0x87 => return format!("F{}", vk - 0x6F),
        0x60..=0x69 => return format!("NumPad{}", vk - 0x60),
        0x20 => "Space",
        0x0D => "Enter",
        0x09 => "Tab",
        0x08 => "Backspace",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x26 => "Up",
        0x28 => "Down",
        0x25 => "Left",
        0x27 => "Right",
        0x2D => "Insert",
        0x2E => "Delete",
        0xBB => "+",
        0xBD => "-",
        0xBC => ",",
        0xBE => ".",
        0xBF => "/",
        0xBA => ";",
        0xDE => "'",
        0xDB => "[",
        0xDD => "]",
        0xDC => "\\",
        0xC0 => "`",
        // `Key.ToString()` for the rest WPF names.
        0x6A => "Multiply",
        0x6B => "Add",
        0x6C => "Separator",
        0x6D => "Subtract",
        0x6E => "Decimal",
        0x6F => "Divide",
        0x13 => "Pause",
        0x14 => "Capital",
        0x90 => "NumLock",
        0x91 => "Scroll",
        0x5D => "Apps",
        0xE2 => "OemBackslash",
        0xAD => "VolumeMute",
        0xAE => "VolumeDown",
        0xAF => "VolumeUp",
        0xB0 => "MediaNextTrack",
        0xB1 => "MediaPreviousTrack",
        0xB2 => "MediaStop",
        0xB3 => "MediaPlayPause",
        0xA6 => "BrowserBack",
        0xA7 => "BrowserForward",
        0xA8 => "BrowserRefresh",
        0xA9 => "BrowserStop",
        0xAA => "BrowserSearch",
        0xAB => "BrowserFavorites",
        0xAC => "BrowserHome",
        0xB4 => "LaunchMail",
        0xB5 => "SelectMedia",
        0xB6 => "LaunchApplication1",
        0xB7 => "LaunchApplication2",
        _ => "",
    };
    simple.to_owned()
}

/// True when both bindings exist and press the same combination.
pub fn conflicts(a: Option<HotkeyBinding>, b: Option<HotkeyBinding>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
}

/// Splits on '+', where a trailing lone '+' is the plus key itself ("Ctrl++").
fn tokenize(text: &str) -> Option<Vec<&str>> {
    let mut tokens = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        if rest == "+" {
            tokens.push("+");
            break;
        }
        match rest.find('+') {
            None => {
                tokens.push(rest.trim());
                break;
            }
            Some(plus) => {
                let token = rest[..plus].trim();
                if token.is_empty() {
                    return None;
                }
                tokens.push(token);
                rest = &rest[plus + 1..];
                if rest.is_empty() {
                    return None;
                }
            }
        }
    }
    Some(tokens)
}

fn virtual_key_for(token: &str) -> Option<u8> {
    let mut chars = token.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let ch = ch.to_ascii_uppercase();
        return match ch {
            'A'..='Z' | '0'..='9' => Some(ch as u8),
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
    let named = [
        ("Space", 0x20),
        ("Enter", 0x0D),
        ("Return", 0x0D),
        ("Tab", 0x09),
        ("Backspace", 0x08),
        ("Back", 0x08),
        ("PageUp", 0x21),
        ("Prior", 0x21),
        ("PageDown", 0x22),
        ("Next", 0x22),
        ("Plus", 0xBB),
        ("Minus", 0xBD),
        ("Comma", 0xBC),
        ("Period", 0xBE),
    ];
    if let Some((_, vk)) = named
        .iter()
        .find(|(name, _)| token.eq_ignore_ascii_case(name))
    {
        return Some(*vk);
    }
    // Raw numbers are rejected so "65" cannot select a key by enum value.
    if token.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return None;
    }
    WPF_KEY_VK
        .iter()
        .find(|(name, _)| token.eq_ignore_ascii_case(name))
        .map(|(_, vk)| *vk)
}

#[cfg(test)]
mod key_event_tests {
    use super::*;

    #[test]
    fn key_events_like_try_from_key_event() {
        let b = HotkeyBinding::from_key_event(0x44, true, true, false, false).unwrap();
        assert_eq!(b.display(), "Ctrl+Alt+D");
        assert_eq!(
            HotkeyBinding::from_key_event(0x20, false, true, false, false)
                .unwrap()
                .display(),
            "Alt+Space"
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0xBB, true, false, true, false)
                .unwrap()
                .display(),
            "Ctrl+Shift++"
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0x75, true, false, false, false)
                .unwrap()
                .display(),
            "Ctrl+F6"
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0x44, false, false, true, false),
            Err(Some("HotkeyModifierRequired"))
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0x44, true, false, false, true),
            Err(Some("HotkeyWinNotAllowed"))
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0x11, true, false, false, false),
            Err(None)
        );
        assert_eq!(
            HotkeyBinding::from_key_event(0x1B, true, false, false, false),
            Err(None)
        );
        assert_eq!(HotkeyBinding::parse("Ctrl+Alt+D"), Some(b));
    }
}
