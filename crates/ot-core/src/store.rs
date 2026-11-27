//! Loading and saving `settings.json`, and first-run detection.
//!
//! First run means the settings file does not exist yet. The wizard writes the file when it finishes (or is
//! skipped), so a user who closes the wizard early sees it again on the next start.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::language::AppLanguage;
use crate::settings::DockSettings;
use crate::{migrate, profiles, validate};

/// Settings folder name under `%LOCALAPPDATA%`. Separate from OpenTaskbar's so both can be installed.
pub const APP_FOLDER: &str = "OpenTaskBar-Rust";
pub const SETTINGS_FILE: &str = "settings.json";
pub const LOG_FILE: &str = "log.txt";

/// `%LOCALAPPDATA%\OpenTaskBar-Rust`: settings, log and recovery journals live here.
pub fn app_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_FOLDER)
}

/// Why settings could not be loaded or saved. The `key` is the localization key OpenTaskbar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsError {
    /// The file is not valid JSON or has values of the wrong type.
    Read,
    /// The file parsed but breaks a validation rule.
    Invalid,
    Disk,
    Access,
    /// Saving was refused because the values break a validation rule.
    ValuesInvalid,
    Save,
    WriteAccess,
}

impl SettingsError {
    pub fn key(self) -> &'static str {
        match self {
            Self::Read => "ErrorDockSettingsRead",
            Self::Invalid => "ErrorDockSettingsInvalid",
            Self::Disk => "ErrorDockSettingsDisk",
            Self::Access => "ErrorDockSettingsAccess",
            Self::ValuesInvalid => "ErrorSettingsValuesInvalid",
            Self::Save => "ErrorDockSettingsSave",
            Self::WriteAccess => "ErrorDockSettingsWriteAccess",
        }
    }
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub settings: DockSettings,
    /// Set when the file existed but could not be used; `settings` then holds defaults.
    pub error: Option<SettingsError>,
    /// True when no settings file existed: this is the first start and the setup wizard should run.
    pub first_run: bool,
    /// Where an unusable file was copied before falling back to defaults, so the user's data is never lost.
    pub backup: Option<PathBuf>,
}

pub struct SettingsStore {
    path: PathBuf,
    default_language: AppLanguage,
}

impl SettingsStore {
    /// The store at `%LOCALAPPDATA%\OpenTaskBar-Rust\settings.json`, with the system UI language as default.
    pub fn for_current_user() -> Self {
        Self::new(
            app_data_dir().join(SETTINGS_FILE),
            AppLanguage::from_culture(&crate::language::system_ui_culture()),
        )
    }

    pub fn new(path: PathBuf, default_language: AppLanguage) -> Self {
        Self {
            path,
            default_language,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_first_run(&self) -> bool {
        !self.path.exists()
    }

    /// Defaults for a new user: system language and a Default profile.
    pub fn defaults(&self) -> DockSettings {
        profiles::ensure_default_profile(DockSettings {
            language: self.default_language.clone(),
            ..DockSettings::default()
        })
    }

    pub fn load(&self) -> LoadOutcome {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return LoadOutcome {
                    settings: self.defaults(),
                    error: None,
                    first_run: true,
                    backup: None,
                };
            }
            Err(error) => {
                let kind = match error.kind() {
                    io::ErrorKind::PermissionDenied => SettingsError::Access,
                    io::ErrorKind::InvalidData => SettingsError::Read,
                    _ => SettingsError::Disk,
                };
                return self.fallback(kind, None);
            }
        };
        match self.parse(&text) {
            Ok(settings) => LoadOutcome {
                settings,
                error: None,
                first_run: false,
                backup: None,
            },
            Err(kind) => {
                let backup = self.backup_unusable_file();
                self.fallback(kind, backup)
            }
        }
    }

    /// Parses, migrates and validates settings JSON.
    pub fn parse(&self, text: &str) -> Result<DockSettings, SettingsError> {
        // A UTF-8 byte order mark is legal in files written by other tools.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Ok(Value::Object(mut root)) = serde_json::from_str::<Value>(text) else {
            return Err(SettingsError::Read);
        };
        migrate::normalize_legacy_json(&mut root);
        let mut settings: DockSettings =
            serde_json::from_value(Value::Object(root.clone())).map_err(|_| SettingsError::Read)?;
        // Before the migration, which copies these values into the default profile.
        let clamped = validate::clamp_numbers(&mut settings);
        let settings = migrate::apply(settings, &root, &self.default_language);
        if !clamped.is_empty() {
            crate::log::warn(format!(
                "Settings: out-of-range values brought into range: {}.",
                clamped.join(", ")
            ));
        }
        match validate::first_violation(&settings) {
            None => Ok(settings),
            Some(rule) => {
                crate::log::warn(format!("Settings file rejected by the rule \"{rule}\"."));
                Err(SettingsError::Invalid)
            }
        }
    }

    /// Validates, syncs the active profile and writes atomically (temp file + replace).
    pub fn save(&self, settings: &DockSettings) -> Result<DockSettings, SettingsError> {
        let settings = profiles::sync_active(settings.clone());
        if !validate::is_valid(&settings) {
            return Err(SettingsError::ValuesInvalid);
        }
        let json = serde_json::to_string_pretty(&settings).map_err(|_| SettingsError::Save)?;
        write_atomically(&self.path, json.as_bytes()).map_err(|error| match error.kind() {
            io::ErrorKind::PermissionDenied => SettingsError::WriteAccess,
            _ => SettingsError::Save,
        })?;
        Ok(settings)
    }

    fn fallback(&self, error: SettingsError, backup: Option<PathBuf>) -> LoadOutcome {
        LoadOutcome {
            settings: self.defaults(),
            error: Some(error),
            first_run: false,
            backup,
        }
    }

    /// Copies an unreadable or invalid file aside (`settings.json.invalid-<unix seconds>`) before the next save
    /// overwrites it with defaults.
    fn backup_unusable_file(&self) -> Option<PathBuf> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut name = self.path.file_name()?.to_os_string();
        name.push(format!(".invalid-{stamp}"));
        let backup = self.path.with_file_name(name);
        fs::copy(&self.path, &backup).ok().map(|_| backup)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no folder"))?;
    fs::create_dir_all(directory)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut temporary_name = path.file_name().unwrap_or_default().to_os_string();
    temporary_name.push(format!(".{}-{nanos}.tmp", std::process::id()));
    let temporary = path.with_file_name(temporary_name);
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // On Windows `rename` replaces the target (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Raw access for tools and tests: the normalized JSON object a file would be migrated from.
pub fn normalized_root(text: &str) -> Option<Map<String, Value>> {
    let Value::Object(mut root) = serde_json::from_str(text).ok()? else {
        return None;
    };
    migrate::normalize_legacy_json(&mut root);
    Some(root)
}
