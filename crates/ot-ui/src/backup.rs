//! Settings backups in OpenTaskbar's formats, so files move between the two apps:
//! - `SettingsBackup`: one JSON file `{ "Version": 1, "Settings": {..}, "Pins": [{ "Name", "ExecutablePath",
//!   "Arguments" }] }`.
//! - `FullBackupArchive`: an `.otbackup` zip with `manifest.json` (`Version`, `HasTheme`, `ThemeId`, `ThemeName`),
//!   `settings.json` (the JSON above) and, when a custom theme pack is active, its files under `theme/`.
//!
//! The zip code is small and self-contained: archives are written with stored entries (every zip reader, .NET's
//! `ZipFile` included, reads them) and read with stored or deflate entries (what .NET writes).

use std::path::Path;

use ot_core::settings::DockSettings;
use ot_core::store::SettingsStore;
use serde_json::{Value, json};

pub type PinBackup = (String, String, Option<String>);

/// Limits for reading an archive (`ThemePackTransfer.MaxUncompressedBytes` / `MaxFileCount`).
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 512;

/// `SettingsBackup.Serialize`.
pub fn serialize(settings: &DockSettings, pins: &[PinBackup]) -> String {
    let pins: Vec<Value> = pins
        .iter()
        .map(
            |(name, path, args)| json!({ "Name": name, "ExecutablePath": path, "Arguments": args }),
        )
        .collect();
    let value = json!({
        "Version": 1,
        "Settings": serde_json::to_value(settings).unwrap_or(Value::Null),
        "Pins": pins,
    });
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

/// `SettingsBackup.Parse`: the settings (migrated and validated like a settings file) and the pins with a target.
pub fn parse(
    text: &str,
    store: &SettingsStore,
) -> Result<(DockSettings, Vec<PinBackup>), &'static str> {
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "ErrorSettingsFileInvalid")?;
    let settings = value
        .get("Settings")
        .filter(|s| s.is_object())
        .ok_or("ErrorSettingsFileInvalid")?;
    let settings = store
        .parse(&settings.to_string())
        .map_err(|_| "ErrorSettingsFileInvalid")?;
    let pins = value
        .get("Pins")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let path = p.get("ExecutablePath")?.as_str()?.trim();
                    (!path.is_empty()).then(|| {
                        (
                            p.get("Name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            path.to_owned(),
                            p.get("Arguments")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((settings, pins))
}

/// `SettingsBackup.Export`.
pub fn export(
    path: &Path,
    settings: &DockSettings,
    pins: &[PinBackup],
) -> Result<(), &'static str> {
    std::fs::write(path, serialize(settings, pins)).map_err(|_| "ErrorSettingsFileWrite")
}

/// `FullBackupArchive.Export`: settings, pins and, when a valid custom theme pack is active, its files under
/// `theme/`.
pub fn export_full(
    path: &Path,
    settings: &DockSettings,
    pins: &[PinBackup],
) -> Result<(), &'static str> {
    use ot_core::theme_pack;
    let theme = settings
        .theme_pack
        .as_deref()
        .filter(|id| theme_pack::is_safe_id(id) && !theme_pack::is_built_in(id))
        .and_then(|id| {
            let dir = theme_pack::root().join(id);
            if theme_pack::validate(&dir).is_some() {
                return None;
            }
            let files = theme_pack::export_files(id).ok()?;
            files
                .iter()
                .all(|(name, _)| theme_pack::is_allowed_file(name))
                .then(|| {
                    let name = theme_pack::ThemePack::load(&dir)
                        .map(|(p, _)| p.display_name())
                        .unwrap_or_else(|_| id.to_owned());
                    (id.to_owned(), name, files)
                })
        });
    let manifest = serde_json::to_string_pretty(&json!({
        "Version": 1,
        "HasTheme": theme.is_some(),
        "ThemeId": theme.as_ref().map(|t| t.0.clone()),
        "ThemeName": theme.as_ref().map(|t| t.1.clone()),
    }))
    .unwrap_or_default();
    let settings_json = serialize(settings, pins);
    let theme_names: Vec<String> = theme
        .as_ref()
        .map(|t| t.2.iter().map(|(n, _)| format!("theme/{n}")).collect())
        .unwrap_or_default();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("manifest.json", manifest.as_bytes()),
        ("settings.json", settings_json.as_bytes()),
    ];
    if let Some((_, _, files)) = &theme {
        for (name, (_, bytes)) in theme_names.iter().zip(files) {
            entries.push((name, bytes));
        }
    }
    let archive = zip_write(&entries);
    let temporary = path.with_extension("otbackup.tmp");
    std::fs::write(&temporary, archive)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|_| {
            let _ = std::fs::remove_file(&temporary);
            "ErrorFullBackupWrite"
        })
}

