//! The Claude Code usage widget (`AiUsageWidget` for `AiProviders.Claude`): the Claude logo and two rows, the
//! five-hour session and the weekly quota, each a short label, a mini bar in the quota's accent (warning above
//! 75 %, red above 90 % or when the server flags it) and the reset time. Loads run on a worker thread: every
//! 5–5.5 minutes, with OpenTaskbar's backoff after errors and a wait for a new sign-in after a 401.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ot_render::{Color, color};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
};
use windows::core::Result as WinResult;

use super::claude_api::{self, Outcome, Report, Window, now_unix};
use super::{Canvas, Card, Env, Fill, Painter, Widget, d2d, text_colors, wake};

const PAD: f32 = 8.0;
const LOGO: f32 = 16.0;
const LOGO_GAP: f32 = 6.0;
const BARS_LEFT: f32 = 2.0;
const ROW: f32 = 12.0;
const ROW_GAP: f32 = 2.0;
const SESSION_LABEL: f32 = 18.0;
const WEEKLY_LABEL: f32 = 22.0;
const BAR: f32 = 38.0;
const BAR_LEFT: f32 = 3.0;
const BAR_RIGHT: f32 = 5.0;
const LABEL_SIZE: f32 = 9.5;
const TEXT_SIZE: f32 = 9.0;
/// `ClaudeBrandBrush`.
const BRAND: Color = color(0xFF, 0xD9, 0x77, 0x57);
/// A report older than this is shown as out of date.
const STALE_SECONDS: i64 = 600;
const DASH: &str = "\u{2014}";

/// One drawn row: label, used fraction (none: no window), accent, reset text.
#[derive(Clone, PartialEq)]
struct Row {
    label: String,
    label_width: f32,
    used: Option<f64>,
    accent: Color,
    text: String,
}

#[derive(Default)]
pub struct ClaudeUsage {
    report: Option<Report>,
    error: Option<&'static str>,
    failures: u32,
    next_load: Option<Instant>,
    sign_in_stamp: Option<i64>,
    pending: Option<Arc<Mutex<Option<Outcome>>>>,
    rows: Vec<Row>,
    stale: bool,
    tooltip: String,
    name: String,
}

/// `Accent(value, severity)`: 0 normal, 1 warning, 2 critical.
fn level(used: f64, severity: Option<&str>) -> u8 {
    let mut l = if used >= 0.9 {
        2
    } else if used >= 0.75 {
        1
    } else {
        0
    };
    if let Some(s) = severity.map(str::to_lowercase).filter(|s| s != "normal") {
        if s.contains("crit") || s.contains("exceed") || s.contains("block") {
            l = 2;
        } else if s.contains("warn") {
            l = l.max(1);
        }
    }
    l
}

fn accent(level: u8, dark: bool) -> Color {
    match (level, dark) {
        // `WidgetFocusAccent`, `WidgetWarning`, `WidgetAccent` (the Windows accent when Windows has one).
        (2, true) => color(0xFF, 0xEF, 0x44, 0x44),
        (2, false) => color(0xFF, 0xDC, 0x26, 0x26),
        (1, true) => color(0xFF, 0xFB, 0xBF, 0x24),
        (1, false) => color(0xFF, 0xD9, 0x77, 0x06),
        (_, dark) if crate::tokens::color_of("WidgetAccent", dark).is_some() => {
            crate::tokens::color_of("WidgetAccent", dark).unwrap_or_default()
        }
        (_, true) => crate::theme::windows_accent().unwrap_or(color(0xFF, 0x2D, 0xD4, 0xBF)),
        (_, false) => crate::theme::windows_accent().unwrap_or(color(0xFF, 0x0D, 0x94, 0x88)),
    }
}

/// `TextMuted`.
const MUTED: Color = color(0x80, 0x7C, 0x86, 0x98);

/// `Duration`: "2 sa 3 dk", "45 dk".
fn duration(env: &Env, seconds: i64) -> String {
    let minutes = ((seconds.max(0) as f64) / 60.0).ceil().max(1.0) as i64;
    if minutes < 60 {
        env.text("MinutesFormat")
            .replace("{0}", &minutes.to_string())
    } else {
        env.text("HoursMinutesFormat")
            .replace("{0}", &(minutes / 60).to_string())
            .replace("{1}", &(minutes % 60).to_string())
    }
}

/// `When`: "Pzt 03:00" within six days, else the short date and time.
fn when(env: &Env, at: i64) -> String {
    let local = crate::locale::local_from_unix(at);
    let time = env.locale.short_time(&local);
    if at - now_unix() < 6 * 86_400 {
        format!("{} {}", env.locale.date_picture(&local, "ddd"), time)
    } else {
        format!("{} {}", env.locale.short_date(&local), time)
    }
}

