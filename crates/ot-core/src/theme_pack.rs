//! Theme packs (`ThemePack`, `ThemePacks`, `WidgetPack` and `ThemePackTransfer` in OpenTaskbar): folders under
//! `%LOCALAPPDATA%\OpenTaskBar-Rust\Themes` with `theme.json`, an optional widget pack (`widgets\pack.json`, design
//! token overrides) and an optional icon pack (`icons\pack.json`, not drawn yet). The six bundled themes are copied
//! there on start without replacing user files, as OpenTaskbar does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

pub const MANIFEST_FILE: &str = "theme.json";
pub const WIDGET_FOLDER: &str = "widgets";
pub const ICON_FOLDER: &str = "icons";
pub const PACK_FILE: &str = "pack.json";
/// `ThemePackTransfer.ExportExtension`.
pub const EXPORT_EXTENSION: &str = ".ottheme";
pub const MAX_FILE_COUNT: usize = 512;
pub const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PACK_BYTES: u64 = 128 * 1024;
/// `theme.json` and `icons\pack.json` are read only up to this size (upstream's `MAX_MANIFEST_BYTES`).
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_GRADIENT_STOPS: usize = 16;
const MAX_DIMENSION: f64 = 2048.0;
const MAX_TOKENS: usize = 256;

/// `ThemePack.BuiltInThemeIds`.
pub const BUILT_IN: [&str; 6] = [
    "catppuccin",
    "gruvbox",
    "nord",
    "prism",
    "solarized",
    "tokyo-night",
];

macro_rules! bundled {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../../../assets/themes/", $path)) as &[u8])),*]
    };
}

/// The bundled theme files (OpenTaskbar embeds `themes\**` as `OpenTaskbar.BuiltInThemes/` resources).
const BUNDLED: &[(&str, &[u8])] = bundled![
    "catppuccin/theme.json",
    "catppuccin/widgets/pack.json",
    "gruvbox/theme.json",
    "gruvbox/widgets/pack.json",
    "nord/theme.json",
    "nord/widgets/pack.json",
    "prism/LICENSE",
    "prism/README.md",
    "prism/theme.json",
    "prism/widgets/pack.json",
    "solarized/theme.json",
    "solarized/widgets/pack.json",
    "tokyo-night/theme.json",
    "tokyo-night/widgets/pack.json",
];

/// `ThemePack.IsBuiltInTheme`.
pub fn is_built_in(id: &str) -> bool {
    BUILT_IN.iter().any(|b| b.eq_ignore_ascii_case(id))
}

/// `ThemePacks.Root`.
pub fn root() -> PathBuf {
    crate::store::app_data_dir().join("Themes")
}

/// `ThemePacks.InstallBuiltInThemes`: copies the bundled themes into the theme folder; existing files stay.
pub fn install_built_in() -> Result<(), &'static str> {
    install_built_in_at(&root())
}

fn install_built_in_at(root: &Path) -> Result<(), &'static str> {
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        for (relative, bytes) in BUNDLED {
            let target = root.join(relative.replace('/', "\\"));
            if target.exists() {
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, bytes)?;
        }
        Ok(())
    })();
    result.map_err(|error| {
        crate::log::warn(format!(
            "Built-in theme packs could not be installed: {error}"
        ));
        "ErrorThemeInstall"
    })
}

// ---------- Lenient JSON ----------

