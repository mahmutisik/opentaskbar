//! The Battery widget. By the user's request it differs from OpenTaskbar's `BatteryWidget` (a card filled with
//! the level's color, waves and a sheen while charging): a neutral card like the other widgets with a small battery
//! outline filled to the level in the level's color (teal above 50 %, amber to 50 %, red to 20 %), the level as
//! text and a bolt while charging; nothing animates. Read with `GetSystemPowerStatus` every 15 s; hidden without
//! a battery. A click opens Windows' battery settings.

use std::time::{Duration, Instant};

use ot_render::{Color, color};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
use windows::core::Result as WinResult;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use super::{Action, Canvas, Card, Env, Painter, Widget, d2d, neutral_card, text_colors};

/// `PollIntervalMs`.
const POLL: Duration = Duration::from_millis(15_000);
const PAD: f32 = 10.0;
/// The battery outline, its terminal and the gap to the text.
const BODY_W: f32 = 20.0;
const BODY_H: f32 = 11.0;
const BODY_RADIUS: f32 = 3.0;
const NUB_W: f32 = 2.0;
const NUB_H: f32 = 5.0;
const ICON_GAP: f32 = 7.0;
/// As the Audio card's level.
const TEXT_SIZE: f32 = 12.5;
const BOLT: f32 = 11.0;
const BOLT_GAP: f32 = 3.0;

/// `BatteryWidget.IsHardwareAvailable`: whether Windows reports a battery.
pub fn present() -> bool {
    sample().0
}

/// `SystemStats.SampleBattery`: (present, percent, charging).
fn sample() -> (bool, u8, bool) {
    // Test seam: `OT_TEST_NO_BATTERY_SECONDS=n` (or `from-to`) reports no battery for that part of the
    // process's life, in seconds.
    if let Ok(value) = std::env::var("OT_TEST_NO_BATTERY_SECONDS") {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let (from, to) = match value.split_once('-') {
            Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
            None => (0, value.parse().unwrap_or(0)),
        };
        let age = START.get_or_init(Instant::now).elapsed();
        if age >= Duration::from_secs(from) && age < Duration::from_secs(to) {
            return (false, 0, false);
        }
    }
    // SAFETY: plain out-parameter call.
    let status = unsafe {
        let mut status: SYSTEM_POWER_STATUS = std::mem::zeroed();
        (GetSystemPowerStatus(&mut status) != 0).then_some(status)
    };
    match status {
        Some(s)
            if s.BatteryFlag != 255 && s.BatteryFlag & 128 == 0 && s.BatteryLifePercent != 255 =>
        {
            (true, s.BatteryLifePercent.min(100), s.BatteryFlag & 8 != 0)
        }
        _ => (false, 0, false),
    }
}

/// The level's color (`PaletteForLevel`'s top stops, which read well on both themes).
fn level_color(percent: u8) -> Color {
    if percent <= 20 {
        color(0xFF, 0xD9, 0x5C, 0x66)
    } else if percent <= 50 {
        color(0xFF, 0xD6, 0xA2, 0x4E)
    } else {
        color(0xFF, 0x4F, 0xB5, 0xA3)
    }
}

#[derive(Default)]
pub struct Battery {
    polled: Option<Instant>,
    present: bool,
    percent: u8,
    charging: bool,
    /// The level as `Percent` ("%77").
    display: String,
    summary: String,
}