/// A theme pack carried by a full backup: its id when exported and its files relative to the theme folder.
pub struct BackupTheme {
    pub id: Option<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

/// `FullBackupArchive.Inspect` / `Import`: the settings, the pins and the theme's files when the archive has one.
pub fn import_full(
    path: &Path,
    store: &SettingsStore,
) -> Result<(DockSettings, Vec<PinBackup>, Option<BackupTheme>), &'static str> {
    const INVALID: &str = "ErrorFullBackupInvalid";
    // The size is checked before the file is read into memory.
    if std::fs::metadata(path).map_err(|_| INVALID)?.len() > MAX_BYTES as u64 {
        return Err(INVALID);
    }
    let bytes = std::fs::read(path).map_err(|_| INVALID)?;
    if bytes.len() > MAX_BYTES {
        return Err(INVALID);
    }
    let entries = zip_read(&bytes).ok_or(INVALID)?;
    let find = |name: &str| entries.iter().find(|(n, _)| n == name).map(|(_, d)| d);
    let manifest: Value =
        serde_json::from_slice(find("manifest.json").ok_or(INVALID)?).map_err(|_| INVALID)?;
    if manifest.get("Version").and_then(Value::as_i64) != Some(1) {
        return Err(INVALID);
    }
    let settings =
        std::str::from_utf8(find("settings.json").ok_or(INVALID)?).map_err(|_| INVALID)?;
    let (settings, pins) = parse(settings, store).map_err(|_| INVALID)?;
    let has_theme = manifest
        .get("HasTheme")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if has_theme && !entries.iter().any(|(n, _)| n.starts_with("theme/")) {
        return Err(INVALID);
    }
    let theme = has_theme.then(|| BackupTheme {
        id: manifest
            .get("ThemeId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        files: entries
            .iter()
            .filter_map(|(n, d)| n.strip_prefix("theme/").map(|r| (r.to_owned(), d.clone())))
            .collect(),
    });
    Ok((settings, pins, theme))
}

// ---------- Zip ----------

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| {
        table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8)
    })
}

/// A zip of stored entries with UTF-8 names.
pub(crate) fn zip_write(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // DOS date 2026-01-01 00:00 (the content matters, not the time).
    let (time, date) = (0u16, ((2026 - 1980) << 9 | 1 << 5 | 1) as u16);
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let name = name.as_bytes();
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend_from_slice(&sig.to_le_bytes());
            if central {
                h.extend_from_slice(&20u16.to_le_bytes()); // made by
            }
            h.extend_from_slice(&20u16.to_le_bytes()); // needed
            h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend_from_slice(&0u16.to_le_bytes()); // stored
            h.extend_from_slice(&time.to_le_bytes());
            h.extend_from_slice(&date.to_le_bytes());
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central {
                h.extend_from_slice(&0u16.to_le_bytes()); // comment
                h.extend_from_slice(&0u16.to_le_bytes()); // disk
                h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
                h.extend_from_slice(&0u32.to_le_bytes()); // external attributes
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name);
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        central.extend(header(0x0201_4b50, true));
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// Reads a zip's file entries (directories skipped); `None` when it is malformed, too large, unsafe (`..` or
/// absolute names) or uses an unsupported method.
pub(crate) fn zip_read(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let eocd = (0..bytes.len().saturating_sub(21))
        .rev()
        .take(65_557)
        .find(|&i| bytes.get(i..i + 4) == Some(&[0x50, 0x4b, 0x05, 0x06]))?;
    let count = u16_at(bytes, eocd + 10)?;
    let mut at = u32_at(bytes, eocd + 16)?;
    if count > MAX_FILES {
        return None;
    }
    let mut entries = Vec::new();
    let mut total = 0usize;
    for _ in 0..count {
        if u32_at(bytes, at)? != 0x0201_4b50 {
            return None;
        }
        let method = u16_at(bytes, at + 10)?;
        let crc = u32_at(bytes, at + 16)? as u32;
        let packed = u32_at(bytes, at + 20)?;
        let size = u32_at(bytes, at + 24)?;
        let name_len = u16_at(bytes, at + 28)?;
        let extra_len = u16_at(bytes, at + 30)?;
        let comment_len = u16_at(bytes, at + 32)?;
        let local = u32_at(bytes, at + 42)?;
        let name =
            String::from_utf8_lossy(bytes.get(at + 46..at + 46 + name_len)?).replace('\\', "/");
        at += 46 + name_len + extra_len + comment_len;
        if name.ends_with('/') {
            continue;
        }
        if name.starts_with('/')
            || name.contains(':')
            || name.contains('\0')
            || name.split('/').any(|p| p == "..")
        {
            return None;
        }
        total += size;
        if total > MAX_BYTES {
            return None;
        }
        if u32_at(bytes, local)? != 0x0403_4b50 {
            return None;
        }
        let start = local + 30 + u16_at(bytes, local + 26)? + u16_at(bytes, local + 28)?;
        let raw = bytes.get(start..start + packed)?;
        let data = match method {
            0 => raw.to_vec(),
            8 => inflate(raw, size)?,
            _ => return None,
        };
        if data.len() != size || crc32(&data) != crc {
            return None;
        }
        entries.push((name, data));
    }
    Some(entries)
}

// ---------- Inflate (RFC 1951) ----------

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    count: u32,
}

