//! Dock widgets (OpenTaskbar's `DockWidget`, `DockWidgetService` and `WidgetCoordinator`): small cards in the bar
//! after the apps. Each widget keeps its own state, recomputes it on the shared tick and reports whether its
//! drawing changed, so a card is redrawn only when what it shows changes. The dock lays the cards out like other
//! items and draws each card's content into one composition surface; the card background and the hover and
//! pressed layers are composition shapes under it.
//!
//! Card style (the user's choice over OpenTaskbar 0.3, whose cards are transparent): the pre-0.3 OpenTaskbar card,
//! a translucent fill with a 1 DIP rim (`WidgetCard` / `WidgetCardStroke`), which widgets may tint with their own
//! colors (the clock's time-of-day themes).

pub mod ai_usage;
pub mod audio;
pub mod audio_flyout;
pub mod battery;
pub mod calendar_flyout;
pub mod claude_api;
pub mod clock;
pub mod gpu;
pub mod system_stats;

use ot_core::settings::DockSettings;
use ot_render::{Color, color};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteFactory, IDWriteFontCollection, IDWriteTextLayout,
};
use windows::core::{HSTRING, Result as WinResult};
use windows_sys::Win32::Foundation::SYSTEMTIME;

use crate::locale::Locale;

/// What a widget reads on a tick.
pub struct Env<'a> {
    pub now: SYSTEMTIME,
    pub locale: &'a Locale,
    pub settings: &'a DockSettings,
    /// The app language, for the widgets' texts.
    pub language: &'a ot_core::language::AppLanguage,
}

impl Env<'_> {
    /// A localized text.
    pub fn text(&self, key: &str) -> String {
        ot_core::language::Catalog::get(key, self.language).into_owned()
    }

    /// `UiText.Format("Percent", n)`: "77%" / "%77".
    pub fn percent(&self, n: i64) -> String {
        self.text("Percent").replace("{0}", &n.to_string())
    }
}

/// The dock window, woken by widgets whose data changes between ticks (Core Audio notifications).
static WAKE_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The message the dock gets from `wake`: update the widgets now.
pub const WM_WIDGETS_WAKE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 17;

/// Tells `wake` which window to post to.
pub fn set_wake_window(hwnd: isize) {
    WAKE_WINDOW.store(hwnd, std::sync::atomic::Ordering::SeqCst);
}

/// Asks the dock to update its widgets now (any thread).
pub fn wake() {
    let hwnd = WAKE_WINDOW.load(std::sync::atomic::Ordering::SeqCst);
    if hwnd != 0 {
        // SAFETY: posting to the dock window.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd as _,
                WM_WIDGETS_WAKE,
                0,
                0,
            );
        }
    }
}

/// What a click on a widget does.
pub enum Action {
    /// Opens a URI with the shell (`ms-settings:` pages, web pages).
    OpenUri(&'static str),
    /// Toggles the widget's flyout (`DockFlyout.Toggle`).
    Flyout,
}

/// A card's fill: one color, or a gradient from the top-left to the bottom-right corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    Solid(Color),
    Diagonal(Color, Color),
}

/// A card's background and rim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    pub fill: Fill,
    pub stroke: Color,
}

/// The plain card (`WidgetCard`, `WidgetCardStroke` of OpenTaskbar before 0.3).
pub fn neutral_card(dark: bool) -> Card {
    let mut card = palette_card(dark);
    if let Some(fill) = crate::tokens::brush("WidgetCard", dark) {
        card.fill = match fill.0.as_slice() {
            [(_, a)] => Fill::Solid(*a),
            [(_, a), .., (_, b)] => Fill::Diagonal(*a, *b),
            [] => card.fill,
        };
    }
    if let Some(stroke) = crate::tokens::color_of("WidgetCardStroke", dark) {
        card.stroke = stroke;
    }
    card
}

