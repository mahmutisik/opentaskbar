//! Recovery journals next to the settings (`%LOCALAPPDATA%\OpenTaskBar-Rust`):
//! - `taskbar-state.txt`: Explorer's taskbar state before the dock took over. Its presence always means
//!   "the native taskbar may still be hidden"; it is deleted only after a verified restore.
//! - `flyout-positions.txt`: where Windows had placed Start/Search/Control Center before the dock moved them.
//!
//! Formats match OpenTaskbar so the files are easy to inspect.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ot_core::log;
use ot_core::store::app_data_dir;

use crate::appbar::SUPPORTED_STATE;
use crate::win::Hwnd;

pub fn taskbar_journal_path() -> PathBuf {
    app_data_dir().join("taskbar-state.txt")
}

pub fn flyout_journal_path() -> PathBuf {
    app_data_dir().join("flyout-positions.txt")
}

pub fn has_pending_recovery() -> bool {
    taskbar_journal_path().exists()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalError {
    Invalid,
    Read,
    Access,
    Write,
    WriteAccess,
    Delete,
}

impl JournalError {
    pub fn key(self) -> &'static str {
        match self {
            Self::Invalid => "ErrorTaskbarJournalInvalid",
            Self::Read => "ErrorTaskbarJournalRead",
            Self::Access => "ErrorTaskbarJournalAccess",
            Self::Write => "ErrorTaskbarJournalWrite",
            Self::WriteAccess => "ErrorTaskbarJournalWriteAccess",
            Self::Delete => "ErrorTaskbarJournalDelete",
        }
    }
}

/// Parses the saved state; only the auto-hide and always-on-top bits are legal.
pub fn parse_state(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let state: u32 = text.parse().ok()?;
    (state & !SUPPORTED_STATE == 0).then_some(state)
}

/// A missing or damaged journal restores a visible taskbar with default state, never a guessed hidden state.
pub fn fallback_state(text: Option<&str>) -> u32 {
    text.and_then(parse_state).unwrap_or(0)
}

pub fn read_state() -> Result<u32, JournalError> {
    match fs::read_to_string(taskbar_journal_path()) {
        Ok(text) => parse_state(&text).ok_or(JournalError::Invalid),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(JournalError::Access),
        Err(_) => Err(JournalError::Read),
    }
}

pub fn write_state(state: u32) -> Result<(), JournalError> {
    write_atomically(&taskbar_journal_path(), state.to_string().as_bytes()).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            JournalError::WriteAccess
        } else {
            JournalError::Write
        }
    })
}

pub fn delete_state() -> Result<(), JournalError> {
    match fs::remove_file(taskbar_journal_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(JournalError::Access),
        Err(_) => Err(JournalError::Delete),
    }
}

/// A flyout handle, its owning process and the position Windows gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlyoutPosition {
    pub handle: Hwnd,
    pub process_id: u32,
    pub left: i32,
    pub top: i32,
}

const MAX_FLYOUT_ENTRIES: usize = 16;

pub fn write_flyouts(positions: &[FlyoutPosition]) {
    let text: String = positions
        .iter()
        .take(MAX_FLYOUT_ENTRIES)
        .map(|p| {
            format!(
                "{} {} {} {}\r\n",
                p.handle as usize, p.process_id, p.left, p.top
            )
        })
        .collect();
    if let Err(error) = write_atomically(&flyout_journal_path(), text.as_bytes()) {
        log::warn(format!("Flyout position journal unavailable: {error}"));
    }
}

pub fn read_flyouts() -> Vec<FlyoutPosition> {
    let Ok(text) = fs::read_to_string(flyout_journal_path()) else {
        return Vec::new();
    };
    text.lines()
        .take(MAX_FLYOUT_ENTRIES)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let handle: usize = fields.next()?.parse().ok()?;
            let process_id: u32 = fields.next()?.parse().ok()?;
            let left: i32 = fields.next()?.parse().ok()?;
            let top: i32 = fields.next()?.parse().ok()?;
            (fields.next().is_none() && handle != 0).then_some(FlyoutPosition {
                handle: handle as Hwnd,
                process_id,
                left,
                top,
            })
        })
        .collect()
}

pub fn delete_flyouts() {
    if let Err(error) = fs::remove_file(flyout_journal_path())
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn(format!(
            "Flyout position journal could not be deleted: {error}"
        ));
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A name of its own per write (process id and time): two writers (the dock and its watchdog, or a leftover
    // temporary file from a crash) never share one.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}-{nanos:x}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = fs::write(&temporary, bytes).and_then(|()| match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        // Replacing can fail while another program holds the old file without delete sharing: remove it and
        // move the new one in (upstream's `write_state_to` fallback).
        Err(_) => fs::remove_file(path).and_then(|()| fs::rename(&temporary, path)),
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_parsing_is_strict() {
        assert_eq!(parse_state("0"), Some(0));
        assert_eq!(parse_state(" 3\r\n"), Some(3));
        assert_eq!(parse_state("4"), None);
        assert_eq!(parse_state("-1"), None);
        assert_eq!(parse_state("1.0"), None);
        assert_eq!(parse_state(""), None);
        assert_eq!(fallback_state(Some("garbage")), 0);
        assert_eq!(fallback_state(Some("2")), 2);
        assert_eq!(fallback_state(None), 0);
    }
}