/// System.Text.Json with `ReadCommentHandling.Skip` and `AllowTrailingCommas`: `//` and `/* */` comments and
/// trailing commas are removed before parsing.
pub fn parse_lenient(text: &str) -> Option<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ',' => {
                // A comma followed (after whitespace and comments) by a closing bracket is dropped.
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                        continue;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                        j += 2;
                        while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                            j += 1;
                        }
                        j += 2;
                        continue;
                    }
                    break;
                }
                if !matches!(chars.get(j), Some('}') | Some(']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    serde_json::from_str(&out).ok()
}

/// A property by name, ignoring case (`PropertyNameCaseInsensitive`).
fn property<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

fn read_json(path: &Path, limit: Option<u64>) -> Result<Value, String> {
    let meta = std::fs::metadata(path).map_err(|_| format!("{} not found.", path.display()))?;
    if let Some(limit) = limit
        && meta.len() > limit
    {
        return Err(format!(
            "{} exceeds the {limit}-byte limit.",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_lenient(&text).ok_or_else(|| format!("{} is not valid JSON.", path.display()))
}

// ---------- Manifest ----------

/// `ThemePackManifest`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub preserve_original_icons: bool,
}

impl Manifest {
    fn from_value(value: &Value) -> Option<Manifest> {
        let object = value.as_object()?;
        let text = |name: &str| {
            property(object, name)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Some(Manifest {
            name: text("name"),
            author: text("author"),
            description: text("description"),
            preserve_original_icons: property(object, "preserveOriginalIcons")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// The file OpenTaskbar writes (`JsonSerializer.Serialize(manifest, WriteIndented)`).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "Name": self.name,
            "Author": self.author,
            "Description": self.description,
            "PreserveOriginalIcons": self.preserve_original_icons,
        }))
        .unwrap_or_default()
    }
}

// ---------- Widget pack ----------

/// A brush token: a color or a linear gradient (`WidgetPack.ParseBrush`), colors as ARGB.
#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    pub stops: Vec<(f32, u32)>,
    /// Start and end points in the unit square.
    pub start: (f32, f32),
    pub end: (f32, f32),
}

impl Brush {
    pub fn solid(argb: u32) -> Brush {
        Brush {
            stops: vec![(0.0, argb)],
            start: (0.0, 0.0),
            end: (0.0, 1.0),
        }
    }

    /// The color of a solid brush, else the first stop.
    pub fn first(&self) -> u32 {
        self.stops.first().map(|s| s.1).unwrap_or(0)
    }

    pub fn is_solid(&self) -> bool {
        self.stops.len() == 1
    }
}

/// A token value, typed by its JSON shape; the consumer decides which type it accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Brush(Brush),
    /// A number (size, opacity).
    Number(f64),
    /// One to four numbers: corner radius or thickness (left, top, right, bottom).
    Numbers([f64; 4]),
    /// A font family or a background mode.
    Text(String),
}

/// `ColorConverter` for `#RGB`, `#ARGB`, `#RRGGBB` and `#AARRGGBB`.
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let expand = |s: &str| -> String { s.chars().flat_map(|c| [c, c]).collect() };
    let full = match hex.len() {
        3 => format!("FF{}", expand(hex)),
        4 => expand(hex),
        6 => format!("FF{hex}"),
        8 => hex.to_owned(),
        _ => return None,
    };
    u32::from_str_radix(&full, 16).ok()
}

fn parse_numbers(value: &Value) -> Option<[f64; 4]> {
    let ok = |n: f64| n.is_finite() && (0.0..=MAX_DIMENSION).contains(&n);
    if let Some(n) = value.as_f64() {
        return ok(n).then_some([n; 4]);
    }
    let parts: Vec<&str> = value.as_str()?.split(',').map(str::trim).collect();
    let numbers: Vec<f64> = parts
        .iter()
        .map(|p| p.parse::<f64>().ok().filter(|n| ok(*n)))
        .collect::<Option<_>>()?;
    match numbers.len() {
        1 => Some([numbers[0]; 4]),
        2 => Some([numbers[0], numbers[1], numbers[0], numbers[1]]),
        4 => Some([numbers[0], numbers[1], numbers[2], numbers[3]]),
        _ => None,
    }
}