fn palette_card(dark: bool) -> Card {
    if dark {
        Card {
            fill: Fill::Solid(color(0x14, 0xFF, 0xFF, 0xFF)),
            stroke: color(0x1E, 0xFF, 0xFF, 0xFF),
        }
    } else {
        Card {
            fill: Fill::Solid(color(0x0D, 0x00, 0x00, 0x00)),
            stroke: color(0x16, 0x00, 0x00, 0x00),
        }
    }
}

/// `TextPrimary` and `TextSecondary`.
pub fn text_colors(dark: bool) -> (Color, Color) {
    (
        crate::theme::text_primary(dark),
        crate::theme::text_secondary(dark),
    )
}

/// `DockSizeProfile.WidgetCardHeight`: the cell height less 2, within 34–46 DIP.
pub fn card_height(cell_height: f64) -> f64 {
    if cell_height <= 0.0 {
        46.0
    } else {
        (cell_height - 2.0).floor().clamp(34.0, 46.0)
    }
}

/// `WidgetSpacing`: 3 DIP on each side of a card.
pub const SPACING: f64 = 3.0;

/// Text measured and ready to draw.
pub struct Text {
    pub layout: IDWriteTextLayout,
    pub width: f32,
    pub height: f32,
}

/// DirectWrite with the Inter faces the widgets use (shared with the widget flyouts).
#[derive(Clone)]
pub struct Painter {
    factory: IDWriteFactory,
    inter: Option<IDWriteFontCollection>,
}

impl Painter {
    pub fn new(factory: IDWriteFactory, inter: Option<IDWriteFontCollection>) -> Self {
        Self { factory, inter }
    }

    pub fn factory(&self) -> &IDWriteFactory {
        &self.factory
    }

    /// One line of `text` in Inter at `size` DIP and `weight`, optionally with tabular figures.
    pub fn text(
        &self,
        text: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        tabular: bool,
    ) -> WinResult<Text> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: DirectWrite calls with valid arguments.
        unsafe {
            let format = self.factory.CreateTextFormat(
                &HSTRING::from(if self.inter.is_some() {
                    "Inter"
                } else {
                    "Segoe UI"
                }),
                self.inter.as_ref(),
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from(""),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let layout = self
                .factory
                .CreateTextLayout(&wide, &format, 10_000.0, 1_000.0)?;
            if tabular {
                // `Typography.NumeralAlignment = Tabular`.
                let typography = self.factory.CreateTypography()?;
                typography.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                    parameter: 1,
                })?;
                layout.SetTypography(
                    &typography,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: wide.len() as u32,
                    },
                )?;
            }
            let mut metrics = Default::default();
            layout.GetMetrics(&mut metrics)?;
            Ok(Text {
                layout,
                width: metrics.width,
                height: metrics.height,
            })
        }
    }
}

/// Converts a palette color for Direct2D.
pub fn d2d(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(c.R) / 255.0,
        g: f32::from(c.G) / 255.0,
        b: f32::from(c.B) / 255.0,
        a: f32::from(c.A) / 255.0,
    }
}

/// Drawing a card's content: the context is in DIPs (the scale is applied), `height` is the card's height.
pub struct Canvas<'a> {
    pub context: &'a ID2D1DeviceContext,
    pub painter: &'a Painter,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub dark: bool,
    /// The card's corner radius (`WidgetRadius`).
    pub radius: f32,
}

