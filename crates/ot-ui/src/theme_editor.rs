//! `ThemePropertyEditor`: the token editor for custom theme packs. It reads and writes the real `theme.json` and
//! `widgets\pack.json`; edits go to the section of the current color mode (`dark` or `light`), as OpenTaskbar does.
//! Built-in themes are read-only (duplicate them first).

use std::path::Path;

use ot_core::language::{AppLanguage, Catalog};
use ot_core::theme_pack::{self, Brush, ThemePack, Token};
use ot_dock::token_defaults::{DEFAULTS, Kind};
use serde_json::{Map, Value, json};

/// `ThemePropertyEditor.Sections`: (title key, tokens).
const SECTIONS: &[(&str, &[&str])] = &[
    (
        "ThemeSectionColors",
        &[
            "TextPrimary",
            "TextSecondary",
            "AccentBrush",
            "AccentText",
            "StrokeSubtle",
            "StrokeFocus",
            "SurfaceSelected",
            "SurfaceHover",
            "SurfacePressed",
        ],
    ),
    (
        "ThemeSectionDock",
        &[
            "DockBackgroundMode",
            "DockMaterial",
            "DockBorderMaterial",
            "DockHighlight",
            "DockRadius",
            "DockInnerRadius",
            "DockWidgetPadding",
            "DockShadowBlur",
            "DockShadowOpacity",
            "DockShadowColor",
        ],
    ),
    (
        "ThemeSectionWidgets",
        &[
            "WidgetCard",
            "WidgetCardHover",
            "WidgetCardPressed",
            "WidgetCardStroke",
            "WidgetTrack",
            "WidgetAccent",
            "DockWidgetSurface",
            "DockWidgetRadius",
        ],
    ),
    (
        "ThemeSectionWindows",
        &[
            "MenuMaterial",
            "MenuDivider",
            "SurfacePopup",
            "MenuLabelWidth",
            "PopupRadius",
            "PopupInnerRadius",
            "SettingsBackground",
            "SettingsBackgroundOpacity",
            "SettingsControl",
            "SettingsControlHover",
            "SettingsSidebarOverlay",
        ],
    ),
    (
        "ThemeSectionIcons",
        &[
            "IconForeground",
            "WindowsLogoBrush",
            "IconHoverGlow",
            "StartTileBackground",
            "StartTileStroke",
            "StartTileHover",
            "WidgetAccent",
            "WidgetWarning",
            "WidgetFocusAccent",
        ],
    ),
    (
        "ThemeSectionTypography",
        &[
            "InterfaceFont",
            "TextSizeBody",
            "TextSizeCaption",
            "ControlRadius",
            "SettingsPageInset",
        ],
    ),
];

fn default_of(key: &str) -> Option<(Kind, &'static str, &'static str)> {
    DEFAULTS
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|&(_, kind, dark, light)| (kind, dark, light))
}

/// The value a token has now without the pack: the Windows-derived colors where OpenTaskbar's `WindowsPalette` sets
/// them, else the palette's.
fn runtime_default(key: &str, dark: bool) -> Option<Token> {
    let solid = |c: ot_render::Color| {
        Token::Brush(Brush::solid(
            (u32::from(c.A) << 24)
                | (u32::from(c.R) << 16)
                | (u32::from(c.G) << 8)
                | u32::from(c.B),
        ))
    };
    match key {
        "AccentBrush" => Some(solid(ot_dock::theme::accent(dark))),
        "StrokeFocus" => ot_dock::theme::stroke_focus(dark).map(solid),
        "SurfaceHover" => Some(solid(ot_dock::theme::surface_hover(dark))),
        "ShellButtonHover" => Some(solid(ot_dock::theme::shell_button_colors(dark).0)),
        "ShellButtonPressed" => Some(solid(ot_dock::theme::shell_button_colors(dark).1)),
        "SurfaceKeyboardFocus" => Some(solid(ot_dock::theme::keyboard_focus_color(dark))),
        _ => None,
    }
}

fn kind_matches(kind: Kind, token: &Token) -> bool {
    matches!(
        (kind, token),
        (Kind::Brush, Token::Brush(_))
            | (Kind::Numbers, Token::Numbers(_) | Token::Number(_))
            | (Kind::Number, Token::Number(_))
            | (Kind::Font, Token::Text(_))
            | (Kind::Mode, Token::Text(_))
    )
}