fn parse_brush(value: &Value) -> Option<Brush> {
    if let Some(text) = value.as_str() {
        return parse_color(text).map(Brush::solid);
    }
    let object = value.as_object()?;
    let stops = property(object, "stops")?.as_array()?;
    if !(2..=MAX_GRADIENT_STOPS).contains(&stops.len()) {
        return None;
    }
    let colors: Vec<u32> = stops
        .iter()
        .map(|s| s.as_str().and_then(parse_color))
        .collect::<Option<_>>()?;
    let direction = property(object, "direction")
        .and_then(Value::as_str)
        .unwrap_or("vertical")
        .to_ascii_lowercase();
    let (mut start, mut end) = match direction.as_str() {
        "horizontal" => ((0.0, 0.0), (1.0, 0.0)),
        "diagonal" => ((0.0, 0.0), (1.0, 1.0)),
        _ => ((0.0, 0.0), (0.0, 1.0)),
    };
    if let Some(angle) = property(object, "angle").and_then(Value::as_f64) {
        if !angle.is_finite() || !(0.0..=360.0).contains(&angle) {
            return None;
        }
        let r = angle.to_radians();
        start = (0.5 - r.sin() / 2.0, 0.5 + r.cos() / 2.0);
        end = (0.5 + r.sin() / 2.0, 0.5 - r.cos() / 2.0);
    }
    let positions: Vec<f64> = match property(object, "positions").and_then(Value::as_array) {
        Some(list) => {
            if list.len() != colors.len() {
                return None;
            }
            list.iter()
                .map(|p| {
                    p.as_f64()
                        .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                        .map(|n| n / 100.0)
                })
                .collect::<Option<_>>()?
        }
        None => (0..colors.len())
            .map(|i| i as f64 / (colors.len() - 1) as f64)
            .collect(),
    };
    Some(Brush {
        stops: positions
            .iter()
            .zip(colors)
            .map(|(p, c)| (*p as f32, c))
            .collect(),
        start: (start.0 as f32, start.1 as f32),
        end: (end.0 as f32, end.1 as f32),
    })
}

