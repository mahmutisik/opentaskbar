//! Icon extraction off the UI thread and, when enabled, out of the dock's process. Shell icon extraction loads the
//! Shell's image pipeline and whatever shell extensions an app registers: in a short-lived worker process (the same
//! executable with `--extract-icons`, talking over stdin/stdout) that memory leaves with the worker a few seconds
//! after the last request, and a crashing or hanging extension costs one icon instead of the dock. Results are
//! cached per (parsing name, size) for the process and, for icons of files, on disk across starts (invalidated when
//! the file's size or modification time changes). Without a worker (disabled, or it cannot start) icons are
//! extracted on the loader thread as before.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use ot_core::log;

use crate::icons::{self, Icon};

pub type IconKey = (String, u32);
type Finished = Arc<Mutex<Vec<(IconKey, Option<Arc<Icon>>)>>>;

/// The worker's command-line flag.
pub const WORKER_FLAG: &str = "--extract-icons";
/// The worker exits this long after its last request.
const WORKER_IDLE: Duration = Duration::from_secs(4);
/// One icon taking longer than this means a hung extension: the worker is ended and the icon skipped.
const WORKER_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest icon side accepted from the worker or the disk cache.
const MAX_SIDE: u32 = 512;
const CACHE_MAGIC: &[u8; 8] = b"OTICON01";

static WORKER_EXE: OnceLock<PathBuf> = OnceLock::new();

/// Lets the loader run extraction in worker processes started from `exe` (the app's own executable). Call before the
/// dock starts; without it extraction stays in this process.
pub fn enable_worker(exe: PathBuf) {
    let _ = WORKER_EXE.set(exe);
}

pub struct IconLoader {
    requests: Sender<IconKey>,
    results: Finished,
    cache: HashMap<IconKey, Option<Arc<Icon>>>,
    pending: Vec<IconKey>,
}

