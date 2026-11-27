//! Application log (`AppLog` in OpenTaskbar): `log.txt` next to the settings, level-tagged timestamped lines,
//! rotation to `log.old.txt` at 1 MB, UTF-8 with a BOM on new files so Notepad shows Turkish text correctly.
//!
//! Unlike the C# logger, callers never touch the disk: a line is queued (bounded) and a dedicated thread
//! batches the writes. When the queue is full the line is dropped and counted rather than blocking the UI
//! thread; the count is written as soon as there is room. `flush()` waits until everything queued is on disk.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

pub const MAX_BYTES: u64 = 1024 * 1024;
const QUEUE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

enum Message {
    Line {
        level: Level,
        time: LocalTime,
        thread: u32,
        text: String,
    },
    Flush(SyncSender<()>),
}

/// A log file with its writer thread. The process normally uses the global one (`init`, `info`, ...).
pub struct Logger {
    sender: Mutex<Option<SyncSender<Message>>>,
    verbose: bool,
    dropped: AtomicU64,
    path: PathBuf,
}

impl Logger {
    /// Opens (or creates) the log file and starts the writer thread. A file that cannot be opened yet (its folder
    /// is being deleted, another process holds it) does not lose the log: lines are kept in memory and the file is
    /// opened again every few seconds.
    pub fn open(path: &Path, max_bytes: u64, verbose: bool) -> io::Result<Self> {
        let writer = open_writer(path, max_bytes);
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let file = path.to_path_buf();
        thread::Builder::new()
            .name("ot-log".into())
            .spawn(move || run_writer(writer, file, max_bytes, receiver))?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            verbose,
            dropped: AtomicU64::new(0),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn log(&self, level: Level, text: impl Into<String>) {
        if level == Level::Debug && !self.verbose {
            return;
        }
        let Ok(guard) = self.sender.lock() else {
            return;
        };
        let Some(sender) = guard.as_ref() else { return };
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            let note = Message::Line {
                level: Level::Warn,
                time: LocalTime::now(),
                thread: thread_number(),
                text: format!("{dropped} log lines were dropped because the log queue was full."),
            };
            if sender.try_send(note).is_ok() {
                self.dropped.fetch_sub(dropped, Ordering::Relaxed);
            }
        }
        let line = Message::Line {
            level,
            time: LocalTime::now(),
            thread: thread_number(),
            text: text.into(),
        };
        if let Err(TrySendError::Full(_)) = sender.try_send(line) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Waits up to `timeout` until every queued line is written and flushed to the file.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (done, wait) = mpsc::sync_channel(1);
        let sent = self
            .sender
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|sender| sender.send(Message::Flush(done)).is_ok())
            })
            .unwrap_or(false);
        sent && wait.recv_timeout(timeout).is_ok()
    }

    /// Flushes and stops the writer thread; later calls are ignored.
    pub fn shutdown(&self) {
        self.flush(Duration::from_secs(2));
        if let Ok(mut guard) = self.sender.lock() {
            guard.take();
        }
    }
}

struct FileWriter {
    path: PathBuf,
    max_bytes: u64,
    out: Option<BufWriter<File>>,
    size: u64,
}

impl FileWriter {
    fn open(path: PathBuf, max_bytes: u64) -> io::Result<Self> {
        let (out, size) = open_log_file(&path)?;
        Ok(Self {
            path,
            max_bytes,
            out: Some(out),
            size,
        })
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.size + line.len() as u64 > self.max_bytes && self.size > 3 {
            self.rotate();
        }
        let out = self
            .out
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is closed"))?;
        out.write_all(line.as_bytes())?;
        self.size += line.len() as u64;
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(out) = self.out.as_mut() {
            let _ = out.flush();
        }
    }

    /// Moves log.txt to log.old.txt and starts a new file. The size is tracked in memory, so unlike the C#
    /// logger no file metadata is read per line. If the move fails, writing continues in the current file.
    fn rotate(&mut self) {
        if let Some(mut out) = self.out.take() {
            let _ = out.flush();
        } // the handle is closed here, so the file can be moved
        let old = self.path.with_extension("old.txt");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&self.path, &old);
        if let Ok((out, size)) = open_log_file(&self.path) {
            self.out = Some(out);
            self.size = size;
        }
    }
}

/// Opens the log for appending; a new (empty) file starts with a UTF-8 BOM.
fn open_log_file(path: &Path) -> io::Result<(BufWriter<File>, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut size = file.metadata()?.len();
    let mut out = BufWriter::with_capacity(16 * 1024, file);
    if size == 0 {
        out.write_all("\u{feff}".as_bytes())?;
        size = 3;
    }
    Ok((out, size))
}

fn open_writer(path: &Path, max_bytes: u64) -> Option<FileWriter> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    FileWriter::open(path.to_path_buf(), max_bytes).ok()
}

/// How often a log file that could not be opened is tried again.
const REOPEN_INTERVAL: Duration = Duration::from_secs(5);