impl ClaudeUsage {
    /// Picks up a finished load and schedules the next one (`LoadAsync`'s success and error paths).
    fn collect(&mut self) {
        let Some(slot) = &self.pending else {
            return;
        };
        let Some(outcome) = slot.lock().ok().and_then(|mut o| o.take()) else {
            return;
        };
        self.pending = None;
        let now = Instant::now();
        match outcome {
            Outcome::Report(report) => {
                self.report = Some(report);
                self.error = None;
                self.failures = 0;
                self.sign_in_stamp = None;
                // 300–330 s.
                let jitter = u64::from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.subsec_nanos())
                        % 31,
                );
                self.next_load = Some(now + Duration::from_secs(300 + jitter));
            }
            Outcome::Error { key, retry_after } => {
                self.error = Some(key);
                self.failures = (self.failures + 1).min(10);
                if key == "ErrorAiUsageSignIn" {
                    self.sign_in_stamp = Some(claude_api::credential_stamp());
                }
                let wait = match retry_after {
                    Some(d) => d.max(Duration::from_secs(1)),
                    None => {
                        let base = if key == "ErrorAiUsageRateLimited" {
                            300
                        } else {
                            120
                        };
                        let factor = 1u64 << (self.failures.saturating_sub(1)).min(10);
                        Duration::from_secs((base * factor).min(1800))
                    }
                };
                self.next_load = Some(now + wait);
            }
        }
    }

    fn start_load(&mut self) {
        // After a sign-in error, wait until the credential changes (`_signInStamp`).
        if let Some(stamp) = self.sign_in_stamp {
            if claude_api::credential_stamp() == stamp {
                return;
            }
            self.sign_in_stamp = None;
            self.next_load = None;
        }
        if self.pending.is_some() || self.next_load.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let slot = Arc::new(Mutex::new(None));
        self.pending = Some(Arc::clone(&slot));
        let spawned = std::thread::Builder::new()
            .name("claude-usage".into())
            .spawn(move || {
                let outcome = claude_api::load();
                if let Ok(mut s) = slot.lock() {
                    *s = Some(outcome);
                }
                wake();
            });
        if spawned.is_err() {
            self.pending = None;
        }
    }

    fn error_text(&self, env: &Env) -> Option<String> {
        self.error
            .map(|key| env.text(key).replace("{0}", "claude auth login"))
    }

    fn window<'a>(&'a self, key: &str) -> Option<&'a Window> {
        self.report
            .as_ref()?
            .windows
            .iter()
            .find(|w| w.label_key == key)
    }

    fn label(env: &Env, key: &str) -> String {
        env.text(key)
    }
}

