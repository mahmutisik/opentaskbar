//! Background apps known from the registry (OpenTaskbar's `TrayIcons.RunningApps` and the registry half of
//! `TrayIconSource`): Windows 11 records every tray icon (path, tooltip, an icon snapshot) under
//! `HKCU\Control Panel\NotifyIconSettings`; the running ones fill the gaps of the live tray host (icons registered
//! before the dock started that never registered again). A worker thread keeps the list current: a registry
//! watcher (debounced 200 ms) and a 30 s refresh for process starts that do not touch the registry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_BINARY, REG_DWORD, REG_EXPAND_SZ,
    REG_NOTIFY_CHANGE_ATTRIBUTES, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_QWORD,
    REG_SZ, RegCloseKey, RegEnumKeyExW, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects,
};

use crate::icons::Icon;

const SETTINGS_KEY: &str = r"Control Panel\NotifyIconSettings";
const DEBOUNCE: Duration = Duration::from_millis(200);
const PERIODIC_MS: u32 = 30_000;
/// PNG snapshots above this size are ignored (`DecodeSnapshot`).
const MAX_SNAPSHOT: usize = 512 * 1024;

/// A background app from the registry.
#[derive(Clone, Debug)]
pub struct TrayApp {
    pub name: String,
    pub path: String,
    pub icon: Option<Arc<Icon>>,
    /// Lowercase GUID text (the registry subkey's name when it is a GUID), empty otherwise.
    pub guid: String,
    pub uid: u32,
    pub icon_hash: u64,
}

impl TrayApp {
    fn same(&self, other: &Self) -> bool {
        self.name == other.name
            && self.path == other.path
            && self.guid == other.guid
            && self.uid == other.uid
            && self.icon_hash == other.icon_hash
    }
}

/// `TrayIconRef` identity: GUID, path and UID (the path case-insensitively).
pub fn identity(guid: &str, path: &str, uid: u32) -> (String, String, u32) {
    (guid.to_ascii_lowercase(), path.trim().to_lowercase(), uid)
}

struct Shared {
    apps: Mutex<Arc<Vec<TrayApp>>>,
    stop: AtomicBool,
}

/// The worker that keeps the registry list current; dropping it stops the thread.
pub struct Source {
    shared: Arc<Shared>,
    wake: HANDLE,
    thread: Option<JoinHandle<()>>,
}

