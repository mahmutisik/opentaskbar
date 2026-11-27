//! UI language and string catalogs. The 14 catalogs are OpenTaskbar's own `Languages/*.json` files, embedded
//! in the binary. Unlike the C# app, which parses every catalog at startup, a catalog is parsed the first time
//! it is needed, so a normal run parses only the active language and English.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const ENGLISH: &str = "en-US";

/// Embedded catalogs: culture name and raw JSON.
static CATALOG_SOURCES: &[(&str, &str)] = &[
    ("de-DE", include_str!("../lang/de-DE.json")),
    ("en-US", include_str!("../lang/en-US.json")),
    ("es-ES", include_str!("../lang/es-ES.json")),
    ("fr-FR", include_str!("../lang/fr-FR.json")),
    ("it-IT", include_str!("../lang/it-IT.json")),
    ("ja-JP", include_str!("../lang/ja-JP.json")),
    ("ko-KR", include_str!("../lang/ko-KR.json")),
    ("nl-NL", include_str!("../lang/nl-NL.json")),
    ("pl-PL", include_str!("../lang/pl-PL.json")),
    ("pt-BR", include_str!("../lang/pt-BR.json")),
    ("ru-RU", include_str!("../lang/ru-RU.json")),
    ("tr-TR", include_str!("../lang/tr-TR.json")),
    ("uk-UA", include_str!("../lang/uk-UA.json")),
    ("zh-CN", include_str!("../lang/zh-CN.json")),
];

/// A UI language, stored as a .NET culture name ("tr-TR"). English is the default.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppLanguage(Box<str>);

impl Default for AppLanguage {
    fn default() -> Self {
        Self::english()
    }
}

impl fmt::Display for AppLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AppLanguage {
    pub fn english() -> Self {
        Self(ENGLISH.into())
    }

    /// Builds a language from a culture name, normalizing its casing like `CultureInfo.Name` does.
    /// Returns `None` for text that is not shaped like a culture name.
    pub fn new(culture_name: &str) -> Option<Self> {
        normalize_culture(culture_name).map(|name| Self(name.into_boxed_str()))
    }

    pub fn culture_name(&self) -> &str {
        &self.0
    }

    /// ISO 639-1 language code ("tr").
    pub fn two_letter(&self) -> &str {
        self.0.split('-').next().unwrap_or(&self.0)
    }

    /// True when a bundled catalog shares this language's two-letter code (`LanguageCatalog.IsSupported`).
    pub fn is_supported(&self) -> bool {
        CATALOG_SOURCES
            .iter()
            .any(|(name, _)| two_letter_of(name).eq_ignore_ascii_case(self.two_letter()))
    }

    /// The language a first launch uses for a system UI culture (`LanguageCatalog.FromCulture`).
    pub fn from_culture(culture_name: &str) -> Self {
        let Some(language) = Self::new(culture_name) else {
            return Self::english();
        };
        if let Some((name, _)) = CATALOG_SOURCES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(language.culture_name()))
        {
            return Self((*name).into());
        }
        match CATALOG_SOURCES
            .iter()
            .find(|(name, _)| two_letter_of(name).eq_ignore_ascii_case(language.two_letter()))
        {
            Some((name, _)) if !language.culture_name().contains('-') => Self((*name).into()),
            Some(_) => language,
            None => Self::english(),
        }
    }

    /// All bundled languages.
    pub fn bundled() -> impl Iterator<Item = AppLanguage> {
        CATALOG_SOURCES.iter().map(|(name, _)| Self((*name).into()))
    }
}

fn two_letter_of(culture: &str) -> &str {
    culture.split('-').next().unwrap_or(culture)
}

/// "tr-tr" -> "tr-TR", "zh-hans-cn" -> "zh-Hans-CN". Accepts only ASCII letters/digits separated by '-'.
fn normalize_culture(text: &str) -> Option<String> {
    let text = text.trim();
    let mut parts = text.split(['-', '_']);
    let language = parts.next()?;
    if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut name = language.to_ascii_lowercase();
    for part in parts {
        if part.is_empty() || part.len() > 8 || !part.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        name.push('-');
        match part.len() {
            2 => name.push_str(&part.to_ascii_uppercase()),
            4 => {
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    name.push(first.to_ascii_uppercase());
                    name.push_str(&chars.as_str().to_ascii_lowercase());
                }
            }
            _ => name.push_str(part),
        }
    }
    Some(name)
}

