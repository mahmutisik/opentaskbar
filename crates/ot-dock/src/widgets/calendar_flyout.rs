//! The Clock widget's calendar flyout (`ClockWidget.BuildCalendar`), 312 DIP wide: the live time (28 Bold) and the
//! full date with the system time zone beside them; month and year navigation (`WidgetIconButton`s around the
//! month title, which jumps to today); a 7 × 6 grid starting on the culture's first day of the week (today in
//! `WidgetSelectedFill`, a picked day in `SurfaceSelectedStrong`, other months' days faded); a divider and a
//! footer with today's date and "Go to today".
//!
//! The flyout opens on the current month with nothing picked (`ClockButton_Click`); the dock asks it to redraw
//! whenever the clock card's time changes.

use ot_core::language::{AppLanguage, Catalog};
use ot_core::settings::DockSettings;
use ot_render::Color;
use windows::core::Result as WinResult;
use windows_sys::Win32::Foundation::SYSTEMTIME;

use crate::locale::{self, Locale};
use crate::panel::{Align, Button, Content, Element, Kind, Ui, Weight};

const WIDTH: f32 = 312.0;
const PREVIOUS_YEAR: u32 = 1;
const PREVIOUS_MONTH: u32 = 2;
const MONTH: u32 = 3;
const NEXT_MONTH: u32 = 4;
const NEXT_YEAR: u32 = 5;
const TODAY: u32 = 6;
/// Day cell `i` (0..42).
const CELL: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Date {
    year: i32,
    month: u32,
    day: u32,
}

/// Days since 1970-01-01 (proleptic Gregorian).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    Date { year, month, day }
}

impl Date {
    fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// 0 = Monday .. 6 = Sunday (1970-01-01 was a Thursday).
    fn weekday(self) -> u32 {
        (self.days() + 3).rem_euclid(7) as u32
    }

    fn add_days(self, n: i64) -> Date {
        civil_from_days(self.days() + n)
    }

    /// The first of the month `n` months away (`AddMonths` on the first of a month).
    fn add_months(self, n: i32) -> Date {
        let index = self.year * 12 + self.month as i32 - 1 + n;
        Date {
            year: index.div_euclid(12),
            month: index.rem_euclid(12) as u32 + 1,
            day: 1,
        }
    }

    fn first_of_month(self) -> Date {
        Date { day: 1, ..self }
    }

    fn system_time(self) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: self.year as u16,
            wMonth: self.month as u16,
            wDayOfWeek: ((self.weekday() + 1) % 7) as u16,
            wDay: self.day as u16,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        }
    }
}

fn today() -> (Date, SYSTEMTIME) {
    let now = locale::now();
    (
        Date {
            year: i32::from(now.wYear),
            month: u32::from(now.wMonth),
            day: u32::from(now.wDay),
        },
        now,
    )
}

/// `TextInfo.ToTitleCase` for month titles: each word's first letter in upper case (Turkish dotted İ in Turkish).
fn title_case(text: &str, turkish: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    for c in text.chars() {
        if start && c.is_alphabetic() {
            if turkish && c == 'i' {
                out.push('İ');
            } else {
                out.extend(c.to_uppercase());
            }
            start = false;
        } else {
            out.push(c);
            if c.is_whitespace() {
                start = true;
            } else if c.is_alphabetic() {
                start = false;
            }
        }
    }
    out
}

pub struct CalendarFlyout {
    language: AppLanguage,
    locale: Locale,
    picture: &'static str,
    shown: Date,
    selected: Option<Date>,
    /// The culture's first day of the week (0 = Monday) and the header's day names from it, read once.
    first_day: u32,
    day_names: Vec<String>,
}