impl Canvas<'_> {
    /// Snaps a DIP coordinate to a device pixel (WPF layout rounding).
    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub fn text(&self, text: &Text, x: f32, y: f32, ink: Color) -> WinResult<()> {
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let brush = self.context.CreateSolidColorBrush(&d2d(ink), None)?;
            self.context.DrawTextLayout(
                ot_render::Vector2 {
                    X: self.snap(x),
                    Y: self.snap(y),
                },
                &text.layout,
                &brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(())
    }

    /// A 24-unit filled glyph (`WidgetGlyph`) in a `size` DIP box at (`x`, `y`).
    pub fn glyph(&self, name: &str, x: f32, y: f32, size: f32, ink: Color) -> WinResult<()> {
        let Some(data) = crate::vector_icon::path_data(name) else {
            return Ok(());
        };
        let (data, nonzero) = match data.strip_prefix("F1") {
            Some(rest) => (rest, true),
            None => (data.strip_prefix("F0").unwrap_or(data), false),
        };
        let s = size / 24.0;
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let factory = self.context.GetFactory()?;
            let path = crate::vector_icon::geometry_with_fill(
                &factory,
                &crate::vector_icon::parse(data),
                nonzero,
            )?;
            let placed = factory.CreateTransformedGeometry(
                &path,
                &ot_render::Matrix3x2 {
                    M11: s,
                    M12: 0.0,
                    M21: 0.0,
                    M22: s,
                    M31: self.snap(x),
                    M32: self.snap(y),
                },
            )?;
            let brush = self.context.CreateSolidColorBrush(&d2d(ink), None)?;
            self.context.FillGeometry(&placed, &brush, None);
        }
        Ok(())
    }

    /// A vertical line (dividers between a widget's parts).
    pub fn vline(&self, x: f32, top: f32, height: f32, ink: Color) -> WinResult<()> {
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let brush = self.context.CreateSolidColorBrush(&d2d(ink), None)?;
            let x = self.snap(x);
            let top = self.snap(top);
            self.context.FillRectangle(
                &windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
                    left: x,
                    top,
                    right: x + 1.0 / self.scale * self.scale.round().max(1.0),
                    bottom: top + height,
                },
                &brush,
            );
        }
        Ok(())
    }

    /// A 24-unit stroked icon (`WidgetIcon`) in a `size` DIP box at (`x`, `y`).
    pub fn icon(&self, name: &str, x: f32, y: f32, size: f32, ink: Color) -> WinResult<()> {
        use windows::core::Interface;
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let brush: windows::Win32::Graphics::Direct2D::ID2D1Brush = self
                .context
                .CreateSolidColorBrush(&d2d(ink), None)?
                .cast()?;
            crate::vector_icon::draw_canvas(
                self.context,
                name,
                self.snap(x),
                self.snap(y),
                size,
                &brush,
            )
        }
    }
}

/// A dock widget.
pub trait Widget {
    /// The settings id ("Clock").
    fn id(&self) -> &'static str;
    /// Reads the time and settings; true when the drawing changed.
    fn update(&mut self, env: &Env) -> bool;
    /// The card's width in DIPs for the current content.
    fn width(&self, painter: &Painter) -> WinResult<f32>;
    /// Draws the content (not the card) into a canvas the card's size.
    fn draw(&self, canvas: &Canvas) -> WinResult<()>;
    /// Draws what lies under the hover layers (the battery's level fill); false when there is nothing.
    fn draw_backdrop(&self, _canvas: &Canvas) -> WinResult<bool> {
        Ok(false)
    }
    /// The card's background and rim.
    fn card(&self, dark: bool) -> Card;
    /// The tooltip, if any.
    fn tooltip(&self) -> Option<String>;
    /// The accessible name (`AutomationProperties.Name`), localized.
    fn name(&self) -> String;
    /// False hides the widget (no battery): `IsHardwareAvailable` / `HasData`.
    fn visible(&self) -> bool {
        true
    }
    /// What a click does.
    fn click(&self) -> Option<Action> {
        None
    }
    /// The card's corner radius for the dock's `WidgetRadius`.
    fn radius(&self, widget_radius: f32) -> f32 {
        widget_radius
    }
}

/// Widgets with a dock slot: id, `DockOrder` and legacy alias ids (`DockWidgetService.LiveDescriptors`). Only the
/// ported ones are listed; the others arrive with later steps of Phase 7.
const LIVE: &[(&str, u32, &[&str])] = &[
    ("SystemStats", 1, &[]),
    ("Battery", 6, &[]),
    ("AiUsageClaude", 9, &[]),
    ("Audio", 16, &[]),
    ("Clock", 18, &["Date"]),
];