impl Serialize for AppLanguage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Reads a culture name, the legacy enum names ("Turkish", "English", "Japanese") or their numeric values (0-2).
impl<'de> Deserialize<'de> for AppLanguage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LanguageVisitor;
        impl Visitor<'_> for LanguageVisitor {
            type Value = AppLanguage;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a supported culture name")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<AppLanguage, E> {
                match v {
                    0 => Ok(AppLanguage("tr-TR".into())),
                    1 => Ok(AppLanguage::english()),
                    2 => Ok(AppLanguage("ja-JP".into())),
                    _ => Err(E::custom("legacy language value is not supported")),
                }
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<AppLanguage, E> {
                self.visit_i64(i64::try_from(v).map_err(E::custom)?)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<AppLanguage, E> {
                let legacy = match v.trim() {
                    s if s.eq_ignore_ascii_case("Turkish") => Some("tr-TR"),
                    s if s.eq_ignore_ascii_case("English") => Some(ENGLISH),
                    s if s.eq_ignore_ascii_case("Japanese") => Some("ja-JP"),
                    _ => None,
                };
                if let Some(name) = legacy {
                    return Ok(AppLanguage(name.into()));
                }
                match AppLanguage::new(v) {
                    Some(language) if language.is_supported() => Ok(language),
                    Some(_) => Err(E::custom("language is not supported")),
                    None => Err(E::custom("language culture is invalid")),
                }
            }
        }
        deserializer.deserialize_any(LanguageVisitor)
    }
}

/// Localized strings with OpenTaskbar's fallback order: exact culture, other cultures of the same language
/// (neutral first, then by name), English.
pub struct Catalog;

type Strings = HashMap<Box<str>, Box<str>>;

fn parsed(index: usize) -> &'static Strings {
    static PARSED: [OnceLock<Strings>; 14] = [const { OnceLock::new() }; 14];
    PARSED[index].get_or_init(|| {
        // The sources are checked by tests; a malformed catalog must never take the dock down.
        serde_json::from_str(CATALOG_SOURCES[index].1).unwrap_or_default()
    })
}

impl Catalog {
    /// The localized string, or the key itself when no catalog (English included) has it.
    pub fn get<'k>(key: &'k str, language: &AppLanguage) -> Cow<'k, str> {
        match Self::try_get(key, language) {
            Some(value) => Cow::Borrowed(value),
            None => Cow::Borrowed(key),
        }
    }

    pub fn try_get(key: &str, language: &AppLanguage) -> Option<&'static str> {
        let exact = CATALOG_SOURCES
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(language.culture_name()));
        if let Some(value) = exact.and_then(|index| parsed(index).get(key)) {
            return Some(value);
        }
        let mut fallbacks: Vec<usize> = (0..CATALOG_SOURCES.len())
            .filter(|&index| {
                let name = CATALOG_SOURCES[index].0;
                Some(index) != exact
                    && two_letter_of(name).eq_ignore_ascii_case(language.two_letter())
            })
            .collect();
        fallbacks.sort_by_key(|&index| {
            let name = CATALOG_SOURCES[index].0;
            (name.contains('-'), name.to_ascii_lowercase())
        });
        for index in fallbacks {
            if let Some(value) = parsed(index).get(key) {
                return Some(value);
            }
        }
        let english = CATALOG_SOURCES
            .iter()
            .position(|(name, _)| *name == ENGLISH)?;
        parsed(english).get(key).map(|value| &**value)
    }

    /// Every key's string for `language`, with `get`'s fallbacks (for a page that shows many of them).
    pub fn all(language: &AppLanguage) -> HashMap<&'static str, &'static str> {
        let mut out: HashMap<&'static str, &'static str> = HashMap::new();
        let mut layer = |index: usize| {
            for (k, v) in parsed(index) {
                out.insert(&**k, &**v);
            }
        };
        if let Some(english) = CATALOG_SOURCES
            .iter()
            .position(|(name, _)| *name == ENGLISH)
        {
            layer(english);
        }
        let exact = CATALOG_SOURCES
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(language.culture_name()));
        let mut fallbacks: Vec<usize> = (0..CATALOG_SOURCES.len())
            .filter(|&index| {
                let name = CATALOG_SOURCES[index].0;
                Some(index) != exact
                    && two_letter_of(name).eq_ignore_ascii_case(language.two_letter())
            })
            .collect();
        fallbacks.sort_by_key(|&index| {
            let name = CATALOG_SOURCES[index].0;
            (name.contains('-'), name.to_ascii_lowercase())
        });
        // Lowest priority first: later layers win.
        for index in fallbacks.into_iter().rev() {
            layer(index);
        }
        if let Some(index) = exact {
            layer(index);
        }
        out
    }
}

/// The user's Windows display language as a culture name ("tr-TR"), or English when it cannot be read.
pub fn system_ui_culture() -> String {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
            fn LCIDToLocaleName(locale: u32, name: *mut u16, len: i32, flags: u32) -> i32;
        }
        let mut buffer = [0u16; 85];
        // SAFETY: plain Win32 calls; the buffer length is passed and LCIDToLocaleName null-terminates it.
        let written = unsafe {
            let lcid = u32::from(GetUserDefaultUILanguage());
            LCIDToLocaleName(lcid, buffer.as_mut_ptr(), buffer.len() as i32, 0)
        };
        if written > 1 {
            return String::from_utf16_lossy(&buffer[..written as usize - 1]);
        }
    }
    ENGLISH.to_owned()
}