/// A brush as CSS for the page.
pub fn brush_css(brush: &Brush) -> String {
    let rgba = |c: u32| {
        format!(
            "rgba({}, {}, {}, {:.4})",
            (c >> 16) & 0xFF,
            (c >> 8) & 0xFF,
            c & 0xFF,
            f64::from(c >> 24) / 255.0
        )
    };
    if brush.is_solid() {
        return rgba(brush.first());
    }
    let (dx, dy) = (
        f64::from(brush.end.0 - brush.start.0),
        f64::from(brush.end.1 - brush.start.1),
    );
    let angle = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
    let stops: Vec<String> = brush
        .stops
        .iter()
        .map(|(o, c)| format!("{} {:.1}%", rgba(*c), f64::from(*o) * 100.0))
        .collect();
    format!("linear-gradient({angle:.1}deg, {})", stops.join(", "))
}

/// A brush in the widget pack's syntax (`BrushToNode`): "#AARRGGBB", or stops, positions and angle.
pub fn brush_value(brush: &Brush) -> Value {
    if brush.is_solid() {
        return Value::String(format!("#{:08X}", brush.first()));
    }
    let (dx, dy) = (
        f64::from(brush.end.0 - brush.start.0),
        f64::from(brush.end.1 - brush.start.1),
    );
    let angle = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
    json!({
        "stops": brush.stops.iter().map(|(_, c)| format!("#{c:08X}")).collect::<Vec<_>>(),
        "positions": brush.stops.iter().map(|(o, _)| (f64::from(*o) * 100.0 * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
        "angle": (angle * 1000.0).round() / 1000.0,
    })
}

fn number_text(n: f64) -> String {
    let rounded = (n * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
}

/// The token's field for the page: its value now (the pack's, else the default) in the shape the control needs.
fn field(
    pack: Option<&theme_pack::WidgetPack>,
    key: &str,
    dark: bool,
    language: &AppLanguage,
) -> Option<Value> {
    let (kind, dark_default, light_default) = default_of(key)?;
    let from_pack = pack
        .and_then(|p| p.raw(key, dark))
        .and_then(theme_pack::parse_token)
        .filter(|t| kind_matches(kind, t));
    let default = || {
        runtime_default(key, dark).or_else(|| {
            let text = if dark { dark_default } else { light_default };
            let value = serde_json::from_str::<Value>(text)
                .unwrap_or_else(|_| Value::String(text.to_owned()));
            theme_pack::parse_token(&value)
        })
    };
    let token = from_pack.or_else(default)?;
    let field_key = format!("ThemeField{key}");
    let localized = Catalog::get(&field_key, language).into_owned();
    let label = if localized == field_key || localized.is_empty() {
        pretty_name(key)
    } else {
        localized
    };
    let mut out = json!({ "key": key, "label": label });
    match (kind, token) {
        (Kind::Brush, Token::Brush(brush)) => {
            out["kind"] = json!("brush");
            out["css"] = json!(brush_css(&brush));
            out["gradient"] = json!(!brush.is_solid());
            out["value"] = brush_value(&brush);
        }
        (Kind::Numbers, Token::Numbers(n)) => {
            out["kind"] = json!("numbers");
            out["text"] = json!(if n.iter().all(|v| (v - n[0]).abs() < 0.01) {
                number_text(n[0])
            } else {
                n.iter()
                    .map(|v| number_text(*v))
                    .collect::<Vec<_>>()
                    .join(",")
            });
        }
        (Kind::Numbers, Token::Number(v)) => {
            out["kind"] = json!("numbers");
            out["text"] = json!(number_text(v));
        }
        (Kind::Number, Token::Number(v)) => {
            out["kind"] = json!("number");
            out["text"] = json!(number_text(v));
        }
        (Kind::Font | Kind::Mode, Token::Text(t)) => {
            out["kind"] = json!(if kind == Kind::Font { "font" } else { "mode" });
            out["text"] = json!(t.rsplit('#').next().unwrap_or(&t));
        }
        _ => return None,
    }
    Some(out)
}

/// `PrettyName` fallback: "WidgetCardHover" → "Widget Card Hover".
fn pretty_name(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out.replace(" Ui ", " UI ").replace(" Ai ", " AI ")
}

/// The editor for a custom theme: its information and the token tabs (`LoadTheme`).
pub fn payload(pack: &ThemePack, dark: bool, language: &AppLanguage) -> Value {
    let widgets = pack.widgets.as_ref();
    let mut covered: Vec<&str> = Vec::new();
    let mut sections = Vec::new();
    for (title, keys) in SECTIONS {
        covered.extend(keys.iter().copied());
        let fields: Vec<Value> = keys
            .iter()
            .filter_map(|k| field(widgets, k, dark, language))
            .collect();
        sections.push(json!({ "title": Catalog::get(title, language), "fields": fields }));
    }
    // "Advanced": the remaining editable tokens, by name.
    let mut advanced: Vec<&str> = DEFAULTS
        .iter()
        .map(|(k, ..)| *k)
        .filter(|k| !covered.contains(k))
        .collect();
    advanced.sort_by_key(|k| k.to_lowercase());
    advanced.dedup();
    let fields: Vec<Value> = advanced
        .iter()
        .filter_map(|k| field(widgets, k, dark, language))
        .collect();
    sections
        .push(json!({ "title": Catalog::get("ThemeSectionAdvanced", language), "fields": fields }));
    json!({
        "id": pack.id,
        "name": pack.manifest.name.clone().unwrap_or_else(|| pack.display_name()),
        "author": pack.manifest.author.clone().unwrap_or_default(),
        "description": pack.manifest.description.clone().unwrap_or_default(),
        "sections": sections,
    })
}

fn read_object(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| theme_pack::parse_lenient(&t))
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Sets `name` in `object`, reusing an existing key that differs only in case.
fn set_property(object: &mut Map<String, Value>, name: &str, value: Value) {
    let key = object
        .keys()
        .find(|k| k.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or_else(|| name.to_owned());
    object.insert(key, value);
}

/// `AtomicWrite`.
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let temporary = path.with_extension(format!(
        "{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result =
        std::fs::write(&temporary, contents).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn editable(id: &str) -> Result<ThemePack, String> {
    if !theme_pack::is_safe_id(id) || theme_pack::is_built_in(id) {
        return Err("built-in or unsafe theme".to_owned());
    }
    ThemePack::load(&theme_pack::root().join(id)).map(|(p, _)| p)
}

/// `UpdateToken` + `SaveTheme`: writes `value` for `key` into the current mode's section.
pub fn save_token(id: &str, dark: bool, key: &str, value: &Value) -> Result<(), String> {
    let pack = editable(id)?;
    let (kind, ..) = default_of(key).ok_or_else(|| format!("unknown token {key}"))?;
    let token =
        theme_pack::parse_token(value).ok_or_else(|| format!("unreadable value for {key}"))?;
    if !kind_matches(kind, &token) {
        return Err(format!("wrong value type for {key}"));
    }
    let directory = pack.dir.join(theme_pack::WIDGET_FOLDER);
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(theme_pack::PACK_FILE);
    let mut root = read_object(&path);
    if root.is_empty() {
        root.insert(
            "name".to_owned(),
            json!(format!("{} Widgets", pack.display_name())),
        );
    }
    let section_name = if dark { "dark" } else { "light" };
    let section_key = root
        .keys()
        .find(|k| k.eq_ignore_ascii_case(section_name))
        .cloned()
        .unwrap_or_else(|| section_name.to_owned());
    let mut section = root
        .get(&section_key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    set_property(&mut section, key, value.clone());
    root.insert(section_key, Value::Object(section));
    atomic_write(
        &path,
        &serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())
}

/// `SaveTheme`'s manifest part: name (1–120 characters), author and description.
pub fn save_info(
    id: &str,
    name: &str,
    author: &str,
    description: &str,
) -> Result<(), &'static str> {
    let pack = editable(id).map_err(|_| "ErrorThemeMissing")?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err("ThemeNameInvalid");
    }
    let path = pack.dir.join(theme_pack::MANIFEST_FILE);
    let mut manifest = read_object(&path);
    set_property(&mut manifest, "Name", json!(name));
    set_property(&mut manifest, "Author", json!(author.trim()));
    set_property(&mut manifest, "Description", json!(description.trim()));
    atomic_write(
        &path,
        &serde_json::to_string_pretty(&Value::Object(manifest)).unwrap_or_default(),
    )
    .map_err(|_| "ErrorThemeInstall")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brush_values_round_trip() {
        let gradient = theme_pack::parse_token(&json!({
            "stops": ["#FF000000", "#80FFFFFF"], "positions": [0, 100], "angle": 90
        }));
        let Some(Token::Brush(b)) = gradient else {
            panic!()
        };
        let value = brush_value(&b);
        assert_eq!(value["stops"], json!(["#FF000000", "#80FFFFFF"]));
        assert_eq!(value["angle"], json!(90.0));
        assert_eq!(brush_value(&Brush::solid(0xFF123456)), json!("#FF123456"));
        assert_eq!(pretty_name("WidgetCardHover"), "Widget Card Hover");
        assert_eq!(number_text(9.0), "9");
    }

    #[test]
    fn every_default_parses() {
        for (key, kind, dark, light) in DEFAULTS {
            for text in [dark, light] {
                let value = serde_json::from_str::<Value>(text)
                    .unwrap_or_else(|_| Value::String((*text).to_owned()));
                let token = theme_pack::parse_token(&value);
                assert!(
                    token.as_ref().is_some_and(|t| kind_matches(*kind, t)),
                    "{key}: {text}"
                );
            }
        }
    }
}