/// Whether the widget `id` is ported (has a dock slot in this build).
pub fn is_supported(id: &str) -> bool {
    LIVE.iter()
        .any(|(live, _, _)| live.eq_ignore_ascii_case(id))
}

pub(crate) fn create(id: &str) -> Option<Box<dyn Widget>> {
    match id {
        "Clock" => Some(Box::new(clock::Clock::default())),
        "Battery" => Some(Box::new(battery::Battery::default())),
        "Audio" => Some(Box::new(audio::Audio::default())),
        "AiUsageClaude" => Some(Box::new(ai_usage::ClaudeUsage::default())),
        "SystemStats" => Some(Box::new(system_stats::SystemStats::default())),
        _ => None,
    }
}

/// The enabled widgets in dock order: by their `widget:` tokens in `DockItemOrder`, else `WidgetOrder`, else the
/// registry's `DockOrder`. Focus mode shows none (`WidgetCoordinator.Apply`).
pub fn enabled(settings: &DockSettings) -> Vec<&'static str> {
    if settings.focus_mode_enabled {
        return Vec::new();
    }
    let selected = settings.widgets.clone().unwrap_or_default();
    let has = |id: &str| selected.iter().any(|s| s.eq_ignore_ascii_case(id));
    let mut ids: Vec<(usize, &'static str)> = LIVE
        .iter()
        .filter(|(id, _, aliases)| has(id) || aliases.iter().any(|a| has(a)))
        .map(|&(id, order, _)| {
            let token = format!("widget:{id}");
            let rank = settings
                .dock_item_order
                .as_ref()
                .and_then(|o| o.iter().position(|t| t.eq_ignore_ascii_case(&token)))
                .or_else(|| {
                    settings
                        .widget_order
                        .as_ref()
                        .and_then(|o| o.iter().position(|w| w.eq_ignore_ascii_case(id)))
                })
                .unwrap_or(10_000 + order as usize);
            (rank, id)
        })
        .collect();
    ids.sort_by_key(|(rank, _)| *rank);
    ids.into_iter().map(|(_, id)| id).collect()
}

/// The live widget instances, kept across dock rebuilds.
#[derive(Default)]
pub struct Widgets {
    items: Vec<Box<dyn Widget>>,
}

impl Widgets {
    /// Keeps the instances of `ids` (creating missing ones) in that order; true when the set changed.
    pub fn sync(&mut self, ids: &[&'static str]) -> bool {
        let before: Vec<&'static str> = self.items.iter().map(|w| w.id()).collect();
        if before == ids {
            return false;
        }
        let mut old = std::mem::take(&mut self.items);
        for id in ids {
            match old.iter().position(|w| w.id() == *id) {
                Some(i) => self.items.push(old.remove(i)),
                None => {
                    if let Some(widget) = create(id) {
                        self.items.push(widget);
                    }
                }
            }
        }
        true
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.items.iter().map(|w| w.id()).collect()
    }

    /// The widgets that show now (a laptop's battery, not a desktop's).
    pub fn visible_ids(&self) -> Vec<&'static str> {
        self.items
            .iter()
            .filter(|w| w.visible())
            .map(|w| w.id())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&dyn Widget> {
        self.items.iter().find(|w| w.id() == id).map(|w| w.as_ref())
    }

    /// Updates every widget; returns the ids whose drawing changed.
    pub fn update(&mut self, env: &Env) -> Vec<&'static str> {
        self.items
            .iter_mut()
            .filter_map(|w| w.update(env).then(|| w.id()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_follows_settings_and_order() {
        let mut s = DockSettings::default();
        assert!(enabled(&s).is_empty());
        s.widgets = Some(vec!["date".into(), "Weather".into()]);
        assert_eq!(enabled(&s), vec!["Clock"]);
        s.focus_mode_enabled = true;
        assert!(enabled(&s).is_empty());
    }

    #[test]
    fn card_height_follows_the_cell() {
        assert_eq!(card_height(44.0), 42.0);
        assert_eq!(card_height(30.0), 34.0);
        assert_eq!(card_height(60.0), 46.0);
    }
}