impl CalendarFlyout {
    pub fn new(settings: &DockSettings) -> Self {
        let (today, _) = today();
        let locale = Locale::new(settings.language.culture_name());
        let first_day = locale.first_day_of_week();
        let day_names = (0..7)
            .map(|i| locale.shortest_day_name(first_day + i))
            .collect();
        Self {
            language: settings.language.clone(),
            locale,
            first_day,
            day_names,
            picture: match (settings.clock24_hour, settings.clock_show_seconds) {
                (true, false) => "HH:mm",
                (true, true) => "HH:mm:ss",
                (false, false) => "hh:mm tt",
                (false, true) => "hh:mm:ss tt",
            },
            shown: today.first_of_month(),
            selected: None,
        }
    }

    fn text(&self, key: &str) -> String {
        Catalog::get(key, &self.language).into_owned()
    }

    fn turkish(&self) -> bool {
        self.language.two_letter().eq_ignore_ascii_case("tr")
    }

    fn push_text(
        ui: &mut Ui,
        rect: (f32, f32, f32, f32),
        text: crate::widgets::Text,
        color: Color,
        align: Align,
    ) {
        ui.push(Element {
            id: 0,
            rect,
            kind: Kind::Text { text, color, align },
            tip: None,
            name: None,
            scrolled: false,
        });
    }

    /// `NavButton`: a 26 DIP `WidgetIconButton` with a 14 DIP glyph in `TextPrimary`.
    fn nav_button(&self, ui: &mut Ui, id: u32, x: f32, y: f32, icon: &'static str, key: &str) {
        let p = ui.palette;
        let label = self.text(key);
        let button = Button {
            icon,
            icon_size: 14.0,
            foreground: p.text,
            background: Color::default(),
            border: None,
            radius: 8.0,
            hover: p.widget_hover,
            pressed: p.widget_pressed,
            enabled: true,
        };
        ui.push(Element {
            id,
            rect: (x, y, x + 26.0, y + 26.0),
            kind: Kind::Button(button),
            tip: Some(label.clone()),
            name: Some(label),
            scrolled: false,
        });
    }

    /// A `FlyoutButton` with a text label: transparent, `SurfaceHover` / `SurfacePressed` layers, 8 DIP corners.
    #[allow(clippy::too_many_arguments)]
    fn text_button(
        ui: &mut Ui,
        id: u32,
        rect: (f32, f32, f32, f32),
        text: crate::widgets::Text,
        tip: Option<String>,
        name: String,
    ) {
        let p = ui.palette;
        let (text_color, hover, pressed) = (p.text, p.surface_hover, p.surface_pressed);
        ui.push(Element {
            id,
            rect,
            kind: Kind::Button(Button {
                icon: "",
                icon_size: 0.0,
                foreground: text_color,
                background: Color::default(),
                border: None,
                radius: 8.0,
                hover,
                pressed,
                enabled: true,
            }),
            tip,
            name: Some(name),
            scrolled: false,
        });
        Self::push_text(ui, rect, text, text_color, Align::Center);
    }
}

impl Content for CalendarFlyout {
    fn width(&self) -> f32 {
        WIDTH
    }