/// Reads a token value by its JSON shape (`WidgetPack.Convert`, without the existing token's type).
pub fn parse_token(value: &Value) -> Option<Token> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=MAX_DIMENSION).contains(n))
            .map(Token::Number),
        Value::Object(_) => parse_brush(value).map(Token::Brush),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with('#') {
                parse_color(trimmed).map(|c| Token::Brush(Brush::solid(c)))
            } else if let Some(numbers) = parse_numbers(value) {
                Some(Token::Numbers(numbers))
            } else if !trimmed.is_empty() && text.len() <= 128 {
                Some(Token::Text(trimmed.to_owned()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `WidgetPackManifest`: `tokens` for both themes, `dark` and `light` on top for one.
#[derive(Clone, Debug, Default)]
pub struct WidgetPack {
    pub name: Option<String>,
    pub tokens: Map<String, Value>,
    pub dark: Map<String, Value>,
    pub light: Map<String, Value>,
}

impl WidgetPack {
    fn from_value(value: &Value) -> Option<WidgetPack> {
        let object = value.as_object()?;
        let section = |name: &str| {
            property(object, name)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        Some(WidgetPack {
            name: property(object, "name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tokens: section("tokens"),
            dark: section("dark"),
            light: section("light"),
        })
    }

    /// `WidgetPack.Resolve`: the typed values for one theme; unreadable values are reported in `problems`.
    pub fn resolve(&self, dark: bool, problems: &mut Vec<String>) -> HashMap<String, Token> {
        let mut raw: Vec<(&String, &Value)> = self.tokens.iter().collect();
        for (key, value) in if dark { &self.dark } else { &self.light } {
            raw.retain(|(k, _)| *k != key);
            raw.push((key, value));
        }
        let mut resolved = HashMap::new();
        if raw.len() > MAX_TOKENS {
            problems.push("Widget theme contains too many token overrides.".to_owned());
            return resolved;
        }
        for (key, value) in raw {
            match parse_token(value) {
                Some(token) => {
                    resolved.insert(key.clone(), token);
                }
                None => problems.push(format!("Token '{key}' has an unreadable value.")),
            }
        }
        resolved
    }

    /// The raw value of `key` for one theme (theme section first, then `tokens`), ignoring case.
    pub fn raw(&self, key: &str, dark: bool) -> Option<&Value> {
        property(if dark { &self.dark } else { &self.light }, key)
            .or_else(|| property(&self.tokens, key))
    }
}

// ---------- Theme pack ----------

/// A theme folder.
#[derive(Clone, Debug)]
pub struct ThemePack {
    /// The folder name, which settings store.
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub widgets: Option<WidgetPack>,
    /// An icon pack is present and used (`!PreserveOriginalIcons` and not a bundled theme).
    pub has_icons: bool,
}

impl ThemePack {
    pub fn display_name(&self) -> String {
        match &self.manifest.name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => self.id.clone(),
        }
    }

    pub fn is_built_in(&self) -> bool {
        is_built_in(&self.id)
    }

    /// `ThemePack.Load`: a broken widget pack is reported in the problems and left out.
    pub fn load(dir: &Path) -> Result<(ThemePack, Vec<String>), String> {
        let id = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest_path = dir.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(format!("{} not found.", manifest_path.display()));
        }
        let manifest = read_json(&manifest_path, Some(MAX_MANIFEST_BYTES)).and_then(|v| {
            Manifest::from_value(&v).ok_or_else(|| format!("{} is empty.", manifest_path.display()))
        })?;
        let mut problems = Vec::new();
        let has_icons = !manifest.preserve_original_icons
            && !is_built_in(&id)
            && dir.join(ICON_FOLDER).join(PACK_FILE).is_file();
        let pack_path = dir.join(WIDGET_FOLDER).join(PACK_FILE);
        let widgets = if pack_path.is_file() {
            match read_json(&pack_path, Some(MAX_PACK_BYTES)).and_then(|v| {
                WidgetPack::from_value(&v)
                    .ok_or_else(|| format!("{} is empty.", pack_path.display()))
            }) {
                Ok(pack) => Some(pack),
                Err(problem) => {
                    problems.push(problem);
                    None
                }
            }
        } else {
            None
        };
        Ok((
            ThemePack {
                id,
                dir: dir.to_path_buf(),
                manifest,
                widgets,
                has_icons,
            },
            problems,
        ))
    }
}

/// `ThemePacks.Discover`: every loadable theme folder, ordered by display name.
pub fn discover() -> Vec<ThemePack> {
    discover_at(&root())
}

fn discover_at(root: &Path) -> Vec<ThemePack> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packs: Vec<ThemePack> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| ThemePack::load(&e.path()).ok().map(|(pack, _)| pack))
        .collect();
    packs.sort_by_key(|p| p.display_name().to_lowercase());
    packs
}

/// `ThemePacks.Select`: the theme in folder `id`, with its problems logged; `None` for the built-in look.
pub fn select(id: Option<&str>) -> Option<ThemePack> {
    let id = id.map(str::trim).filter(|s| !s.is_empty())?;
    if !is_safe_id(id) {
        crate::log::warn(format!(
            "Theme pack '{id}' could not be loaded: unsafe name."
        ));
        return None;
    }
    match ThemePack::load(&root().join(id)) {
        Ok((pack, problems)) => {
            for problem in problems {
                crate::log::warn(format!("Theme pack '{id}': {problem}"));
            }
            Some(pack)
        }
        Err(error) => {
            crate::log::warn(format!("Theme pack '{id}' could not be loaded: {error}"));
            None
        }
    }
}

// ---------- Transfer ----------

/// `ThemePackTransfer.IsSafeId`.
pub fn is_safe_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id.chars().count() <= 120
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

fn sanitize(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// `ThemePackTransfer.UniqueId`.
pub fn unique_id(root: &Path, name: &str) -> String {
    let base = sanitize(name).unwrap_or_else(|| "theme".to_owned());
    let mut id = base.clone();
    let mut suffix = 2;
    while root.join(&id).exists() {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}

fn copy_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// `ThemePackTransfer.CreateTheme`: a new folder named after `name`, optionally a copy of `based_on`.
pub fn create(name: &str, author: Option<&str>, based_on: Option<&str>) -> std::io::Result<String> {
    create_at(&root(), name, author, based_on)
}

fn create_at(
    root: &Path,
    name: &str,
    author: Option<&str>,
    based_on: Option<&str>,
) -> std::io::Result<String> {
    std::fs::create_dir_all(root)?;
    let id = unique_id(root, name);
    let target = root.join(&id);
    std::fs::create_dir_all(&target)?;
    let mut preserve = false;
    if let Some(source_id) = based_on.filter(|b| is_safe_id(b)) {
        let source = root.join(source_id);
        if source.is_dir() {
            preserve = is_built_in(source_id)
                || ThemePack::load(&source).is_ok_and(|(p, _)| p.manifest.preserve_original_icons);
            copy_dir(&source, &target)?;
        }
    }
    let manifest = Manifest {
        name: Some(if name.trim().is_empty() {
            id.clone()
        } else {
            name.trim().to_owned()
        }),
        author: author
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned),
        description: None,
        preserve_original_icons: preserve,
    };
    std::fs::write(target.join(MANIFEST_FILE), manifest.to_json())?;
    Ok(id)
}

/// `ThemePackTransfer.DeleteTheme`.
pub fn delete(id: &str) -> Result<(), &'static str> {
    if !is_safe_id(id) {
        return Err("ErrorThemeInvalid");
    }
    let path = root().join(id);
    if !path.is_dir() {
        return Err("ErrorThemeMissing");
    }
    std::fs::remove_dir_all(&path).map_err(|error| {
        crate::log::warn(format!("Theme delete failed: {error}"));
        "ErrorThemeInstall"
    })
}

/// `ThemePackTransfer.IsAllowedExtension`.
pub fn is_allowed_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".json", ".png", ".ico", ".jpg", ".jpeg", ".bmp", ".md", ".txt"]
        .iter()
        .any(|ext| lower.ends_with(ext))
        // A bundled theme's `LICENSE` (no extension; OpenTaskbar's importer refuses it).
        || lower.rsplit('/').next() == Some("license")
}