fn run_writer(
    mut writer: Option<FileWriter>,
    path: PathBuf,
    max_bytes: u64,
    receiver: Receiver<Message>,
) {
    let mut line = String::with_capacity(256);
    // Lines formatted while the file is unavailable, oldest first; bounded like the queue.
    let mut held: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut last_attempt = std::time::Instant::now();
    loop {
        let first = match receiver.recv_timeout(Duration::from_secs(60)) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => {
                if writer.is_none() && !held.is_empty() {
                    writer = open_writer(&path, max_bytes);
                    last_attempt = std::time::Instant::now();
                    write_held(&mut writer, &mut held);
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if writer.is_none() && last_attempt.elapsed() >= REOPEN_INTERVAL {
            writer = open_writer(&path, max_bytes);
            last_attempt = std::time::Instant::now();
            write_held(&mut writer, &mut held);
        }
        // Write everything already queued, then flush once.
        let mut pending_flushes = Vec::new();
        let mut next = Some(first);
        while let Some(message) = next {
            match message {
                Message::Line {
                    level,
                    time,
                    thread,
                    text,
                } => {
                    line.clear();
                    format_line(&mut line, level, &time, thread, &text);
                    // Logging must never take the app down; a failed write is skipped.
                    match writer.as_mut() {
                        Some(file) => {
                            let _ = file.write_line(&line);
                        }
                        None => {
                            if held.len() >= QUEUE_CAPACITY {
                                held.pop_front();
                            }
                            held.push_back(line.clone());
                        }
                    }
                }
                Message::Flush(done) => pending_flushes.push(done),
            }
            next = receiver.try_recv().ok();
        }
        if let Some(file) = writer.as_mut() {
            file.flush();
        }
        for done in pending_flushes {
            let _ = done.send(());
        }
    }
    if writer.is_none() && !held.is_empty() {
        writer = open_writer(&path, max_bytes);
        write_held(&mut writer, &mut held);
    }
    if let Some(file) = writer.as_mut() {
        file.flush();
    }
}

fn write_held(writer: &mut Option<FileWriter>, held: &mut std::collections::VecDeque<String>) {
    if let Some(file) = writer.as_mut() {
        while let Some(line) = held.pop_front() {
            let _ = file.write_line(&line);
        }
        file.flush();
    }
}

/// `2026-11-23 16:14:29.123 [INFO ][T 1] message`, multi-line messages kept as-is, as in OpenTaskbar.
fn format_line(out: &mut String, level: Level, time: &LocalTime, thread: u32, text: &str) {
    let _ = write!(
        out,
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{:<5}][T{:>2}] {}\r\n",
        time.year,
        time.month,
        time.day,
        time.hour,
        time.minute,
        time.second,
        time.millis,
        level.tag(),
        thread,
        text.replace('\n', "\r\n").replace("\r\r\n", "\r\n"),
    );
}

/// Small per-process thread numbers (1 = first thread that logs), like .NET managed thread IDs in the C# log.
fn thread_number() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    thread_local!(static ID: u32 = NEXT.fetch_add(1, Ordering::Relaxed));
    ID.with(|id| *id)
}

#[derive(Clone, Copy, Debug)]
struct LocalTime {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

impl LocalTime {
    #[cfg(windows)]
    fn now() -> Self {
        #[repr(C)]
        struct SystemTime([u16; 8]);
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLocalTime(time: *mut SystemTime);
        }
        let mut st = SystemTime([0; 8]);
        // SAFETY: GetLocalTime fills the 16-byte SYSTEMTIME it is given.
        unsafe { GetLocalTime(&mut st) };
        let [year, month, _weekday, day, hour, minute, second, millis] = st.0;
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millis,
        }
    }

    #[cfg(not(windows))]
    fn now() -> Self {
        // UTC fallback for non-Windows builds of the tests.
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = since.as_secs();
        let (days, rem) = (secs / 86_400, secs % 86_400);
        let (year, month, day) = civil_from_days(days as i64);
        Self {
            year: year as u16,
            month: month as u16,
            day: day as u16,
            hour: (rem / 3600) as u16,
            minute: (rem / 60 % 60) as u16,
            second: (rem % 60) as u16,
            millis: since.subsec_millis() as u16,
        }
    }
}

#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

// ---- Process-wide logger ----

static GLOBAL: OnceLock<Logger> = OnceLock::new();
static INIT_FAILED: AtomicBool = AtomicBool::new(false);

/// Starts the process log at `path` (normally `%LOCALAPPDATA%\OpenTaskBar-Rust\log.txt`). Debug lines are
/// written only when `OT_VERBOSE_LOG=1`. Safe to call more than once; only the first call opens the file.
pub fn init(path: &Path) -> bool {
    if GLOBAL.get().is_some() {
        return true;
    }
    let verbose = std::env::var("OT_VERBOSE_LOG").is_ok_and(|v| v == "1");
    match Logger::open(path, MAX_BYTES, verbose) {
        Ok(logger) => {
            let _ = GLOBAL.set(logger);
            true
        }
        Err(_) => {
            INIT_FAILED.store(true, Ordering::Relaxed);
            false
        }
    }
}

