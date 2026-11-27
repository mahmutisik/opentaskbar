//! Cost of a log call on the calling thread: the queued logger versus writing and flushing each line under a
//! lock, which is what OpenTaskbar's C# `AppLog` does (`StreamWriter` with `AutoFlush = true`).
//! `cargo run -p ot-core --release --example log_cost`

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ot_core::log::{Level, Logger, MAX_BYTES};

const LINES: usize = 10_000;

fn main() {
    let dir = std::env::temp_dir().join(format!("ot-log-cost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let text =
        "EVENT AppBarSetPos reason=ReserveShelf ok=True verified=True shelf=58 native=0 extra=0";

    // Synchronous, flushed per line (C# behaviour).
    let file = Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("sync.txt"))
            .unwrap(),
    );
    let started = Instant::now();
    for i in 0..LINES {
        let mut file = file.lock().unwrap();
        writeln!(file, "2026-11-23 16:14:29.123 [INFO ][T 1] {text} {i}").unwrap();
        file.flush().unwrap();
    }
    let sync = started.elapsed();

    // Queued: the caller only enqueues; the writer thread batches.
    let logger = Logger::open(&dir.join("queued.txt"), MAX_BYTES * 64, false).unwrap();
    let started = Instant::now();
    for i in 0..LINES {
        logger.log(Level::Info, format!("{text} {i}"));
    }
    let queued = started.elapsed();
    logger.flush(Duration::from_secs(10));
    let drained = started.elapsed();
    logger.shutdown();

    let per = |d: Duration| d.as_secs_f64() * 1e6 / LINES as f64;
    println!(
        "sync + flush per line:  {:.2} us/line on the caller",
        per(sync)
    );
    println!(
        "queued:                  {:.2} us/line on the caller ({:.1} ms until all on disk)",
        per(queued),
        drained.as_secs_f64() * 1000.0
    );
    let _ = std::fs::remove_dir_all(&dir);
}
