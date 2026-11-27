//! The active theme pack's design tokens (`ThemePacks.Select` + `ThemePacks.ApplyWidgets` + `ThemeManager.SetOverrides`
//! in OpenTaskbar). The palettes in `theme.rs` ask here first, so a pack's value wins over the built-in palette and
//! the Windows accent colors, exactly where OpenTaskbar's override dictionary is merged last. Brushes and colors apply
//! at once; radii, sizes and fonts are not read yet (OpenTaskbar applies those only after a restart).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ot_core::log;
use ot_core::theme_pack::{self, Token};
use ot_render::{Color, color};

use crate::theme::Gradient;

struct Active {
    id: String,
    dark: HashMap<String, Token>,
    light: HashMap<String, Token>,
}

static ACTIVE: RwLock<Option<Arc<Active>>> = RwLock::new(None);

fn current() -> Option<Arc<Active>> {
    ACTIVE.read().ok().and_then(|a| a.clone())
}

/// The selected pack's folder name, when one is active.
pub fn active_id() -> Option<String> {
    current().map(|a| a.id.clone())
}

/// `ThemePacks.Select` + `ApplyWidgets`: activates theme `id` (None restores the built-in look). Selecting the active
/// theme again does nothing unless `reload` rereads it from disk. Returns whether the active theme changed.
pub fn select(id: Option<&str>, reload: bool) -> bool {
    let requested = id.map(str::trim).filter(|s| !s.is_empty());
    let same = match (active_id(), requested) {
        (Some(a), Some(r)) => a.eq_ignore_ascii_case(r),
        (None, None) => true,
        _ => false,
    };
    if same && !reload {
        return false;
    }
    let next = theme_pack::select(requested).map(|pack| {
        let mut problems = Vec::new();
        let (dark, light) = match &pack.widgets {
            Some(widgets) => (
                widgets.resolve(true, &mut problems),
                widgets.resolve(false, &mut problems),
            ),
            None => (HashMap::new(), HashMap::new()),
        };
        problems.sort();
        problems.dedup();
        for problem in problems {
            log::warn(format!("Widget pack: {problem}"));
        }
        log::info(format!(
            "Theme pack \"{}\" selected ({} tokens).",
            pack.id,
            dark.len().max(light.len())
        ));
        Arc::new(Active {
            id: pack.id,
            dark,
            light,
        })
    });
    if next.is_none() && requested.is_none() && !same {
        log::info("Theme pack: built-in look.");
    }
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = next;
    }
    true
}

fn token(key: &str, dark: bool) -> Option<Token> {
    let active = current()?;
    let map = if dark { &active.dark } else { &active.light };
    map.get(key).cloned()
}

fn argb(value: u32) -> Color {
    color(
        (value >> 24) as u8,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    )
}

/// A brush token as one color: the color of a solid brush, the first stop of a gradient.
pub fn color_of(key: &str, dark: bool) -> Option<Color> {
    match token(key, dark)? {
        Token::Brush(brush) => Some(argb(brush.first())),
        _ => None,
    }
}

/// A brush token as gradient stops (a solid brush is one color at both ends).
pub fn gradient(key: &str, dark: bool) -> Option<Gradient> {
    match token(key, dark)? {
        Token::Brush(brush) if brush.is_solid() => {
            let c = argb(brush.first());
            Some(vec![(0.0, c), (1.0, c)])
        }
        Token::Brush(brush) => Some(brush.stops.iter().map(|&(o, c)| (o, argb(c))).collect()),
        _ => None,
    }
}

/// A gradient with its start and end points in the unit square.
pub type DirectedGradient = (Gradient, (f32, f32), (f32, f32));

/// A brush token with its gradient direction.
pub fn brush(key: &str, dark: bool) -> Option<DirectedGradient> {
    match token(key, dark)? {
        Token::Brush(brush) => Some((
            brush.stops.iter().map(|&(o, c)| (o, argb(c))).collect(),
            brush.start,
            brush.end,
        )),
        _ => None,
    }
}

/// A text token (font family, background mode).
pub fn text(key: &str, dark: bool) -> Option<String> {
    match token(key, dark)? {
        Token::Text(text) => Some(text),
        _ => None,
    }
}