/// The files of theme `id` for an export, as paths relative to the theme folder (`ExportTheme`'s checks).
pub fn export_files(id: &str) -> Result<Vec<(String, Vec<u8>)>, &'static str> {
    if !is_safe_id(id) {
        return Err("ErrorThemeInvalid");
    }
    let source = root().join(id);
    if !source.is_dir() {
        return Err("ErrorThemeMissing");
    }
    let mut files = Vec::new();
    collect_files(&source, "", &mut files).map_err(|_| "ErrorThemeInstall")?;
    let total: usize = files.iter().map(|(_, b)| b.len()).sum();
    if files.is_empty() || files.len() > MAX_FILE_COUNT || total as u64 > MAX_UNCOMPRESSED_BYTES {
        return Err("ErrorThemeInvalid");
    }
    Ok(files)
}

fn collect_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &format!("{name}/"), out)?;
        } else {
            out.push((name, std::fs::read(entry.path())?));
            if out.len() > MAX_FILE_COUNT {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// `ThemePackTransfer.ImportTheme` after the archive is read: `entries` are the zip's files. One theme folder at the
/// root, or a theme laid out at the root, is installed under a unique id; returns that id or an error key.
pub fn import_entries(entries: &[(String, Vec<u8>)]) -> Result<String, &'static str> {
    import_entries_at(&root(), entries)
}

fn import_entries_at(root: &Path, entries: &[(String, Vec<u8>)]) -> Result<String, &'static str> {
    std::fs::create_dir_all(root).map_err(|_| "ErrorThemeInstall")?;
    let staging = root.join(format!(
        ".import-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| {
        extract_safe(entries, &staging)?;
        let theme_dir =
            if staging.join(MANIFEST_FILE).is_file() || staging.join(ICON_FOLDER).is_dir() {
                staging.clone()
            } else {
                std::fs::read_dir(&staging)
                    .map_err(|_| "ErrorThemeInstall")?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .find(|p| p.is_dir() && p.join(MANIFEST_FILE).is_file())
                    .ok_or("ErrorThemeInvalid")?
            };
        if validate(&theme_dir).is_some() {
            return Err("ErrorThemeInvalid");
        }
        let (pack, _) = ThemePack::load(&theme_dir).map_err(|_| "ErrorThemeInvalid")?;
        let id = unique_id(root, &pack.display_name());
        std::fs::rename(&theme_dir, root.join(&id)).map_err(|_| "ErrorThemeInstall")?;
        Ok(id)
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    if let Err(key) = result {
        crate::log::warn(format!("Theme import failed: {key}"));
    }
    result
}

/// `ThemePackTransfer.ExtractSafe`.
fn extract_safe(entries: &[(String, Vec<u8>)], destination: &Path) -> Result<(), &'static str> {
    let mut total = 0u64;
    let mut count = 0;
    for (name, bytes) in entries {
        let relative = name.replace('\\', "/");
        if relative.ends_with('/') {
            continue;
        }
        count += 1;
        total += bytes.len() as u64;
        if count > MAX_FILE_COUNT
            || total > MAX_UNCOMPRESSED_BYTES
            || bytes.len() as u64 > MAX_FILE_BYTES
        {
            return Err("ErrorThemeInvalid");
        }
        if !is_allowed_file(&relative) {
            return Err("ErrorThemeInvalid");
        }
        if relative.starts_with('/')
            || relative.contains(':')
            || relative.contains('\0')
            || relative
                .split('/')
                .any(|p| p == ".." || p == "." || p.is_empty())
        {
            return Err("ErrorThemeInvalid");
        }
        let target = destination.join(relative.replace('/', "\\"));
        // The joined path must stay inside the destination even after Windows' own name rules (trailing dots and
        // spaces, reserved device names).
        if !target.starts_with(destination)
            || target
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("ErrorThemeInvalid");
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "ErrorThemeInstall")?;
        }
        std::fs::write(&target, bytes).map_err(|_| "ErrorThemeInstall")?;
    }
    Ok(())
}

