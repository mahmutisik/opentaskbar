//! The System meters widget (`SystemStatsWidget`): a chip icon, then CPU, RAM, GPU and GPU temperature as
//! label-over-value columns divided by hairlines. CPU (`GetSystemTimes`) and RAM (`GlobalMemoryStatusEx`) are
//! read every second; the GPU meters come from a worker thread every 2 s and appear after two valid readings
//! (hidden again after five misses). No click action; the tooltip lists the values.

use ot_core::settings::SystemMeters;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
};
use windows::core::Result as WinResult;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::GetSystemTimes;

use super::gpu::Sensors;
use super::{Canvas, Card, Env, Painter, Widget, neutral_card, text_colors};

const PAD: f32 = 10.0;
const ICON: f32 = 15.0;
const ICON_GAP: f32 = 8.0;
/// Divider: 1 DIP wide, 18 tall, 9 DIP on each side.
const DIVIDER_GAP: f32 = 9.0;
const DIVIDER_HEIGHT: f32 = 18.0;
const LABEL_SIZE: f32 = 10.0;
const VALUE_SIZE: f32 = 12.5;
/// `SensorShowSamples` / `SensorHideMisses`.
const SHOW_AFTER: u8 = 2;
const HIDE_AFTER: u8 = 5;
const DASH: &str = "\u{2014}";

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// A GPU meter's show/hide hysteresis.
#[derive(Default)]
struct Gate {
    shown: bool,
    valid: u8,
    misses: u8,
}

impl Gate {
    fn feed(&mut self, ok: bool) {
        if ok {
            self.misses = 0;
            self.valid = (self.valid + 1).min(SHOW_AFTER);
            if self.valid >= SHOW_AFTER {
                self.shown = true;
            }
        } else {
            self.valid = 0;
            self.misses = (self.misses + 1).min(HIDE_AFTER);
            if self.misses >= HIDE_AFTER {
                self.shown = false;
            }
        }
    }
}

#[derive(Default)]
pub struct SystemStats {
    /// (idle, total) of the last CPU sample.
    cpu_base: Option<(u64, u64)>,
    cpu: Option<i64>,
    ram: Option<i64>,
    gpu: Option<i64>,
    gpu_temp: Option<i64>,
    gpu_gate: Gate,
    temp_gate: Gate,
    meters: Option<SystemMeters>,
    sensors: Option<Sensors>,
    sensor_samples: u64,
    name: String,
    tooltip: String,
}

/// `(int)Math.Round(value * 100)` (halves to even).
fn percent(value: f64) -> i64 {
    (value * 100.0).round_ties_even() as i64
}

impl SystemStats {
    fn sample_cpu(&mut self) -> Option<f64> {
        let (mut idle, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: out-parameter call.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let idle = filetime(idle);
        let total = filetime(kernel) + filetime(user);
        let usage = self.cpu_base.and_then(|(last_idle, last_total)| {
            (total > last_total).then(|| {
                (1.0 - (idle.saturating_sub(last_idle)) as f64 / (total - last_total) as f64)
                    .clamp(0.0, 1.0)
            })
        });
        self.cpu_base = Some((idle, total));
        usage
    }

    fn sample_ram() -> Option<f64> {
        // SAFETY: out-parameter call with the size set.
        unsafe {
            let mut status: MEMORYSTATUSEX = std::mem::zeroed();
            status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut status) != 0).then(|| f64::from(status.dwMemoryLoad) / 100.0)
        }
    }

    fn has(&self, m: SystemMeters) -> bool {
        self.meters.is_some_and(|all| all.bits() & m.bits() != 0)
    }

    /// The columns shown: (label, value, minimum width).
    fn columns(&self) -> Vec<(&'static str, String, f32)> {
        let text = |v: Option<i64>, unit: &str| v.map_or(DASH.to_owned(), |v| format!("{v}{unit}"));
        let mut columns = Vec::new();
        if self.has(SystemMeters::Cpu) {
            columns.push(("CPU", text(self.cpu, "%"), 34.0));
        }
        if self.has(SystemMeters::Memory) {
            columns.push(("RAM", text(self.ram, "%"), 34.0));
        }
        if self.has(SystemMeters::Gpu) && self.gpu_gate.shown {
            columns.push(("GPU", text(self.gpu, "%"), 34.0));
        }
        if self.has(SystemMeters::GpuTemperature) && self.temp_gate.shown {
            columns.push(("GPU\u{b0}", text(self.gpu_temp, "\u{b0}"), 26.0));
        }
        columns
    }
}