pub fn global() -> Option<&'static Logger> {
    GLOBAL.get()
}

pub fn info(text: impl Into<String>) {
    if let Some(logger) = GLOBAL.get() {
        logger.log(Level::Info, text);
    }
}

pub fn warn(text: impl Into<String>) {
    if let Some(logger) = GLOBAL.get() {
        logger.log(Level::Warn, text);
    }
}

pub fn error(text: impl Into<String>) {
    if let Some(logger) = GLOBAL.get() {
        logger.log(Level::Error, text);
    }
}

/// Debug lines are built only when verbose logging is on.
pub fn debug(text: impl FnOnce() -> String) {
    if let Some(logger) = GLOBAL.get().filter(|logger| logger.verbose) {
        logger.log(Level::Debug, text());
    }
}

/// A failure record: localization key, resolved message and optional details (`AppLog.ReportError`).
pub fn report_error(key: &str, language: &crate::AppLanguage, details: Option<&str>) {
    let mut text = format!(
        "Failure key={key} | message={}",
        crate::Catalog::get(key, language)
    );
    if let Some(details) = details {
        text.push_str("\n  details=");
        text.push_str(details);
    }
    error(text);
}

/// A structured shell event with `key=value` fields (`ShellTelemetry.Event`).
pub fn event(name: &str, fields: &[(&str, &dyn std::fmt::Display)]) {
    if GLOBAL.get().is_none() {
        return;
    }
    let mut text = format!("EVENT {name}");
    for (key, value) in fields {
        let _ = write!(text, " {key}={value}");
    }
    info(text);
}

/// Process-local correlation sequence for shell events (never 0).
pub fn next_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn flush(timeout: Duration) -> bool {
    GLOBAL.get().is_some_and(|logger| logger.flush(timeout))
}

pub fn shutdown() {
    if let Some(logger) = GLOBAL.get() {
        logger.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ot-log-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("log.txt")
    }

    #[test]
    fn writes_formatted_lines_with_bom() {
        let path = temp("format");
        let logger = Logger::open(&path, MAX_BYTES, false).unwrap();
        logger.log(Level::Info, "merhaba dünya");
        logger.log(Level::Debug, "hidden");
        logger.log(Level::Error, "two\nlines");
        assert!(logger.flush(Duration::from_secs(2)));
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with("\u{feff}".as_bytes()));
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(
            lines[0].contains(" [INFO ][T") && lines[0].ends_with("] merhaba dünya"),
            "{}",
            lines[0]
        );
        assert_eq!(&lines[0][4..5], "-");
        assert_eq!(&lines[0][23..24], " ");
        assert!(lines[1].contains("[ERROR]") && lines[1].ends_with("two"));
        assert_eq!(lines[2], "lines");
        assert!(!text.contains("hidden"));
        logger.shutdown();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn keeps_lines_until_the_file_can_be_opened() {
        // The log folder's name is taken by a file, so the log cannot be created at first.
        let path = temp("unavailable");
        let folder = path.parent().unwrap().to_path_buf();
        let _ = fs::remove_file(&folder);
        fs::write(&folder, b"blocker").unwrap();
        let logger = Logger::open(&path, MAX_BYTES, false).unwrap();
        logger.log(Level::Info, "kept while the file was missing");
        assert!(logger.flush(Duration::from_secs(2)));
        assert!(!path.exists());
        // The folder becomes usable; the next attempt (here the one at shutdown) writes the kept lines.
        fs::remove_file(&folder).unwrap();
        logger.shutdown();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            text = fs::read_to_string(&path).unwrap_or_default();
            if !text.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(text.contains("kept while the file was missing"), "{text}");
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn rotates_when_full() {
        let path = temp("rotate");
        let logger = Logger::open(&path, 300, true).unwrap();
        for i in 0..20 {
            logger.log(Level::Info, format!("line {i:02} {}", "x".repeat(20)));
        }
        assert!(logger.flush(Duration::from_secs(2)));
        let old = path.with_extension("old.txt");
        assert!(old.exists(), "log.old.txt created");
        assert!(fs::metadata(&path).unwrap().len() <= 300);
        let current = fs::read_to_string(&path).unwrap();
        assert!(current.starts_with('\u{feff}') && current.contains("line 19"));
        logger.shutdown();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn appends_to_existing_file_without_bom() {
        let path = temp("append");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "old\r\n").unwrap();
        let logger = Logger::open(&path, MAX_BYTES, false).unwrap();
        logger.log(Level::Warn, "new");
        assert!(logger.flush(Duration::from_secs(2)));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("old\r\n") && text.contains("[WARN ]"));
        logger.shutdown();
        logger.log(Level::Info, "after shutdown is ignored");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