impl Bits<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            self.bit |= u32::from(*self.data.get(self.pos)?) << self.count;
            self.pos += 1;
            self.count += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.count -= n;
        Some(v)
    }
}

/// A canonical Huffman code: how many codes of each length and the symbols in code order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for l in 1..15 {
            offsets[l + 1] = offsets[l] + counts[l];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = s as u16;
                offsets[l as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decompresses a raw deflate stream of `expected` bytes (a bound, not trusted beyond `MAX_BYTES`).
fn inflate(data: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.min(MAX_BYTES));
    let mut bits = Bits {
        data,
        pos: 0,
        bit: 0,
        count: 0,
    };
    loop {
        let last = bits.bits(1)? == 1;
        match bits.bits(2)? {
            0 => {
                // Stored: byte-aligned length, its complement, then the bytes.
                bits.bit = 0;
                bits.count = 0;
                let len = u16_at(data, bits.pos)?;
                let nlen = u16_at(data, bits.pos + 2)?;
                if len != !nlen & 0xFFFF {
                    return None;
                }
                bits.pos += 4;
                out.extend_from_slice(data.get(bits.pos..bits.pos + len)?);
                bits.pos += len;
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                block(&mut bits, &mut out, &lit, &dist)?;
            }
            2 => {
                let hlit = bits.bits(5)? as usize + 257;
                let hdist = bits.bits(5)? as usize + 1;
                let hclen = bits.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut code_lengths = [0u8; 19];
                for &i in ORDER.iter().take(hclen) {
                    code_lengths[i] = bits.bits(3)? as u8;
                }
                let lencode = Huffman::new(&code_lengths)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = lencode.decode(&mut bits)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.bits(2)? as usize),
                        17 => (0, 3 + bits.bits(3)? as usize),
                        18 => (0, 11 + bits.bits(7)? as usize),
                        _ => return None,
                    };
                    if i + repeat > lengths.len() {
                        return None;
                    }
                    lengths[i..i + repeat].fill(value);
                    i += repeat;
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                block(&mut bits, &mut out, &lit, &dist)?;
            }
            _ => return None,
        }
        if out.len() > MAX_BYTES {
            return None;
        }
        if last {
            return Some(out);
        }
    }
}

fn block(bits: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Option<()> {
    loop {
        let sym = lit.decode(bits)? as usize;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Some(());
        } else {
            let i = sym - 257;
            let len = *LENGTH_BASE.get(i)? as usize
                + bits.bits(u32::from(*LENGTH_EXTRA.get(i)?))? as usize;
            let d = dist.decode(bits)? as usize;
            let distance =
                *DIST_BASE.get(d)? as usize + bits.bits(u32::from(*DIST_EXTRA.get(d)?))? as usize;
            let from = out.len().checked_sub(distance)?;
            for k in 0..len {
                let b = out[from + k];
                out.push(b);
            }
            if out.len() > MAX_BYTES {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 3_421_780_262);
    }

    #[test]
    fn inflates_fixed_stored_and_dynamic_blocks() {
        let fixed = [
            203, 72, 205, 201, 201, 87, 200, 64, 34, 203, 243, 139, 114, 82, 0,
        ];
        assert_eq!(inflate(&fixed, 23).unwrap(), b"hello hello hello world");
        let stored = [
            1, 23, 0, 232, 255, 104, 101, 108, 108, 111, 32, 104, 101, 108, 108, 111, 32, 104, 101,
            108, 108, 111, 32, 119, 111, 114, 108, 100,
        ];
        assert_eq!(inflate(&stored, 23).unwrap(), b"hello hello hello world");
        let dynamic = include_bytes!("../testdata/dynamic.deflate");
        let text = include_bytes!("../testdata/dynamic.txt");
        assert_eq!(inflate(dynamic, text.len()).unwrap(), text);
    }

    #[test]
    fn zip_round_trip_and_safety() {
        let zip = zip_write(&[("manifest.json", b"{}"), ("settings.json", "ş".as_bytes())]);
        let entries = zip_read(&zip).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].0, "settings.json");
        assert_eq!(entries[1].1, "ş".as_bytes());
        let evil = zip_write(&[("../x.txt", b"no")]);
        assert!(zip_read(&evil).is_none());
    }

    #[test]
    fn settings_backup_round_trip() {
        let store = SettingsStore::new(
            std::path::PathBuf::new(),
            ot_core::language::AppLanguage::english(),
        );
        let settings = DockSettings::default();
        let pins = vec![(
            "Notepad".to_owned(),
            r"C:\Windows\notepad.exe".to_owned(),
            None,
        )];
        let text = serialize(&settings, &pins);
        let (back, back_pins) = parse(&text, &store).unwrap();
        assert_eq!(back.edge, settings.edge);
        assert_eq!(back_pins, pins);
        assert!(parse("{\"Version\":1}", &store).is_err());
    }
}