impl Widget for SystemStats {
    fn id(&self) -> &'static str {
        "SystemStats"
    }

    fn update(&mut self, env: &Env) -> bool {
        let meters = env.settings.stats_meters;
        if self.meters != Some(meters) {
            // `ApplyMeterSelection`: GPU meters are validated again; sensors only for the GPU meters asked for.
            self.meters = Some(meters);
            self.gpu_gate = Gate::default();
            self.temp_gate = Gate::default();
            let usage = self.has(SystemMeters::Gpu);
            let temperature = self.has(SystemMeters::GpuTemperature);
            self.sensors = (usage || temperature).then(|| Sensors::start(usage, temperature));
            self.sensor_samples = 0;
        }
        if let Some(cpu) = self.sample_cpu() {
            self.cpu = Some(percent(cpu));
        }
        if let Some(ram) = Self::sample_ram() {
            self.ram = Some(percent(ram));
        }
        if let Some(readings) = self.sensors.as_ref().map(Sensors::readings)
            && readings.samples != self.sensor_samples
        {
            self.sensor_samples = readings.samples;
            let usage = readings.usage.filter(|_| self.has(SystemMeters::Gpu));
            let celsius = readings
                .celsius
                .filter(|_| self.has(SystemMeters::GpuTemperature));
            self.gpu_gate.feed(usage.is_some());
            self.temp_gate.feed(celsius.is_some());
            if let Some(u) = usage {
                self.gpu = Some(percent(u));
            }
            if let Some(c) = celsius {
                self.gpu_temp = Some(c.round_ties_even() as i64);
            }
        }
        // "CPU 4% · RAM 39% · GPU 0% · GPU 48°C".
        let mut parts = Vec::new();
        if let Some(v) = self.cpu {
            parts.push(format!("CPU {v}%"));
        }
        if let Some(v) = self.ram {
            parts.push(format!("RAM {v}%"));
        }
        if self.gpu_gate.shown
            && let Some(v) = self.gpu
        {
            parts.push(format!("GPU {v}%"));
        }
        if self.temp_gate.shown
            && let Some(v) = self.gpu_temp
        {
            parts.push(format!("GPU {v}\u{b0}C"));
        }
        let name = env.text("WidgetSystemStats");
        let tooltip = if parts.is_empty() {
            name.clone()
        } else {
            parts.join(" \u{b7} ")
        };
        let changed = tooltip != self.tooltip || name != self.name;
        self.tooltip = tooltip;
        self.name = name;
        changed
    }

    fn width(&self, painter: &Painter) -> WinResult<f32> {
        let mut width = PAD + ICON + ICON_GAP;
        for (i, (label, value, min)) in self.columns().iter().enumerate() {
            if i > 0 {
                width += DIVIDER_GAP * 2.0 + 1.0;
            }
            let l = painter
                .text(label, LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL, false)?
                .width;
            let v = painter
                .text(value, VALUE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?
                .width;
            width += l.max(v).max(*min);
        }
        Ok((width + PAD).ceil())
    }

    fn draw(&self, canvas: &Canvas) -> WinResult<()> {
        let p = canvas.painter;
        let (primary, secondary) = text_colors(canvas.dark);
        let stroke = crate::theme::stroke_subtle(canvas.dark);
        let h = canvas.height;
        let mut x = PAD;
        canvas.icon("Cpu", x, (h - ICON) / 2.0, ICON, secondary)?;
        x += ICON + ICON_GAP;
        for (i, (label, value, min)) in self.columns().iter().enumerate() {
            if i > 0 {
                x += DIVIDER_GAP;
                canvas.vline(x, (h - DIVIDER_HEIGHT) / 2.0, DIVIDER_HEIGHT, stroke)?;
                x += 1.0 + DIVIDER_GAP;
            }
            let l = p.text(label, LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL, false)?;
            let v = p.text(value, VALUE_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
            let top = (h - l.height - v.height) / 2.0;
            canvas.text(&l, x, top, secondary)?;
            canvas.text(&v, x, top + l.height, primary)?;
            x += l.width.max(v.width).max(*min);
        }
        Ok(())
    }

    fn card(&self, dark: bool) -> Card {
        neutral_card(dark)
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
    fn gpu_meters_need_two_readings_and_five_misses() {
        let mut gate = Gate::default();
        gate.feed(true);
        assert!(!gate.shown);
        gate.feed(true);
        assert!(gate.shown);
        for _ in 0..4 {
            gate.feed(false);
        }
        assert!(gate.shown);
        gate.feed(false);
        assert!(!gate.shown);
    }

    #[test]
    fn rounds_like_dotnet() {
        assert_eq!(percent(0.125), 12);
        assert_eq!(percent(0.135), 14);
    }
}