impl Source {
    /// Starts the worker; `changed` runs on the worker thread after each list that differs from the last.
    pub fn start(changed: impl Fn() + Send + 'static) -> Option<Self> {
        // SAFETY: an unnamed manual-reset event, closed in Drop.
        let wake = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if wake.is_null() {
            return None;
        }
        let shared = Arc::new(Shared {
            apps: Mutex::new(Arc::new(Vec::new())),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let wake_handle = wake as usize;
        let thread = std::thread::Builder::new()
            .name("tray-apps".into())
            .spawn(move || run(&worker, wake_handle as HANDLE, &changed))
            .ok();
        Some(Self {
            shared,
            wake,
            thread,
        })
    }

    /// The latest list (sorted by name).
    pub fn apps(&self) -> Arc<Vec<TrayApp>> {
        self.shared
            .apps
            .lock()
            .map(|a| Arc::clone(&a))
            .unwrap_or_default()
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // SAFETY: our own event.
        unsafe { SetEvent(self.wake) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: closed once, after the thread stopped using it.
        unsafe { CloseHandle(self.wake) };
    }
}

fn run(shared: &Shared, wake: HANDLE, changed: &dyn Fn()) {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    // SAFETY: COM for the shell icon and WIC calls on this thread, released below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let mut watcher = Watcher::open();
    let mut last: Option<Vec<TrayApp>> = None;
    let mut cache = IconCache::default();
    loop {
        let apps = running_apps(&mut cache);
        let differs = last
            .as_ref()
            .is_none_or(|l| l.len() != apps.len() || l.iter().zip(&apps).any(|(a, b)| !a.same(b)));
        if differs {
            if let Ok(mut slot) = shared.apps.lock() {
                *slot = Arc::new(apps.clone());
            }
            last = Some(apps);
            changed();
        }
        // Wait for a registry change, the 30 s refresh, or stop.
        let mut handles = vec![wake];
        if let Some(event) = watcher.as_ref().map(|w| w.event) {
            handles.push(event);
        }
        // SAFETY: valid event handles for the duration of the wait.
        let result = unsafe {
            WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, PERIODIC_MS)
        };
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if result == WAIT_OBJECT_0 + 1 {
            // One-shot notifications: re-arm, then let a burst of writes settle.
            if !watcher.as_mut().is_some_and(Watcher::arm) {
                watcher = None;
            }
            std::thread::sleep(DEBOUNCE);
            if shared.stop.load(Ordering::SeqCst) {
                break;
            }
        }
    }
    drop(watcher);
    if com {
        // SAFETY: balances CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

/// `RegNotifyChangeKeyValue` on the settings key (subtree; names, attributes, values).
struct Watcher {
    key: HKEY,
    event: HANDLE,
}

impl Watcher {
    fn open() -> Option<Self> {
        let key = open_key(KEY_NOTIFY | KEY_READ)?;
        // SAFETY: manual-reset event, closed in Drop.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        let mut watcher = Self { key, event };
        if event.is_null() || !watcher.arm() {
            return None;
        }
        Some(watcher)
    }

    fn arm(&mut self) -> bool {
        // SAFETY: our own key and event.
        unsafe {
            ResetEvent(self.event);
            RegNotifyChangeKeyValue(
                self.key,
                1,
                REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_ATTRIBUTES | REG_NOTIFY_CHANGE_LAST_SET,
                self.event,
                1,
            ) == 0
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: closing our own handles once.
        unsafe {
            RegCloseKey(self.key);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_key(access: u32) -> Option<HKEY> {
    let name = wide(SETTINGS_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid name and out pointer.
    (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, access, &mut key) } == 0)
        .then_some(key)
}

enum Value {
    Text(String),
    Number(i64),
    Bytes(Vec<u8>),
}

fn read_value(key: HKEY, name: &str) -> Option<Value> {
    let name = wide(name);
    let mut kind = 0u32;
    let mut size = 0u32;
    // SAFETY: size query, then a read into a buffer of that size.
    unsafe {
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        ) != 0
        {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            data.as_mut_ptr(),
            &mut size,
        ) != 0
        {
            return None;
        }
        data.truncate(size as usize);
        match kind {
            REG_SZ | REG_EXPAND_SZ => {
                let units: Vec<u16> = data
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                Some(Value::Text(String::from_utf16_lossy(&units[..end])))
            }
            // .NET reads a DWORD as a signed int.
            REG_DWORD if data.len() >= 4 => Some(Value::Number(i64::from(i32::from_le_bytes([
                data[0], data[1], data[2], data[3],
            ])))),
            REG_QWORD if data.len() >= 8 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&data[..8]);
                Some(Value::Number(i64::from_le_bytes(bytes)))
            }
            REG_BINARY => Some(Value::Bytes(data)),
            _ => None,
        }
    }
}

struct Candidate {
    path: String,
    file_name: String,
    tooltip: Option<String>,
    snapshot: Option<Vec<u8>>,
    promoted: bool,
    prefer_exe: bool,
    guid: String,
    uid: u32,
}

/// `IsTransientTooltip`: a tooltip that describes a passing state; the snapshot then likely shows it too.
pub fn is_transient_tooltip(tooltip: Option<&str>) -> bool {
    let Some(text) = tooltip.filter(|t| !t.trim().is_empty()) else {
        return false;
    };
    let lower = text.to_lowercase();
    [
        "starting",
        "initializing",
        "installing",
        "updating",
        "connecting",
        "loading",
        "başlatılıyor",
        "yükleniyor",
        "güncelleniyor",
        "bağlanıyor",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// `Guid.TryParse` for the formats a subkey name may use (D, B, P, N); lowercase "D" text.
pub fn parse_guid(text: &str) -> Option<String> {
    let t = text.trim();
    let inner =
        if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('(') && t.ends_with(')')) {
            &t[1..t.len() - 1]
        } else {
            t
        };
    let hex: String = match inner.len() {
        36 => {
            let b = inner.as_bytes();
            if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
                return None;
            }
            inner.replace('-', "")
        }
        32 if inner.len() == t.len() => inner.to_owned(),
        _ => return None,
    };
    if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = hex.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

fn read_uid(key: HKEY) -> u32 {
    // Value names are case-insensitive, so this also reads "uID" and "UId".
    match read_value(key, "UID") {
        Some(Value::Number(n)) if (0..=i64::from(u32::MAX)).contains(&n) => n as u32,
        Some(Value::Text(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// `ResolveKnownFolderPath`: `{KNOWNFOLDERID}\relative\path.exe` or a plain path.
fn resolve_known_folder(path: &str) -> Option<String> {
    if !path.starts_with('{') {
        return Some(path.to_owned());
    }
    let end = path.find('}')?;
    let id = parse_guid(&path[..=end])?;
    let folder = known_folder(&id)?;
    let rest = path[end + 1..].trim_start_matches('\\');
    Some(if rest.is_empty() {
        folder
    } else if folder.ends_with('\\') {
        format!("{folder}{rest}")
    } else {
        format!("{folder}\\{rest}")
    })
}

fn known_folder(id: &str) -> Option<String> {
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let guid = windows::core::GUID::try_from(id).ok()?;
    // SAFETY: the returned string is freed below.
    unsafe {
        let path = SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        text
    }
}

fn read_candidates() -> Vec<Candidate> {
    let Some(root) = open_key(KEY_READ) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: buffer and length describe `name`.
        let status = unsafe {
            RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut length,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        index += 1;
        let name = String::from_utf16_lossy(&name[..length as usize]);
        let sub = wide(&name);
        let mut entry: HKEY = std::ptr::null_mut();
        // SAFETY: valid name and out pointer; closed below.
        if unsafe { RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_READ, &mut entry) } != 0 {
            continue;
        }
        if let Some(Value::Text(raw)) = read_value(entry, "ExecutablePath")
            && let Some(path) = resolve_known_folder(&raw)
        {
            let file_name = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or_default()
                .to_owned();
            if !file_name.is_empty() && !file_name.eq_ignore_ascii_case("explorer.exe") {
                let raw_tooltip = match read_value(entry, "InitialTooltip") {
                    Some(Value::Text(t)) => Some(
                        t.trim_matches(['\u{202a}', '\u{202c}', '\u{200e}', '\u{200f}', ' '])
                            .to_owned(),
                    ),
                    _ => None,
                };
                let prefer_exe = is_transient_tooltip(raw_tooltip.as_deref());
                let tooltip = raw_tooltip.as_deref().and_then(|t| {
                    t.split(['\r', '\n'])
                        .find(|l| !l.is_empty())
                        .map(|l| l.trim().to_owned())
                });
                candidates.push(Candidate {
                    path,
                    file_name,
                    tooltip,
                    snapshot: match read_value(entry, "IconSnapshot") {
                        Some(Value::Bytes(b)) => Some(b),
                        _ => None,
                    },
                    promoted: matches!(read_value(entry, "IsPromoted"), Some(Value::Number(1))),
                    prefer_exe,
                    guid: parse_guid(&name).unwrap_or_default(),
                    uid: read_uid(entry),
                });
            }
        }
        // SAFETY: opened above.
        unsafe { RegCloseKey(entry) };
    }
    // SAFETY: opened above.
    unsafe { RegCloseKey(root) };
    // Promoted first, transient tooltips last; one entry per identity.
    candidates.sort_by_key(|c| (!c.promoted, c.prefer_exe));
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(identity(&c.guid, &c.path, c.uid)));
    candidates
}

/// `RunningExecutables`: full paths of running processes in this session whose file name is one of `names`.
fn running_executables(
    names: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut paths = std::collections::HashSet::new();
    // SAFETY: plain Win32 calls; the snapshot is closed below.
    unsafe {
        let mut session = 0u32;
        let session =
            (ProcessIdToSessionId(GetCurrentProcessId(), &mut session) != 0).then_some(session);
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return paths;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase();
            if entry.th32ProcessID != 0 && names.contains(&exe) {
                let mut id = 0u32;
                let same_session = session.is_none_or(|s| {
                    ProcessIdToSessionId(entry.th32ProcessID, &mut id) != 0 && id == s
                });
                if same_session && let Some(path) = ot_shell::win::process_path(entry.th32ProcessID)
                {
                    paths.insert(path.to_lowercase());
                }
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    paths
}

/// A cached icon: the snapshot's length and hash, the prefer-exe flag and the icon.
type CachedIcon = (usize, u64, bool, Option<Arc<Icon>>);

#[derive(Default)]
struct IconCache {
    /// Per lowercase path.
    entries: std::collections::HashMap<String, CachedIcon>,
    descriptions: std::collections::HashMap<String, String>,
}

const MAX_CACHE_ENTRIES: usize = 64;

impl IconCache {
    /// `GetCachedIcon`: the snapshot (or the exe's icon when the tooltip was transient, or as a fallback).
    fn icon(&mut self, path: &str, snapshot: Option<&[u8]>, prefer_exe: bool) -> Option<Arc<Icon>> {
        let len = snapshot.map_or(0, <[u8]>::len);
        let hash = snapshot.map_or(0, fnv1a);
        let key = path.to_lowercase();
        if let Some((l, h, p, icon)) = self.entries.get(&key)
            && *l == len
            && *h == hash
            && *p == prefer_exe
        {
            return icon.clone();
        }
        let exe = || crate::icons::load(&crate::icons::parsing_name(path, None), 32);
        let icon = if prefer_exe {
            exe().or_else(|| snapshot.and_then(decode_snapshot))
        } else {
            snapshot.and_then(decode_snapshot).or_else(exe)
        }
        .map(Arc::new);
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            self.entries.clear();
        }
        self.entries
            .insert(key, (len, hash, prefer_exe, icon.clone()));
        icon
    }

    /// `DescribeExecutable`: FileDescription, else the file name without extension.
    fn describe(&mut self, path: &str) -> String {
        if let Some(name) = self.descriptions.get(path) {
            return name.clone();
        }
        let name = crate::app_name::file_description(path).unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if self.descriptions.len() >= MAX_CACHE_ENTRIES {
            self.descriptions.clear();
        }
        self.descriptions.insert(path.to_owned(), name.clone());
        name
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Hash of an icon's pixels (to notice a changed icon).
pub fn icon_hash(icon: &Icon) -> u64 {
    fnv1a(&icon.pixels) ^ (u64::from(icon.width) << 32 | u64::from(icon.height))
}

/// `DecodeSnapshot`: a PNG (at most 512 KB), decoded at most 32 px wide (`DecodePixelWidth = 32`; the height
/// follows the aspect ratio).
fn decode_snapshot(png: &[u8]) -> Option<Icon> {
    if png.is_empty() || png.len() > MAX_SNAPSHOT {
        return None;
    }
    let size = match png_width(png) {
        Some(width) if width > 32 => {
            let height = i32::from_be_bytes([png[20], png[21], png[22], png[23]]) as u32;
            Some((
                32,
                ((f64::from(height) * 32.0 / f64::from(width)).round() as u32).max(1),
            ))
        }
        _ => None,
    };
    crate::icons::decode_image(png, size)
}

/// `TryReadPngWidth`.
fn png_width(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 24 || bytes[..4] != [0x89, 0x50, 0x4E, 0x47] {
        return None;
    }
    let width = i32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = i32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    ((1..=2048).contains(&width) && (1..=2048).contains(&height)).then_some(width as u32)
}

/// `EnumerateRunningApps`: registry entries whose executable runs in this session, one per identity, by name.
fn running_apps(cache: &mut IconCache) -> Vec<TrayApp> {
    let mut candidates = read_candidates();
    if candidates.is_empty() {
        return Vec::new();
    }
    let names = candidates
        .iter()
        .map(|c| c.file_name.to_lowercase())
        .collect();
    let running = running_executables(&names);
    if running.is_empty() {
        return Vec::new();
    }
    let driver_store = |path: &str| {
        path.to_lowercase()
            .contains(r"\driverstore\filerepository\")
    };
    candidates.sort_by_key(|c| (!c.promoted, driver_store(&c.path)));
    let mut seen = std::collections::HashSet::new();
    let mut apps: Vec<TrayApp> = candidates
        .into_iter()
        .filter(|c| running.contains(&c.path.to_lowercase()))
        .filter(|c| seen.insert(identity(&c.guid, &c.path, c.uid)))
        .map(|c| {
            let name = match c.tooltip.filter(|t| !t.trim().is_empty()) {
                Some(tooltip) => tooltip,
                None => cache.describe(&c.path),
            };
            let icon = cache.icon(&c.path, c.snapshot.as_deref(), c.prefer_exe);
            TrayApp {
                name,
                icon_hash: icon.as_deref().map_or(0, icon_hash),
                icon,
                path: c.path,
                guid: c.guid,
                uid: c.uid,
            }
        })
        .collect();
    apps.sort_by_cached_key(|a| a.name.to_lowercase());
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_parse_like_dotnet() {
        let d = "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9";
        assert_eq!(parse_guid(d).as_deref(), Some(d));
        assert_eq!(
            parse_guid("{0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9}").as_deref(),
            Some(d)
        );
        assert_eq!(
            parse_guid("0a1b2c3d4e5f60718293a4b5c6d7e8f9").as_deref(),
            Some(d)
        );
        assert_eq!(parse_guid("1234567890123456789"), None);
        assert_eq!(parse_guid("{0a1b2c3d4e5f60718293a4b5c6d7e8f9}"), None);
    }

    #[test]
    fn transient_tooltips() {
        assert!(is_transient_tooltip(Some("App is Loading...")));
        assert!(is_transient_tooltip(Some("Güncelleniyor")));
        assert!(!is_transient_tooltip(Some("App")));
        assert!(!is_transient_tooltip(None));
    }

    #[test]
    fn png_header_width() {
        let mut png = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        png.extend_from_slice(&64i32.to_be_bytes());
        png.extend_from_slice(&48i32.to_be_bytes());
        assert_eq!(png_width(&png), Some(64));
        assert_eq!(png_width(&png[..20]), None);
    }
}

#[cfg(test)]
mod live {
    /// Reads this machine's registry list (read-only); run with `--ignored --nocapture`.
    #[test]
    #[ignore = "reads the machine's tray settings"]
    fn print_running_apps() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // SAFETY: COM for this test thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let started = std::time::Instant::now();
        let apps = super::running_apps(&mut super::IconCache::default());
        println!("{} apps in {:?}", apps.len(), started.elapsed());
        for app in apps {
            let icon = app
                .icon
                .as_ref()
                .map(|i| format!("{}x{}", i.width, i.height));
            println!(
                "{:?} | {} | guid={} uid={} icon={:?}",
                app.name, app.path, app.guid, app.uid, icon
            );
        }
    }
}