impl IconLoader {
    /// `notify` runs on the loader thread after each result (e.g. posts a message to the dock window).
    pub fn new(notify: impl Fn() + Send + 'static) -> Self {
        let (requests, receiver) = mpsc::channel::<IconKey>();
        let results: Finished = Arc::default();
        let sink = results.clone();
        std::thread::Builder::new()
            .name("ot-icons".into())
            .spawn(move || loader_thread(&receiver, &sink, &notify))
            .expect("icon thread");
        Self {
            requests,
            results,
            cache: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// The icon if loaded; otherwise requests it once and returns `None` for now.
    pub fn get(&mut self, parsing_name: &str, size: u32) -> Option<Arc<Icon>> {
        let key = (parsing_name.to_owned(), size);
        if let Some(icon) = self.cache.get(&key) {
            return icon.clone();
        }
        if !self.pending.contains(&key) {
            self.pending.push(key.clone());
            let _ = self.requests.send(key);
        }
        None
    }

    /// Moves finished results into the cache; returns true when something new arrived.
    pub fn drain(&mut self) -> bool {
        let finished: Vec<_> = self
            .results
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default();
        let any = !finished.is_empty();
        for (key, icon) in finished {
            self.pending.retain(|k| k != &key);
            self.cache.insert(key, icon);
        }
        any
    }
}

fn loader_thread(receiver: &Receiver<IconKey>, sink: &Finished, notify: &dyn Fn()) {
    let mut worker: Option<Worker> = None;
    // Set once a worker cannot be started: extraction then stays on this thread (COM initialized on first use).
    let mut in_process = WORKER_EXE.get().is_none();
    let mut com = false;
    loop {
        let key = if worker.is_some() {
            match receiver.recv_timeout(WORKER_IDLE) {
                Ok(key) => key,
                Err(RecvTimeoutError::Timeout) => {
                    // Idle: the worker and everything the Shell loaded into it go away.
                    worker = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match receiver.recv() {
                Ok(key) => key,
                Err(_) => break,
            }
        };
        let cached = read_cached(&key);
        let icon = match cached {
            Some(icon) => Some(icon),
            None => {
                let extracted = if in_process {
                    if !com {
                        // SAFETY: COM for the Shell image factory on this thread.
                        let _ = unsafe {
                            windows::Win32::System::Com::CoInitializeEx(
                                None,
                                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                            )
                        };
                        com = true;
                    }
                    icons::load(&key.0, key.1)
                } else {
                    match extract_with_worker(&mut worker, &key) {
                        Ok(icon) => icon,
                        Err(WorkerError::Unavailable) => {
                            in_process = true;
                            log::warn("Icon worker unavailable; extracting icons in the dock.");
                            // SAFETY: as above.
                            let _ = unsafe {
                                windows::Win32::System::Com::CoInitializeEx(
                                    None,
                                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                                )
                            };
                            com = true;
                            icons::load(&key.0, key.1)
                        }
                        Err(_) => None,
                    }
                };
                if let Some(icon) = &extracted {
                    write_cached(&key, icon);
                }
                extracted
            }
        };
        if let Ok(mut sink) = sink.lock() {
            sink.push((key, icon.map(Arc::new)));
        }
        notify();
    }
}

#[derive(Debug)]
enum WorkerError {
    /// The worker process cannot be started at all.
    Unavailable,
    /// It ended or stopped answering; the next request starts a new one.
    Failed,
}

/// Extracts `key` in the worker, starting one when needed. A worker that died is replaced once; one that hangs on
/// this icon is ended and the icon skipped.
fn extract_with_worker(
    slot: &mut Option<Worker>,
    key: &IconKey,
) -> Result<Option<Icon>, WorkerError> {
    for attempt in 0..2 {
        if slot.is_none() {
            *slot = Some(Worker::start().ok_or(WorkerError::Unavailable)?);
        }
        let Some(worker) = slot.as_mut() else {
            return Err(WorkerError::Unavailable);
        };
        match worker.extract(key) {
            Ok(icon) => return Ok(icon),
            Err(timed_out) => {
                *slot = None;
                if timed_out {
                    log::warn(format!(
                        "Icon worker: no answer for {} in {} s; skipped.",
                        key.0,
                        WORKER_TIMEOUT.as_secs()
                    ));
                    return Err(WorkerError::Failed);
                }
                if attempt == 1 {
                    log::warn(format!("Icon worker ended twice on {}; skipped.", key.0));
                }
            }
        }
    }
    Err(WorkerError::Failed)
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    answers: Receiver<Option<Icon>>,
}

impl Worker {
    fn start() -> Option<Worker> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = WORKER_EXE.get()?;
        let mut child = Command::new(exe)
            .arg(WORKER_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| log::warn(format!("Icon worker could not start: {error}")))
            .ok()?;
        let stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?;
        let (send, answers) = mpsc::channel();
        std::thread::Builder::new()
            .name("ot-icons-reader".into())
            .spawn(move || {
                while let Some(answer) = read_frame(&mut stdout) {
                    if send.send(answer).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Worker {
            child,
            stdin,
            answers,
        })
    }

    /// `Err(true)`: no answer in time; `Err(false)`: the worker ended.
    fn extract(&mut self, key: &IconKey) -> Result<Option<Icon>, bool> {
        let line = format!("{}\t{}\n", key.1, key.0);
        if self.stdin.write_all(line.as_bytes()).is_err() || self.stdin.flush().is_err() {
            return Err(false);
        }
        match self.answers.recv_timeout(WORKER_TIMEOUT) {
            Ok(icon) => Ok(icon),
            Err(RecvTimeoutError::Timeout) => Err(true),
            Err(RecvTimeoutError::Disconnected) => Err(false),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One answer: width, height and byte length (little-endian u32 each), then the pixels; a zero width means no icon.
fn read_frame(reader: &mut impl Read) -> Option<Option<Icon>> {
    let mut header = [0u8; 12];
    reader.read_exact(&mut header).ok()?;
    let word =
        |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
    let (width, height, length) = (word(0), word(4), word(8));
    if width == 0 {
        return Some(None);
    }
    if width > MAX_SIDE
        || height > MAX_SIDE
        || length as u64 != u64::from(width) * u64::from(height) * 4
    {
        return None;
    }
    let mut pixels = vec![0u8; length as usize];
    reader.read_exact(&mut pixels).ok()?;
    Some(Some(Icon {
        width,
        height,
        pixels,
    }))
}

fn write_frame(writer: &mut impl Write, icon: Option<&Icon>) -> std::io::Result<()> {
    match icon {
        Some(icon) => {
            writer.write_all(&icon.width.to_le_bytes())?;
            writer.write_all(&icon.height.to_le_bytes())?;
            writer.write_all(&(icon.pixels.len() as u32).to_le_bytes())?;
            writer.write_all(&icon.pixels)?;
        }
        None => writer.write_all(&[0u8; 12])?,
    }
    writer.flush()
}

/// `--extract-icons`: answers "size<TAB>parsing name" lines from stdin until it closes. Started by the dock's loader;
/// harmless when run by hand with stdin closed.
pub fn run_worker() {
    // SAFETY: COM for the Shell image factory on this (the only) thread.
    let _ = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let icon = line
            .split_once('\t')
            .and_then(|(size, name)| Some((size.parse::<u32>().ok()?, name)))
            .filter(|&(size, _)| (1..=MAX_SIDE).contains(&size))
            .and_then(|(size, name)| icons::load(name, size));
        if write_frame(&mut stdout, icon.as_ref()).is_err() {
            break;
        }
    }
}

// ---------- Disk cache ----------

fn cache_dir() -> PathBuf {
    ot_core::store::app_data_dir().join("icon-cache")
}

/// Size and modification time of the icon's file; only icons of files are cached on disk (Store apps and shell
/// objects have nothing to check them against).
fn fingerprint(parsing_name: &str) -> Option<(u64, u64)> {
    let is_path = parsing_name.get(1..3) == Some(":\\") || parsing_name.starts_with("\\\\");
    if !is_path {
        return None;
    }
    let meta = std::fs::metadata(parsing_name).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), modified))
}

/// FNV-1a 64: a stable file name for (name, size).
fn cache_name(key: &IconKey) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.0.to_lowercase().bytes().chain(key.1.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}.oticon")
}

/// `OT_ICON_CACHE=0` turns the disk cache off (measurements).
fn disk_cache_enabled() -> bool {
    std::env::var_os("OT_ICON_CACHE").is_none_or(|v| v != "0")
}

fn read_cached(key: &IconKey) -> Option<Icon> {
    if !disk_cache_enabled() {
        return None;
    }
    read_cached_in(&cache_dir(), key, fingerprint(&key.0)?)
}

fn read_cached_in(dir: &Path, key: &IconKey, print: (u64, u64)) -> Option<Icon> {
    let path = dir.join(cache_name(key));
    if std::fs::metadata(&path).ok()?.len() > u64::from(MAX_SIDE * MAX_SIDE * 4) + 64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 32 || &bytes[..8] != CACHE_MAGIC {
        return None;
    }
    let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap_or_default());
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap_or_default());
    if (u64_at(8), u64_at(16)) != print {
        return None;
    }
    let (width, height) = (u32_at(24), u32_at(28));
    if width == 0 || width > MAX_SIDE || height == 0 || height > MAX_SIDE {
        return None;
    }
    let pixels = bytes[32..].to_vec();
    (pixels.len() as u64 == u64::from(width) * u64::from(height) * 4).then_some(Icon {
        width,
        height,
        pixels,
    })
}

fn write_cached(key: &IconKey, icon: &Icon) {
    if !disk_cache_enabled() {
        return;
    }
    if let Some(print) = fingerprint(&key.0) {
        write_cached_in(&cache_dir(), key, print, icon);
    }
}

fn write_cached_in(dir: &Path, key: &IconKey, print: (u64, u64), icon: &Icon) {
    let mut bytes = Vec::with_capacity(32 + icon.pixels.len());
    bytes.extend_from_slice(CACHE_MAGIC);
    bytes.extend_from_slice(&print.0.to_le_bytes());
    bytes.extend_from_slice(&print.1.to_le_bytes());
    bytes.extend_from_slice(&icon.width.to_le_bytes());
    bytes.extend_from_slice(&icon.height.to_le_bytes());
    bytes.extend_from_slice(&icon.pixels);
    let path = dir.join(cache_name(key));
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&temporary, &bytes))
        .and_then(|()| std::fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon() -> Icon {
        Icon {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
        }
    }

    #[test]
    fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, Some(&icon())).unwrap();
        write_frame(&mut buffer, None).unwrap();
        let mut reader = buffer.as_slice();
        let first = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(
            (first.width, first.height, first.pixels),
            (2, 1, icon().pixels)
        );
        assert!(read_frame(&mut reader).unwrap().is_none());
        assert!(read_frame(&mut reader).is_none());
        // A frame that claims an impossible size is refused.
        let mut bad = Vec::new();
        bad.extend_from_slice(&2u32.to_le_bytes());
        bad.extend_from_slice(&1u32.to_le_bytes());
        bad.extend_from_slice(&9u32.to_le_bytes());
        assert!(read_frame(&mut bad.as_slice()).is_none());
    }

    #[test]
    fn disk_cache_checks_the_fingerprint() {
        let dir = std::env::temp_dir().join(format!("ot-icon-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = (r"C:\Apps\App.exe".to_owned(), 28);
        write_cached_in(&dir, &key, (10, 20), &icon());
        let read = read_cached_in(&dir, &key, (10, 20)).unwrap();
        assert_eq!(read.pixels, icon().pixels);
        assert!(read_cached_in(&dir, &key, (10, 21)).is_none());
        assert!(read_cached_in(&dir, &(key.0.clone(), 42), (10, 20)).is_none());
        assert_eq!(cache_name(&key), cache_name(&(key.0.to_uppercase(), 28)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
