//! Dock colors (`Design/Palettes/Dark.xaml`, `Light.xaml`, `Design/Tokens/Effects.xaml` and `ThemeManager` in
//! OpenTaskbar). Glass is the palette's translucent gradient with its alpha replaced by the user's opacity, exactly
//! as `ThemeManager.Fill` does; there is no backdrop blur in OpenTaskbar, so there is none here either.

use ot_core::settings::{AppTheme, SurfaceBackground};
use ot_render::{Color, color};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Dark,
    Light,
}

/// Resolves System to the Windows app theme.
pub fn tone(theme: AppTheme) -> Tone {
    match theme {
        AppTheme::Dark => Tone::Dark,
        AppTheme::Light => Tone::Light,
        AppTheme::System => {
            if windows_uses_light_theme() {
                Tone::Light
            } else {
                Tone::Dark
            }
        }
    }
}

fn windows_uses_light_theme() -> bool {
    let key: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme".encode_utf16().chain(Some(0)).collect();
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: a DWORD read into a 4-byte buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data != 0
}

/// Gradient stops (offset, color).
pub type Gradient = Vec<(f32, Color)>;

pub struct DockColors {
    /// `None` for SurfaceBackground::None (no fill, rim or shadow).
    pub fill: Option<Gradient>,
    pub rim: Option<Gradient>,
    pub shadow: bool,
    pub indicator: Color,
    pub indicator_foreground: Color,
    pub glyph: Color,
    pub separator: Color,
}

/// `WindowsPalette`: OpenTaskbar lays brushes derived from the Windows personalization colors over its palettes
/// whenever Windows exposes an accent color (almost always). These are the values the user actually sees.
pub fn windows_palette() -> bool {
    windows_accent().is_some()
}

/// `AccentBrush`: the Windows accent, else the palette's own.
pub fn accent(dark: bool) -> Color {
    if let Some(themed) = tok("AccentBrush", dark) {
        return themed;
    }
    windows_accent().unwrap_or(if dark {
        color(0xFF, 0x38, 0xBD, 0xF8)
    } else {
        color(0xFF, 0x03, 0x69, 0xA1)
    })
}

/// `SurfaceHover` (menu items, flyout rows).
pub fn surface_hover(dark: bool) -> Color {
    if let Some(themed) = tok("SurfaceHover", dark) {
        return themed;
    }
    match (windows_palette(), dark) {
        (true, true) => color(0x14, 0xFF, 0xFF, 0xFF),
        (true, false) => color(0x0D, 0x00, 0x00, 0x00),
        (false, true) => color(0x18, 0xFF, 0xFF, 0xFF),
        (false, false) => color(0x12, 0x16, 0x20, 0x38),
    }
}

/// A theme pack's color for `key`, if the active pack sets it.
fn tok(key: &str, dark: bool) -> Option<Color> {
    crate::tokens::color_of(key, dark)
}

/// `ThemeManager.Fill`: the source's stops with their alpha replaced.
fn with_alpha(gradient: Gradient, alpha: u8) -> Gradient {
    gradient
        .into_iter()
        .map(|(o, c)| (o, color(alpha, c.R, c.G, c.B)))
        .collect()
}

/// `TextPrimary`.
pub fn text_primary(dark: bool) -> Color {
    tok("TextPrimary", dark).unwrap_or(if dark {
        color(0xFF, 0xF3, 0xF4, 0xF6)
    } else {
        color(0xFF, 0x1B, 0x1B, 0x1B)
    })
}

/// `TextSecondary`.
pub fn text_secondary(dark: bool) -> Color {
    tok("TextSecondary", dark).unwrap_or(if dark {
        color(0xFF, 0xAA, 0xB2, 0xC0)
    } else {
        color(0xFF, 0x61, 0x66, 0x6E)
    })
}

/// `StrokeSubtle`.
pub fn stroke_subtle(dark: bool) -> Color {
    tok("StrokeSubtle", dark).unwrap_or(if dark {
        color(0xFF, 0x3A, 0x43, 0x52)
    } else {
        color(0xFF, 0xE5, 0xE5, 0xE5)
    })
}

/// `SurfacePopup`.
pub fn surface_popup(dark: bool) -> Color {
    tok("SurfacePopup", dark).unwrap_or(if dark {
        color(0xF7, 0x25, 0x2B, 0x37)
    } else {
        color(0xF7, 0xFF, 0xFF, 0xFF)
    })
}

