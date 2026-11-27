//! The Clock widget (`ClockWidget`): an icon, the time in bold with tabular figures, and the date beside it (the
//! culture's short month-day and the abbreviated weekday, or another `DateStyle`). Its card is tinted by the time
//! of day with OpenTaskbar's own morning, noon and evening themes (sun or moon icon in the theme's accent).

use ot_core::settings::ClockDateStyle;
use ot_render::{Color, color};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
};
use windows::core::Result as WinResult;

use super::{Canvas, Card, Env, Fill, Painter, Widget, text_colors};

/// `ClockWidget.TimeOfDayPeriod`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    Morning,
    Noon,
    #[default]
    Evening,
}

/// `GetPeriod`: 6–10 morning, 11–17 noon, otherwise evening.
pub fn period(hour: u16) -> Period {
    match hour {
        6..=10 => Period::Morning,
        11..=17 => Period::Noon,
        _ => Period::Evening,
    }
}

/// `GetTheme`: (gradient start, gradient end, rim, accent, icon).
fn theme(period: Period) -> (Color, Color, Color, Color, &'static str) {
    match period {
        Period::Morning => (
            color(0x38, 0xF5, 0x9E, 0x0B),
            color(0x10, 0xFB, 0x92, 0x3C),
            color(0x48, 0xF5, 0x9E, 0x0B),
            color(0xFF, 0xF5, 0x9E, 0x0B),
            "Sun",
        ),
        Period::Noon => (
            color(0x34, 0x38, 0xBD, 0xF8),
            color(0x0E, 0x02, 0x84, 0xC7),
            color(0x48, 0x38, 0xBD, 0xF8),
            color(0xFF, 0x0E, 0xA5, 0xE9),
            "Sun",
        ),
        Period::Evening => (
            color(0x3C, 0x63, 0x66, 0xF1),
            color(0x12, 0x43, 0x38, 0xCA),
            color(0x4E, 0x81, 0x8C, 0xF8),
            color(0xFF, 0xA7, 0x8B, 0xFA),
            "Moon",
        ),
    }
}

#[derive(Default)]
pub struct Clock {
    time: String,
    date: Option<String>,
    weekday: Option<String>,
    tooltip: String,
    name: String,
    period: Period,
}

/// Padding inside the card (`Padding="10,0"`), icon size and gaps.
const PAD: f32 = 10.0;
const ICON: f32 = 15.0;
const ICON_GAP: f32 = 8.0;
const DATE_GAP: f32 = 9.0;
const TIME_SIZE: f32 = 15.0;
const DATE_SIZE: f32 = 11.5;
const WEEKDAY_SIZE: f32 = 10.5;

impl Widget for Clock {
    fn id(&self) -> &'static str {
        "Clock"
    }

    fn update(&mut self, env: &Env) -> bool {
        let s = env.settings;
        let picture = match (s.clock24_hour, s.clock_show_seconds) {
            (true, false) => "HH:mm",
            (true, true) => "HH:mm:ss",
            (false, false) => "hh:mm tt",
            (false, true) => "hh:mm:ss tt",
        };
        let now = &env.now;
        let time = env.locale.time(now, picture);
        let (date, weekday) = match s.date_style {
            ClockDateStyle::Hidden => (None, None),
            ClockDateStyle::DayAndShortDate => {
                let pattern = env.locale.month_day_pattern().replace("MMMM", "MMM");
                (
                    Some(env.locale.date_picture(now, &pattern)),
                    Some(env.locale.date_picture(now, "ddd")),
                )
            }
            ClockDateStyle::ShortDateOnly => (Some(env.locale.date_picture(now, "MMM d")), None),
            ClockDateStyle::NumericDate => (Some(env.locale.short_date(now)), None),
        };
        let tooltip = env.locale.long_date(now);
        let period = period(now.wHour);
        let changed = time != self.time
            || date != self.date
            || weekday != self.weekday
            || period != self.period;
        self.time = time;
        self.date = date;
        self.weekday = weekday;
        self.tooltip = tooltip;
        self.name = env.text("Calendar");
        self.period = period;
        changed
    }

    fn width(&self, painter: &Painter) -> WinResult<f32> {
        let time = painter.text(&self.time, TIME_SIZE, DWRITE_FONT_WEIGHT_BOLD, true)?;
        let mut width = PAD + ICON + ICON_GAP + time.width + PAD;
        if let Some(date) = &self.date {
            let mut column = painter
                .text(date, DATE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?
                .width;
            if let Some(weekday) = &self.weekday {
                column = column.max(
                    painter
                        .text(weekday, WEEKDAY_SIZE, DWRITE_FONT_WEIGHT_NORMAL, false)?
                        .width,
                );
            }
            width += DATE_GAP + column;
        }
        // WPF lays out on whole device pixels; a card never shrinks below its content.
        Ok(width.ceil())
    }

    fn draw(&self, canvas: &Canvas) -> WinResult<()> {
        let p = canvas.painter;
        let (primary, secondary) = text_colors(canvas.dark);
        let (_, _, _, accent, icon) = theme(self.period);
        let h = canvas.height;
        let mut x = PAD;
        canvas.icon(icon, x, (h - ICON) / 2.0, ICON, accent)?;
        x += ICON + ICON_GAP;
        let time = p.text(&self.time, TIME_SIZE, DWRITE_FONT_WEIGHT_BOLD, true)?;
        canvas.text(&time, x, (h - time.height) / 2.0, primary)?;
        x += time.width;
        if let Some(date) = &self.date {
            x += DATE_GAP;
            let date = p.text(date, DATE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?;
            let weekday = match &self.weekday {
                Some(w) => Some(p.text(w, WEEKDAY_SIZE, DWRITE_FONT_WEIGHT_NORMAL, false)?),
                None => None,
            };
            let column = date.height + weekday.as_ref().map_or(0.0, |w| w.height);
            let top = (h - column) / 2.0;
            canvas.text(&date, x, top, primary)?;
            if let Some(weekday) = &weekday {
                canvas.text(weekday, x, top + date.height, secondary)?;
            }
        }
        Ok(())
    }

    fn card(&self, _dark: bool) -> Card {
        let (start, end, stroke, _, _) = theme(self.period);
        Card {
            fill: Fill::Diagonal(start, end),
            stroke,
        }
    }

    fn tooltip(&self) -> Option<String> {
        (!self.tooltip.is_empty()).then(|| self.tooltip.clone())
    }

    fn name(&self) -> String {
        // `AutomationProperties.Name="{local:Loc Calendar}"`.
        self.name.clone()
    }

    /// `ClockButton_Click`: the calendar flyout.
    fn click(&self) -> Option<super::Action> {
        Some(super::Action::Flyout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_like_opentaskbar() {
        assert_eq!(period(8), Period::Morning);
        assert_eq!(period(14), Period::Noon);
        assert_eq!(period(21), Period::Evening);
        assert_eq!(period(2), Period::Evening);
    }
}