    fn build(&mut self, ui: &mut Ui) -> WinResult<()> {
        let p = ui.palette.clone();
        let (today, now) = today();

        // Header (margin 2,0,2,14): the zone docked right (10.5, margin 8,6,0,0, at most 135 wide), the live time
        // and the full date filling the rest.
        let zone = ui.text(
            &locale::time_zone_label(),
            10.5,
            Weight::Normal,
            false,
            Some(135.0),
        )?;
        let (zw, zh) = (zone.width, zone.height);
        let right = WIDTH - 2.0;
        Self::push_text(
            ui,
            (right - zw, 6.0, right, 6.0 + zh),
            zone,
            p.text_secondary,
            Align::Left,
        );
        let time = ui.text(
            &self.locale.time(&now, self.picture),
            28.0,
            Weight::Bold,
            true,
            None,
        )?;
        let th = time.height;
        Self::push_text(
            ui,
            (2.0, 0.0, right - zw - 8.0, th),
            time,
            p.text,
            Align::Left,
        );
        let date = ui.text(
            &self.locale.date_picture(&now, "dddd, d MMMM"),
            12.0,
            Weight::Normal,
            false,
            None,
        )?;
        let dh = date.height;
        Self::push_text(
            ui,
            (2.0, th + 1.0, right - zw - 8.0, th + 1.0 + dh),
            date,
            p.text_secondary,
            Align::Left,
        );
        let mut y = (th + 1.0 + dh).max(6.0 + zh) + 14.0;

        // Navigation (margin 0,0,0,10): « ‹ month › ».
        self.nav_button(ui, PREVIOUS_YEAR, 0.0, y, "ChevronsLeft", "PreviousYear");
        self.nav_button(ui, PREVIOUS_MONTH, 26.0, y, "ChevronLeft", "PreviousMonth");
        self.nav_button(ui, NEXT_MONTH, WIDTH - 52.0, y, "ChevronRight", "NextMonth");
        self.nav_button(ui, NEXT_YEAR, WIDTH - 26.0, y, "ChevronsRight", "NextYear");
        let title = title_case(
            &self
                .locale
                .date_picture(&self.shown.system_time(), "MMMM yyyy"),
            self.turkish(),
        );
        let title = ui.text(&title, 13.0, Weight::SemiBold, false, None)?;
        // Padding 8,4, centered in the middle column, as tall as the row.
        let w = title.width + 16.0;
        let center = WIDTH / 2.0;
        let go_today = self.text("GoToToday");
        Self::text_button(
            ui,
            MONTH,
            (center - w / 2.0, y, center + w / 2.0, y + 26.0),
            title,
            Some(go_today.clone()),
            go_today.clone(),
        );
        y += 26.0 + 10.0;

        // The grid (`UniformGrid`, 7 columns, margin 0,0,0,4): every cell as tall as the tallest, a day cell's
        // 31 DIP plus its 1.5 DIP margins.
        let column = WIDTH / 7.0;
        let row = 34.0;
        let first = self.first_day;
        for i in 0..7u32 {
            let text = ui.text(
                &self.day_names[i as usize],
                11.0,
                Weight::Medium,
                false,
                None,
            )?;
            let x = i as f32 * column;
            // Margin 0,0,0,6: centered in the cell above that margin.
            Self::push_text(
                ui,
                (x, y, x + column, y + row - 6.0),
                text,
                p.text_secondary,
                Align::Center,
            );
        }
        let month_first = self.shown;
        let offset = (month_first.weekday() + 7 - first) % 7;
        let start = month_first.add_days(-i64::from(offset));
        for cell in 0..42u32 {
            let date = start.add_days(i64::from(cell));
            let is_today = date == today;
            let is_selected = self.selected == Some(date) && !is_today;
            let current = date.month == self.shown.month && date.year == self.shown.year;
            let x = (cell % 7) as f32 * column;
            let top = y + row * (1 + cell / 7) as f32;
            let rect = (x + 1.5, top + 1.5, x + column - 1.5, top + 1.5 + 31.0);
            let (fill, border, ink) = if is_today {
                (
                    Some(p.selected_fill),
                    Some(p.selected_stroke),
                    p.selected_text,
                )
            } else if is_selected {
                (
                    Some(p.surface_selected_strong),
                    Some(p.stroke_subtle),
                    p.text,
                )
            } else if current {
                (None, None, p.text)
            } else {
                // `TextSecondary` at opacity 0.42.
                let c = p.text_secondary;
                (
                    None,
                    None,
                    Color {
                        A: (f32::from(c.A) * 0.42).round() as u8,
                        ..c
                    },
                )
            };
            if let Some(fill) = fill {
                ui.push(Element {
                    id: 0,
                    rect,
                    kind: Kind::Rect {
                        fill,
                        border,
                        radius: 7.0,
                    },
                    tip: None,
                    name: None,
                    scrolled: false,
                });
            }
            let weight = if is_today || is_selected {
                Weight::SemiBold
            } else {
                Weight::Normal
            };
            let label = ui.text(&date.day.to_string(), 12.0, weight, true, None)?;
            Self::push_text(ui, rect, label, ink, Align::Center);
            ui.push(Element {
                id: CELL + cell,
                rect,
                kind: Kind::Hit,
                tip: None,
                name: None,
                scrolled: false,
            });
        }
        y += 7.0 * row + 4.0;

        // Divider (margin 0,8,0,8).
        ui.push(Element {
            id: 0,
            rect: (0.0, y + 8.0, WIDTH, y + 9.0),
            kind: Kind::Fill(p.stroke_subtle),
            tip: None,
            name: None,
            scrolled: false,
        });
        y += 17.0;

        // Footer: "Go to today" docked right (11 SemiBold, padding 8,4), today's date filling the rest.
        let button_text = ui.text(&go_today, 11.0, Weight::SemiBold, false, None)?;
        let (bw, bh) = (button_text.width + 16.0, button_text.height + 8.0);
        let summary = format!(
            "{} \u{b7} {}",
            self.text("Today"),
            self.locale.date_picture(&now, "d MMMM, dddd")
        );
        let summary = ui.text(&summary, 10.5, Weight::Normal, false, Some(WIDTH - bw))?;
        let sh = summary.height;
        let height = bh.max(sh);
        Self::text_button(
            ui,
            TODAY,
            (
                WIDTH - bw,
                y + (height - bh) / 2.0,
                WIDTH,
                y + (height + bh) / 2.0,
            ),
            button_text,
            None,
            go_today,
        );
        Self::push_text(
            ui,
            (0.0, y, WIDTH - bw, y + height),
            summary,
            p.text_secondary,
            Align::Left,
        );
        Ok(())
    }