/// `StrokeFocus`: the Windows accent (`WindowsPalette`) unless a theme pack sets it.
pub fn stroke_focus(dark: bool) -> Option<Color> {
    tok("StrokeFocus", dark).or_else(windows_accent)
}

pub fn dock_colors(tone: Tone, background: SurfaceBackground, opacity: f64) -> DockColors {
    let dark = tone == Tone::Dark;
    let alpha = match background {
        SurfaceBackground::Solid => 255,
        _ => (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    };
    let (fill, rim, indicator, indicator_foreground, glyph) = match tone {
        Tone::Dark => (
            vec![
                (0.0, color(alpha, 0x2B, 0x31, 0x3E)),
                (1.0, color(alpha, 0x1B, 0x20, 0x29)),
            ],
            vec![
                (0.0, color(0x38, 0xFF, 0xFF, 0xFF)),
                (0.30, color(0x18, 0xFF, 0xFF, 0xFF)),
                (0.70, color(0x06, 0xFF, 0xFF, 0xFF)),
                (1.0, color(0x14, 0x00, 0x00, 0x00)),
            ],
            // Running indicator: white with a black outline for both states (the user's choice over OpenTaskbar's
            // 40 % accent dot, which was hard to see); the app in front has the larger dot.
            color(0xFF, 0xFF, 0xFF, 0xFF),
            color(0xFF, 0xFF, 0xFF, 0xFF),
            color(0xF0, 0xF1, 0xF5, 0xF9),
        ),
        Tone::Light => (
            vec![
                (0.0, color(alpha, 0xFF, 0xFF, 0xFF)),
                (1.0, color(alpha, 0xF3, 0xF3, 0xF3)),
            ],
            vec![
                (0.0, color(0x60, 0xFF, 0xFF, 0xFF)),
                (0.30, color(0x25, 0xFF, 0xFF, 0xFF)),
                (0.70, color(0x15, 0x00, 0x00, 0x00)),
                (1.0, color(0x25, 0x00, 0x00, 0x00)),
            ],
            // Running indicator: white with a black outline for both states (the user's choice over OpenTaskbar's
            // 40 % accent dot, which was hard to see); the app in front has the larger dot.
            color(0xFF, 0xFF, 0xFF, 0xFF),
            color(0xFF, 0xFF, 0xFF, 0xFF),
            color(0xE6, 0x0F, 0x17, 0x2A),
        ),
    };
    // A theme pack's `DockMaterial` (alpha from the user's surface opacity), `DockBorderMaterial` and indicators.
    let fill = crate::tokens::gradient("DockMaterial", dark)
        .map(|g| with_alpha(g, alpha))
        .unwrap_or(fill);
    let rim = crate::tokens::gradient("DockBorderMaterial", dark).unwrap_or(rim);
    let none = background == SurfaceBackground::None;
    DockColors {
        fill: (!none).then_some(fill),
        rim: (!none).then_some(rim),
        shadow: !none,
        indicator: tok("IndicatorBrush", dark).unwrap_or(indicator),
        indicator_foreground: tok("IndicatorForegroundBrush", dark).unwrap_or(indicator_foreground),
        glyph: tok("IconForeground", dark).unwrap_or(glyph),
        // `StartDivider` is filled with `TextSecondary`.
        separator: text_secondary(dark),
    }
}

/// `ShadowDock`: BlurRadius 36, ShadowDepth 4 (downwards), black, opacity 0.28.
pub const SHADOW_BLUR_DIPS: f32 = 36.0;
pub const SHADOW_DEPTH_DIPS: f32 = 4.0;
pub const SHADOW_OPACITY: f32 = 0.28;

/// Context menu colors (`MenuMaterial`, `MenuStroke`, `DockInnerBevelMaterial`, text, divider and hover brushes of
/// the palettes, with `ThemeManager`'s Fill/Stroke rules and the user's border color).
#[derive(Clone)]
pub struct MenuColors {
    pub fill: Option<Gradient>,
    pub stroke: Option<Gradient>,
    pub stroke_thickness: f32,
    pub bevel: Gradient,
    pub text: Color,
    pub text_secondary: Color,
    pub danger: Color,
    pub divider: Color,
    pub hover: Color,
}

pub fn menu_colors(
    tone: Tone,
    background: SurfaceBackground,
    opacity: f64,
    border_thickness: f64,
    border_color: Option<&str>,
) -> MenuColors {
    let alpha = match background {
        SurfaceBackground::Solid => 255,
        _ => (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    };
    let dark = tone == Tone::Dark;
    let fill: Gradient = if windows_palette() {
        // `WindowsPalette`'s MenuMaterial: two stops.
        if dark {
            vec![
                (0.0, color(alpha, 0x2A, 0x30, 0x3C)),
                (1.0, color(alpha, 0x1F, 0x25, 0x30)),
            ]
        } else {
            vec![
                (0.0, color(alpha, 0xFF, 0xFF, 0xFF)),
                (1.0, color(alpha, 0xF7, 0xF7, 0xF7)),
            ]
        }
    } else if dark {
        vec![
            (0.0, color(alpha, 0x2A, 0x30, 0x3C)),
            (0.5, color(alpha, 0x25, 0x2B, 0x37)),
            (1.0, color(alpha, 0x1F, 0x25, 0x30)),
        ]
    } else {
        vec![
            (0.0, color(alpha, 0xFF, 0xFF, 0xFF)),
            (0.5, color(alpha, 0xFF, 0xFF, 0xFF)),
            (1.0, color(alpha, 0xF7, 0xF7, 0xF7)),
        ]
    };
    let stroke: Gradient = if dark {
        vec![
            (0.0, color(0x70, 0xFF, 0xFF, 0xFF)),
            (0.35, color(0x25, 0xFF, 0xFF, 0xFF)),
            (0.75, color(0x10, 0xFF, 0xFF, 0xFF)),
            (1.0, color(0x4A, 0x0B, 0x12, 0x20)),
        ]
    } else {
        vec![
            (0.0, color(0x85, 0xFF, 0xFF, 0xFF)),
            (0.35, color(0x30, 0x00, 0x00, 0x00)),
            (0.75, color(0x20, 0x00, 0x00, 0x00)),
            (1.0, color(0x40, 0x0F, 0x17, 0x2A)),
        ]
    };
    let bevel: Gradient = if dark {
        vec![
            (0.0, color(0x70, 0xFF, 0xFF, 0xFF)),
            (0.22, color(0x22, 0xFF, 0xFF, 0xFF)),
            (0.6, color(0x00, 0x00, 0x00, 0x00)),
            (1.0, color(0x18, 0x00, 0x00, 0x00)),
        ]
    } else {
        vec![
            (0.0, color(0x85, 0xFF, 0xFF, 0xFF)),
            (0.3, color(0x25, 0xFF, 0xFF, 0xFF)),
            (0.65, color(0x00, 0x00, 0x00, 0x00)),
            (1.0, color(0x14, 0x00, 0x00, 0x00)),
        ]
    };
    let fill = crate::tokens::gradient("MenuMaterial", dark)
        .map(|g| with_alpha(g, alpha))
        .unwrap_or(fill);
    let stroke = crate::tokens::gradient("MenuStroke", dark).unwrap_or(stroke);
    let bevel = crate::tokens::gradient("DockInnerBevelMaterial", dark).unwrap_or(bevel);
    let none = background == SurfaceBackground::None;
    let stroke = if none {
        None
    } else if let Some(user) = border_color.and_then(parse_color) {
        Some(vec![(0.0, user), (1.0, user)])
    } else {
        Some(stroke)
    };
    MenuColors {
        fill: (!none).then_some(fill),
        stroke,
        stroke_thickness: border_thickness.clamp(0.0, 12.0) as f32,
        bevel,
        text: text_primary(dark),
        text_secondary: text_secondary(dark),
        danger: tok("DangerText", dark).unwrap_or(if dark {
            color(0xFF, 0xFF, 0x9D, 0xA7)
        } else {
            color(0xFF, 0xAF, 0x23, 0x38)
        }),
        divider: tok("MenuDivider", dark).unwrap_or(if dark {
            color(0xFF, 0x32, 0x3A, 0x48)
        } else {
            color(0xFF, 0xE8, 0xE8, 0xE8)
        }),
        hover: surface_hover(dark),
    }
}

/// "#RRGGBB" or "#AARRGGBB" (WPF color syntax).
pub fn parse_color(text: &str) -> Option<Color> {
    let hex = text.trim().strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(color(
            0xFF,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        )),
        8 => Some(color(
            (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        )),
        _ => None,
    }
}

/// Reads a registry value of the current user into `buffer`; returns the bytes read.
fn user_value(key: &str, name: &str, flags: u32, buffer: &mut [u8]) -> Option<usize> {
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut size = buffer.len() as u32;
    // SAFETY: the buffer and its size are passed together.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            flags,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then_some(size as usize)
}

/// The Windows accent color as `WindowsPalette.Read` finds it: the fourth stop of `Explorer\Accent\AccentPalette`,
/// else DWM's `AccentColor` (ABGR), else `ColorizationColor` (ARGB). OpenTaskbar uses it opaque for `StrokeFocus`.
pub fn windows_accent() -> Option<Color> {
    let mut palette = [0u8; 64];
    if let Some(read) = user_value(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentPalette",
        RRF_RT_REG_BINARY,
        &mut palette,
    ) && read >= 28
    {
        return Some(color(0xFF, palette[12], palette[13], palette[14]));
    }
    let mut dword = [0u8; 4];
    if user_value(
        r"Software\Microsoft\Windows\DWM",
        "AccentColor",
        RRF_RT_REG_DWORD,
        &mut dword,
    )
    .is_some()
    {
        let v = u32::from_le_bytes(dword);
        return Some(color(0xFF, v as u8, (v >> 8) as u8, (v >> 16) as u8));
    }
    if user_value(
        r"Software\Microsoft\Windows\DWM",
        "ColorizationColor",
        RRF_RT_REG_DWORD,
        &mut dword,
    )
    .is_some()
    {
        let v = u32::from_le_bytes(dword);
        return Some(color(0xFF, (v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    None
}

/// The "Background apps" flyout's colors: the menu material and stroke, the `DockHighlight` sheen and the widget
/// button states (`SurfaceHover`/`SurfacePressed`, `WidgetCardHover`/`WidgetCardPressed` scaled by the widget
/// opacity in Glass mode, `WidgetSelectedFill`, `AccentBrush`, the `WidgetDanger*` brushes).
pub fn flyout_colors(settings: &ot_core::settings::DockSettings) -> crate::tray_flyout::Colors {
    let tone = tone(settings.theme);
    let dark = tone == Tone::Dark;
    let menu = menu_colors(
        tone,
        settings.menu_background,
        settings.menu_opacity,
        settings.menu_border_thickness,
        settings.menu_border_color.as_deref(),
    );
    let sheen = (settings.menu_background != SurfaceBackground::None).then(|| {
        if let Some(themed) = crate::tokens::gradient("DockHighlight", dark) {
            return themed;
        }
        if dark {
            vec![
                (0.0, color(0x40, 0xFF, 0xFF, 0xFF)),
                (0.08, color(0x1A, 0xFF, 0xFF, 0xFF)),
                (0.22, color(0x06, 0xFF, 0xFF, 0xFF)),
                (0.45, color(0x00, 0xFF, 0xFF, 0xFF)),
                (1.0, color(0x00, 0xFF, 0xFF, 0xFF)),
            ]
        } else {
            vec![
                (0.0, color(0x45, 0xFF, 0xFF, 0xFF)),
                (0.5, color(0x15, 0xFF, 0xFF, 0xFF)),
                (1.0, color(0x00, 0xFF, 0xFF, 0xFF)),
            ]
        }
    });
    let widget_scale = if settings.widget_background == SurfaceBackground::Glass {
        settings.widget_opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let scaled = |c: Color| color((f64::from(c.A) * widget_scale).round() as u8, c.R, c.G, c.B);
    let accent = tok("AccentBrush", dark).or_else(windows_accent);
    let mut colors = if dark {
        crate::tray_flyout::Colors {
            menu,
            sheen,
            hover: surface_hover(true),
            pressed: color(0x22, 0xFF, 0xFF, 0xFF),
            widget_hover: scaled(color(0x22, 0xFF, 0xFF, 0xFF)),
            widget_pressed: scaled(color(0x38, 0xFF, 0xFF, 0xFF)),
            selected: color(0x33, 0x5B, 0x92, 0xFF),
            accent: accent.unwrap_or(color(0xFF, 0x38, 0xBD, 0xF8)),
            danger_hover: color(0x22, 0xDF, 0x4B, 0x53),
            danger_stroke: color(0x42, 0xDF, 0x4B, 0x53),
            danger_text: color(0xFF, 0xF3, 0x6C, 0x74),
            dark,
        }
    } else {
        crate::tray_flyout::Colors {
            menu,
            sheen,
            hover: surface_hover(false),
            pressed: color(0x16, 0x00, 0x00, 0x00),
            widget_hover: scaled(color(0x18, 0x00, 0x00, 0x00)),
            widget_pressed: scaled(color(0x2A, 0x00, 0x00, 0x00)),
            selected: color(0x26, 0x24, 0x74, 0xE9),
            accent: accent.unwrap_or(color(0xFF, 0x03, 0x69, 0xA1)),
            danger_hover: color(0x1F, 0xDF, 0x4B, 0x53),
            danger_stroke: color(0x3A, 0xDF, 0x4B, 0x53),
            danger_text: color(0xFF, 0xDC, 0x26, 0x26),
            dark,
        }
    };
    // `ResolveWidgetHoverSource` / `ResolveWidgetPressedSource`: a pack's card states, else its surface states.
    if let Some(hover) = tok("WidgetCardHover", dark).or_else(|| tok("SurfaceHover", dark)) {
        colors.widget_hover = scaled(hover);
    }
    let pressed = tok("WidgetCardPressed", dark)
        .or_else(|| tok("SurfacePressed", dark))
        .or_else(|| {
            tok("WidgetCardHover", dark).map(|c| {
                color(
                    (f64::from(c.A) * 1.5).round().min(255.0) as u8,
                    c.R,
                    c.G,
                    c.B,
                )
            })
        });
    if let Some(pressed) = pressed {
        colors.widget_pressed = scaled(pressed);
    }
    if let Some(c) = tok("SurfacePressed", dark) {
        colors.pressed = c;
    }
    if let Some(c) = tok("WidgetSelectedFill", dark) {
        colors.selected = c;
    }
    if let Some(c) = tok("WidgetDangerHover", dark) {
        colors.danger_hover = c;
    }
    if let Some(c) = tok("WidgetDangerStroke", dark) {
        colors.danger_stroke = c;
    }
    if let Some(c) = tok("WidgetDangerForeground", dark) {
        colors.danger_text = c;
    }
    colors
}

/// `light1` of the Windows accent palette (`AccentPalette` bytes 8..11), else the accent mixed 35 % toward white.
fn accent_light1() -> Option<Color> {
    let mut palette = [0u8; 64];
    if let Some(read) = user_value(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentPalette",
        RRF_RT_REG_BINARY,
        &mut palette,
    ) && read >= 28
    {
        return Some(color(0xFF, palette[8], palette[9], palette[10]));
    }
    let accent = windows_accent()?;
    let mix = |c: u8| (f64::from(c) + (255.0 - f64::from(c)) * 0.35).round() as u8;
    Some(color(0xFF, mix(accent.R), mix(accent.G), mix(accent.B)))
}

/// The widget controls' brushes for widget flyouts (`WindowsPalette` where it overrides the theme's).
pub fn panel_palette(settings: &ot_core::settings::DockSettings) -> crate::panel::Palette {
    let flyout = flyout_colors(settings);
    let dark = flyout.dark;
    let themed_fill = crate::tokens::gradient("WidgetAccentFill", dark)
        .and_then(|g| Some((g.first()?.1, g.last()?.1)));
    let accent_fill = match (windows_accent(), accent_light1()) {
        _ if themed_fill.is_some() => themed_fill.unwrap_or_default(),
        (Some(accent), Some(light1)) => (accent, light1),
        _ if dark => (color(0xFF, 0x2D, 0xD4, 0xBF), color(0xFF, 0x0E, 0xA5, 0xE9)),
        _ => (color(0xFF, 0x14, 0xB8, 0xA6), color(0xFF, 0x02, 0x84, 0xC7)),
    };
    crate::panel::Palette {
        dark,
        text: text_primary(dark),
        text_secondary: text_secondary(dark),
        stroke_subtle: stroke_subtle(dark),
        surface_selected: tok("SurfaceSelected", dark).unwrap_or(if dark {
            color(0x14, 0xFF, 0xFF, 0xFF)
        } else {
            color(0x0D, 0x00, 0x00, 0x00)
        }),
        surface_hover: surface_hover(dark),
        surface_pressed: flyout.pressed,
        // `SurfaceSelectedStrong`: the same overlay as `SurfacePressed` in both palettes.
        surface_selected_strong: tok("SurfaceSelectedStrong", dark).unwrap_or(if dark {
            color(0x22, 0xFF, 0xFF, 0xFF)
        } else {
            color(0x16, 0x00, 0x00, 0x00)
        }),
        surface_popup: surface_popup(dark),
        selected_fill: flyout.selected,
        selected_text: tok("WidgetSelectedForeground", dark).unwrap_or(if dark {
            color(0xFF, 0xA2, 0xC0, 0xFF)
        } else {
            color(0xFF, 0x1D, 0x66, 0xD7)
        }),
        selected_stroke: tok("WidgetSelectedStroke", dark).unwrap_or(if dark {
            color(0x78, 0x91, 0xB8, 0xFF)
        } else {
            color(0x70, 0x2C, 0x74, 0xE9)
        }),
        track: tok("WidgetTrack", dark).unwrap_or(if dark {
            color(0x2E, 0xFF, 0xFF, 0xFF)
        } else {
            color(0x1F, 0x00, 0x00, 0x00)
        }),
        accent_fill,
        danger_hover: flyout.danger_hover,
        danger_stroke: flyout.danger_stroke,
        danger_text: flyout.danger_text,
        widget_hover: flyout.widget_hover,
        widget_pressed: flyout.widget_pressed,
    }
}

/// `ShellButtonHover` and `ShellButtonPressed`: the shell buttons' hover and pressed layers.
pub fn shell_button_colors(dark: bool) -> (Color, Color) {
    if let (Some(hover), Some(pressed)) = (
        tok("ShellButtonHover", dark),
        tok("ShellButtonPressed", dark),
    ) {
        return (hover, pressed);
    }
    match (windows_palette(), dark) {
        (true, true) => (color(0x22, 0xFF, 0xFF, 0xFF), color(0x33, 0xFF, 0xFF, 0xFF)),
        (true, false) => (color(0x14, 0x00, 0x00, 0x00), color(0x22, 0x00, 0x00, 0x00)),
        (false, true) => (color(0x2A, 0xFF, 0xFF, 0xFF), color(0x47, 0xFF, 0xFF, 0xFF)),
        (false, false) => (color(0x1A, 0x00, 0x00, 0x00), color(0x30, 0x00, 0x00, 0x00)),
    }
}

/// `StartTileHover` / `StartTilePressed`: the Start button's states when a theme pack sets them.
pub fn start_tile_colors(dark: bool) -> Option<(Color, Color)> {
    let hover = tok("StartTileHover", dark)?;
    Some((hover, tok("StartTilePressed", dark).unwrap_or(hover)))
}

/// `SurfaceKeyboardFocus` (the keyboard-focus surface OpenTaskbar's widgets use): from the Windows accent palette as
/// `WindowsPalette` derives it (dark2 in dark mode, light3 in light mode), else the palette's own color.
pub fn keyboard_focus_color(dark: bool) -> Color {
    if let Some(themed) = tok("SurfaceKeyboardFocus", dark) {
        return themed;
    }
    let mut palette = [0u8; 64];
    if let Some(read) = user_value(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentPalette",
        RRF_RT_REG_BINARY,
        &mut palette,
    ) && read >= 28
    {
        let at = if dark { 20 } else { 0 };
        return color(0xFF, palette[at], palette[at + 1], palette[at + 2]);
    }
    if let Some(accent) = windows_accent() {
        let (target, amount) = if dark { (0u8, 0.45) } else { (255u8, 0.75) };
        let mix =
            |c: u8| (f64::from(c) + (f64::from(target) - f64::from(c)) * amount).round() as u8;
        return color(0xFF, mix(accent.R), mix(accent.G), mix(accent.B));
    }
    if dark {
        color(0xFF, 0x2A, 0x30, 0x3C)
    } else {
        color(0xFF, 0xE9, 0xE9, 0xE9)
    }
}