impl Widget for ClaudeUsage {
    fn id(&self) -> &'static str {
        "AiUsageClaude"
    }

    fn update(&mut self, env: &Env) -> bool {
        self.collect();
        self.start_load();
        let dark = crate::theme::tone(env.settings.theme) == crate::theme::Tone::Dark;
        let now = now_unix();
        let stale = self
            .report
            .as_ref()
            .is_some_and(|r| now - r.fetched_at > STALE_SECONDS);
        let session = self.window("AiUsageSession").cloned();
        let long = self
            .window("AiUsageWeekly")
            .or_else(|| self.window("AiUsageMonthly"))
            .cloned();
        let long_short = if long
            .as_ref()
            .is_some_and(|w| w.label_key == "AiUsageMonthly")
        {
            "AiUsageMonthlyShort"
        } else {
            "AiUsageWeeklyShort"
        };
        // `AiUsageRing` Highest (default): both rows; with no data both rows show a dash.
        let show_session = session.is_some() || long.is_none();
        let show_long = long.is_some() || session.is_none();
        let row = |w: &Option<Window>, label_key: &str, width: f32, is_session: bool| Row {
            label: env.text(label_key),
            label_width: width,
            used: w.as_ref().map(|w| w.used),
            accent: w.as_ref().map_or(MUTED, |w| {
                accent(level(w.used, w.severity.as_deref()), dark)
            }),
            text: match w.as_ref().and_then(|w| w.resets_at) {
                Some(at) if is_session => duration(env, at - now),
                Some(at) => when(env, at),
                None => DASH.to_owned(),
            },
        };
        let mut rows = Vec::new();
        if show_session {
            rows.push(row(&session, "AiUsageSessionShort", SESSION_LABEL, true));
        }
        if show_long {
            rows.push(row(&long, long_short, WEEKLY_LABEL, false));
        }
        // `Summary()`.
        let main_windows: Vec<&Window> = self
            .report
            .iter()
            .flat_map(|r| r.windows.iter())
            .filter(|w| !w.label_key.starts_with("AiUsageWeeklyModel"))
            .collect();
        let main = main_windows
            .iter()
            .copied()
            .max_by(|a, b| a.used.total_cmp(&b.used))
            .or_else(|| self.report.as_ref().and_then(|r| r.windows.first()));
        let tooltip = match main {
            None => match self.error_text(env) {
                Some(e) => format!("Claude Code: {e}"),
                None => "Claude Code".to_owned(),
            },
            Some(main) => {
                let mut parts = vec![format!(
                    "Claude Code {}",
                    env.percent((main.used * 100.0).round_ties_even() as i64)
                )];
                for w in main_windows.iter().filter(|w| !std::ptr::eq(**w, main)) {
                    parts.push(format!(
                        "{} {}",
                        Self::label(env, &w.label_key),
                        env.percent((w.used * 100.0).round_ties_even() as i64)
                    ));
                }
                if main_windows.iter().any(|w| w.used >= 1.0) {
                    parts.push(env.text("AiUsageLimitReached"));
                } else if let Some(at) = main.resets_at {
                    let remaining = at - now;
                    parts.push(if remaining < 6 * 3600 {
                        env.text("AiUsageResetsIn")
                            .replace("{0}", &duration(env, remaining))
                    } else {
                        env.text("AiUsageResetsAt").replace("{0}", &when(env, at))
                    });
                }
                if let Some(e) = self.error_text(env) {
                    parts.push(e);
                }
                parts.join(" \u{b7} ")
            }
        };
        let name = format!("{}: {}", env.text("WidgetAiUsage"), tooltip);
        let changed = rows != self.rows
            || stale != self.stale
            || tooltip != self.tooltip
            || name != self.name;
        self.rows = rows;
        self.stale = stale;
        self.tooltip = tooltip;
        self.name = name;
        changed
    }

    fn width(&self, painter: &Painter) -> WinResult<f32> {
        let mut panel: f32 = 0.0;
        for row in &self.rows {
            let text = painter.text(&row.text, TEXT_SIZE, DWRITE_FONT_WEIGHT_NORMAL, true)?;
            panel = panel.max(row.label_width + BAR_LEFT + BAR + BAR_RIGHT + text.width);
        }
        Ok((PAD + LOGO + LOGO_GAP + BARS_LEFT + panel + PAD).ceil())
    }

    fn draw(&self, canvas: &Canvas) -> WinResult<()> {
        let p = canvas.painter;
        let (primary, secondary) = text_colors(canvas.dark);
        let h = canvas.height;
        canvas.glyph(
            "LogoClaude",
            PAD,
            (h - LOGO) / 2.0,
            LOGO,
            if self.stale { secondary } else { BRAND },
        )?;
        let x0 = PAD + LOGO + LOGO_GAP + BARS_LEFT;
        let n = self.rows.len() as f32;
        let panel = n * ROW + (n - 1.0).max(0.0) * ROW_GAP;
        let mut top = (h - panel) / 2.0;
        for row in &self.rows {
            let label = p.text(&row.label, LABEL_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
            canvas.text(&label, x0, top + (ROW - label.height) / 2.0, secondary)?;
            let bx = canvas.snap(x0 + row.label_width + BAR_LEFT);
            let by = canvas.snap(top + (ROW - 4.0) / 2.0);
            let bar = |width: f32, ink: Color| -> WinResult<()> {
                if width <= 0.0 {
                    return Ok(());
                }
                // SAFETY: drawing inside BeginDraw/EndDraw.
                unsafe {
                    let brush = canvas.context.CreateSolidColorBrush(&d2d(ink), None)?;
                    canvas.context.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: bx,
                                top: by,
                                right: bx + width,
                                bottom: by + 4.0,
                            },
                            radiusX: 2.0,
                            radiusY: 2.0,
                        },
                        &brush,
                    );
                }
                Ok(())
            };
            // Track: `TextMuted` at opacity 0.35.
            bar(
                BAR,
                color(
                    (f32::from(MUTED.A) * 0.35).round() as u8,
                    MUTED.R,
                    MUTED.G,
                    MUTED.B,
                ),
            )?;
            bar(
                (BAR * row.used.unwrap_or(0.0) as f32).clamp(0.0, BAR),
                row.accent,
            )?;
            let text = p.text(&row.text, TEXT_SIZE, DWRITE_FONT_WEIGHT_NORMAL, true)?;
            canvas.text(
                &text,
                x0 + row.label_width + BAR_LEFT + BAR + BAR_RIGHT,
                top + (ROW - text.height) / 2.0,
                if self.stale { secondary } else { primary },
            )?;
            top += ROW + ROW_GAP;
        }
        Ok(())
    }

    fn card(&self, _dark: bool) -> Card {
        // Tinted with Claude's brand color (the user's colored cards).
        Card {
            fill: Fill::Diagonal(color(0x30, 0xD9, 0x77, 0x57), color(0x0E, 0xD9, 0x77, 0x57)),
            stroke: color(0x46, 0xD9, 0x77, 0x57),
        }
    }

    fn tooltip(&self) -> Option<String> {
        Some(self.tooltip.clone())
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_follow_usage_and_severity() {
        assert_eq!(level(0.5, None), 0);
        assert_eq!(level(0.8, None), 1);
        assert_eq!(level(0.95, None), 2);
        assert_eq!(level(0.1, Some("warning")), 1);
        assert_eq!(level(0.1, Some("Critical")), 2);
        assert_eq!(level(0.8, Some("normal")), 1);
    }
}