    fn click(&mut self, id: u32) {
        let (today, _) = today();
        match id {
            PREVIOUS_YEAR => self.shown = self.shown.add_months(-12),
            PREVIOUS_MONTH => self.shown = self.shown.add_months(-1),
            NEXT_MONTH => self.shown = self.shown.add_months(1),
            NEXT_YEAR => self.shown = self.shown.add_months(12),
            MONTH | TODAY => {
                self.shown = today.first_of_month();
                self.selected = Some(today);
            }
            id if (CELL..CELL + 42).contains(&id) => {
                let offset = (self.shown.weekday() + 7 - self.first_day) % 7;
                let date = self
                    .shown
                    .add_days(i64::from(id - CELL) - i64::from(offset));
                self.selected = Some(date);
                if date.month != self.shown.month || date.year != self.shown.year {
                    self.shown = date.first_of_month();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        let d = Date {
            year: 2026,
            month: 11,
            day: 24,
        };
        assert_eq!(d.weekday(), 1); // Tuesday
        assert_eq!(civil_from_days(d.days()), d);
        assert_eq!(
            d.add_days(7),
            Date {
                year: 2026,
                month: 12,
                day: 1
            }
        );
        assert_eq!(
            d.first_of_month().add_months(2),
            Date {
                year: 2027,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            d.first_of_month().add_months(-11),
            Date {
                year: 2025,
                month: 12,
                day: 1
            }
        );
        // 2024-02-29 exists; 2023-03-01 follows 2023-02-28.
        let leap = Date {
            year: 2024,
            month: 2,
            day: 29,
        };
        assert_eq!(civil_from_days(leap.days()), leap);
        assert_eq!(
            Date {
                year: 2023,
                month: 2,
                day: 28
            }
            .add_days(1),
            Date {
                year: 2023,
                month: 3,
                day: 1
            }
        );
    }

    #[test]
    fn month_titles() {
        assert_eq!(title_case("kasım 2026", true), "Kasım 2026");
        assert_eq!(title_case("ißtanbul ekim", true), "İßtanbul Ekim");
        assert_eq!(title_case("november 2026", false), "November 2026");
    }
}