impl Widget for Battery {
    fn id(&self) -> &'static str {
        "Battery"
    }

    fn update(&mut self, env: &Env) -> bool {
        if self.polled.is_some_and(|t| t.elapsed() < POLL) && !self.summary.is_empty() {
            return false;
        }
        self.polled = Some(Instant::now());
        let (present, percent, charging) = sample();
        let charging = present && charging;
        if present != self.present || self.summary.is_empty() {
            ot_core::log::info(format!(
                "Battery widget: present={present} percent={percent} charging={charging}."
            ));
        }
        // Tooltip and accessible name: "Pil: %77", "Pil · şarj oluyor: %77", "Pil: Kullanılamıyor".
        let summary = if present {
            format!(
                "{}: {}",
                env.text(if charging {
                    "BatteryCharging"
                } else {
                    "Battery"
                }),
                env.percent(i64::from(percent))
            )
        } else {
            format!(
                "{}: {}",
                env.text("WidgetBattery"),
                env.text("SystemDataUnavailable")
            )
        };
        let display = env.percent(i64::from(percent));
        let changed = present != self.present
            || percent != self.percent
            || charging != self.charging
            || display != self.display
            || summary != self.summary;
        self.display = display;
        self.present = present;
        self.percent = percent;
        self.charging = charging;
        self.summary = summary;
        changed
    }

    fn width(&self, painter: &Painter) -> WinResult<f32> {
        let text = painter.text(&self.display, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let bolt = if self.charging { BOLT_GAP + BOLT } else { 0.0 };
        Ok((PAD + BODY_W + NUB_W + ICON_GAP + text.width + bolt + PAD).ceil())
    }

    fn draw(&self, canvas: &Canvas) -> WinResult<()> {
        let (primary, secondary) = text_colors(canvas.dark);
        let h = canvas.height;
        // One device pixel per DIP (rounded) for the outline, so it stays crisp at every scale.
        let stroke = canvas.scale.round().max(1.0) / canvas.scale;
        let x0 = canvas.snap(PAD);
        let y0 = canvas.snap((h - BODY_H) / 2.0);
        let level = level_color(self.percent);
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe {
            let context = canvas.context;
            let outline = context.CreateSolidColorBrush(&d2d(secondary), None)?;
            context.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x0 + stroke / 2.0,
                        top: y0 + stroke / 2.0,
                        right: x0 + BODY_W - stroke / 2.0,
                        bottom: y0 + BODY_H - stroke / 2.0,
                    },
                    radiusX: BODY_RADIUS,
                    radiusY: BODY_RADIUS,
                },
                &outline,
                stroke,
                None,
            );
            let nub_top = canvas.snap(y0 + (BODY_H - NUB_H) / 2.0);
            context.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x0 + BODY_W - stroke,
                        top: nub_top,
                        right: x0 + BODY_W + NUB_W,
                        bottom: nub_top + NUB_H,
                    },
                    radiusX: NUB_W / 2.0,
                    radiusY: NUB_W / 2.0,
                },
                &outline,
            );
            // The level: inside the outline with a 1 DIP gap, at least a sliver so an empty battery still shows red.
            let inset = stroke + 1.0;
            let full = BODY_W - 2.0 * inset;
            let width = canvas
                .snap(full * f32::from(self.percent.clamp(5, 100)) / 100.0)
                .max(stroke);
            let fill = context.CreateSolidColorBrush(&d2d(level), None)?;
            context.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x0 + inset,
                        top: y0 + inset,
                        right: x0 + inset + width,
                        bottom: y0 + BODY_H - inset,
                    },
                    radiusX: 1.0,
                    radiusY: 1.0,
                },
                &fill,
            );
        }
        let text =
            canvas
                .painter
                .text(&self.display, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        let text_x = PAD + BODY_W + NUB_W + ICON_GAP;
        canvas.text(&text, text_x, (h - text.height) / 2.0, primary)?;
        if self.charging {
            canvas.glyph(
                "Zap",
                text_x + text.width + BOLT_GAP,
                (h - BOLT) / 2.0,
                BOLT,
                level,
            )?;
        }
        Ok(())
    }

    fn card(&self, dark: bool) -> Card {
        neutral_card(dark)
    }

    fn tooltip(&self) -> Option<String> {
        Some(self.summary.clone())
    }

    fn name(&self) -> String {
        self.summary.clone()
    }

    fn visible(&self) -> bool {
        self.present
    }

    fn click(&self) -> Option<Action> {
        Some(Action::OpenUri("ms-settings:batterysaver"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_colors() {
        assert_eq!(level_color(20), color(0xFF, 0xD9, 0x5C, 0x66));
        assert_eq!(level_color(50), color(0xFF, 0xD6, 0xA2, 0x4E));
        assert_eq!(level_color(77), color(0xFF, 0x4F, 0xB5, 0xA3));
    }
}
