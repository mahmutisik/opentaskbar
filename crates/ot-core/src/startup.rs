//! Startup timing (`StartupStopwatch` in OpenTaskbar): elapsed-millisecond checkpoints in the log, so a slow
//! start can be attributed to a phase without a debugger.

use std::sync::OnceLock;
use std::time::Instant;

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Starts the clock. Call first thing in `main`; later calls keep the first instant.
pub fn begin() {
    STARTED.get_or_init(Instant::now);
}

/// Milliseconds since `begin` (or since the first call into this module).
pub fn elapsed_ms() -> f64 {
    STARTED.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Logs `Startup +<ms>ms: <label>`.
pub fn mark(label: &str) {
    crate::log::info(format!("Startup +{:.0}ms: {label}", elapsed_ms()));
}

/// Times one startup phase; `finish` logs `Startup phase '<label>' <message>: <ms>ms`.
pub struct Phase {
    label: &'static str,
    started: Instant,
}

impl Phase {
    pub fn start(label: &'static str) -> Self {
        mark(&format!("begin {label}"));
        Self {
            label,
            started: Instant::now(),
        }
    }

    pub fn finish(self, message: &str) {
        crate::log::info(format!(
            "Startup phase '{}' {message}: {:.0}ms",
            self.label,
            self.started.elapsed().as_secs_f64() * 1000.0
        ));
    }
}