/// `ThemePackTransfer.Validate`: a message when a JSON file does not fit the theme schema.
pub fn validate(dir: &Path) -> Option<String> {
    let manifest = match read_json(&dir.join(MANIFEST_FILE), Some(MAX_MANIFEST_BYTES)) {
        Ok(v) => v,
        Err(_) => return Some("theme.json is missing or not valid JSON.".to_owned()),
    };
    let Some(manifest) = Manifest::from_value(&manifest) else {
        return Some("theme.json is empty.".to_owned());
    };
    if manifest
        .author
        .as_ref()
        .is_some_and(|a| a.chars().count() > 200)
        || manifest
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > 1000)
    {
        return Some("theme.json fields are too long.".to_owned());
    }
    let icons = dir.join(ICON_FOLDER).join(PACK_FILE);
    if icons.is_file() {
        let Ok(value) = read_json(&icons, Some(MAX_MANIFEST_BYTES)) else {
            return Some("icons/pack.json is not valid JSON.".to_owned());
        };
        if let Some(object) = value.as_object() {
            if property(object, "icons")
                .and_then(Value::as_object)
                .is_some_and(|m| m.len() > MAX_FILE_COUNT)
            {
                return Some("icons/pack.json has too many entries.".to_owned());
            }
            if let Some(scale) = property(object, "fallback")
                .and_then(Value::as_object)
                .and_then(|f| property(f, "iconScale"))
                .and_then(Value::as_f64)
                && !(0.1..=2.0).contains(&scale)
            {
                return Some("icons/pack.json has an out-of-range icon scale.".to_owned());
            }
        }
    }
    let widgets = dir.join(WIDGET_FOLDER).join(PACK_FILE);
    if widgets.is_file() {
        let Some(pack) = read_json(&widgets, Some(MAX_PACK_BYTES))
            .ok()
            .as_ref()
            .and_then(WidgetPack::from_value)
        else {
            return Some("widgets/pack.json is not valid JSON.".to_owned());
        };
        if [&pack.tokens, &pack.dark, &pack.light]
            .iter()
            .any(|m| m.len() > MAX_FILE_COUNT)
        {
            return Some("widgets/pack.json has too many tokens.".to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ot-theme-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lenient_json() {
        let v = parse_lenient(
            "\u{feff}{ // c\n \"a\": \"x//y\", /* b */ \"b\": [1, 2,], \"c\": { \"d\": \"/*\", },\n}",
        )
        .unwrap();
        assert_eq!(v["a"], "x//y");
        assert_eq!(v["b"], json!([1, 2]));
        assert_eq!(v["c"]["d"], "/*");
    }

    #[test]
    fn colors_and_tokens() {
        assert_eq!(parse_color("#FF1FC2F0"), Some(0xFF1FC2F0));
        assert_eq!(parse_color("#1FC2F0"), Some(0xFF1FC2F0));
        assert_eq!(parse_color("#ABC"), Some(0xFFAABBCC));
        assert_eq!(parse_color("red"), None);
        assert_eq!(parse_token(&json!(16)), Some(Token::Number(16.0)));
        assert_eq!(
            parse_token(&json!("21,21,0,0")),
            Some(Token::Numbers([21.0, 21.0, 0.0, 0.0]))
        );
        assert_eq!(
            parse_token(&json!("Segoe UI Variable Display")),
            Some(Token::Text("Segoe UI Variable Display".into()))
        );
        let Some(Token::Brush(b)) =
            parse_token(&json!({ "stops": ["#FF000000", "#FFFFFFFF", "#FF808080"] }))
        else {
            panic!()
        };
        assert_eq!(
            b.stops,
            vec![(0.0, 0xFF000000), (0.5, 0xFFFFFFFF), (1.0, 0xFF808080)]
        );
        assert_eq!((b.start, b.end), ((0.0, 0.0), (0.0, 1.0)));
        let Some(Token::Brush(d)) =
            parse_token(&json!({ "stops": ["#FF000000", "#FFFFFFFF"], "direction": "diagonal" }))
        else {
            panic!()
        };
        assert_eq!(d.end, (1.0, 1.0));
        assert_eq!(parse_token(&json!({ "stops": ["#FF000000"] })), None);
    }

    #[test]
    fn bundled_themes_install_and_resolve() {
        let root = temp("bundled");
        install_built_in_at(&root).unwrap();
        // A user's edit survives a second install.
        std::fs::write(
            root.join("nord").join(MANIFEST_FILE),
            "{ \"name\": \"Mine\" }",
        )
        .unwrap();
        install_built_in_at(&root).unwrap();
        let packs = discover_at(&root);
        assert_eq!(packs.len(), 6);
        assert!(packs.iter().any(|p| p.display_name() == "Mine"));
        let catppuccin = packs.iter().find(|p| p.id == "catppuccin").unwrap();
        assert!(catppuccin.is_built_in() && !catppuccin.has_icons);
        let mut problems = Vec::new();
        let dark = catppuccin
            .widgets
            .as_ref()
            .unwrap()
            .resolve(true, &mut problems);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(matches!(dark.get("DockMaterial"), Some(Token::Brush(b)) if b.stops.len() >= 2));
        assert!(matches!(dark.get("WidgetRadius"), Some(Token::Number(_))));
        for pack in &packs {
            assert!(validate(&pack.dir).is_none(), "{}", pack.id);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_duplicate_and_import() {
        let root = temp("transfer");
        install_built_in_at(&root).unwrap();
        let id = create_at(&root, "My theme", None, None).unwrap();
        assert_eq!(id, "My-theme");
        assert_eq!(
            create_at(&root, "My theme", None, None).unwrap(),
            "My-theme-2"
        );
        let copy = create_at(&root, "Nord copy", None, Some("nord")).unwrap();
        let (pack, _) = ThemePack::load(&root.join(&copy)).unwrap();
        assert_eq!(pack.display_name(), "Nord copy");
        assert!(pack.manifest.preserve_original_icons && pack.widgets.is_some());
        // An archive with one theme folder, and one with an unsafe path.
        let manifest = br#"{ "name": "Imported" }"#.to_vec();
        let id = import_entries_at(&root, &[("x/theme.json".into(), manifest.clone())]).unwrap();
        assert_eq!(id, "Imported");
        assert_eq!(
            import_entries_at(&root, &[("../evil.json".into(), manifest.clone())]),
            Err("ErrorThemeInvalid")
        );
        assert_eq!(
            import_entries_at(&root, &[("x/run.exe".into(), manifest)]),
            Err("ErrorThemeInvalid")
        );
        assert!(!is_safe_id("..") && !is_safe_id("a/b") && is_safe_id("tokyo-night"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
