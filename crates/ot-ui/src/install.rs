//! The wizard's install step: copies the executable to `%LOCALAPPDATA%\Programs\OpenTaskBar-Rust` and, when asked,
//! registers it to start with Windows (HKCU Run, value `OpenTaskBar-Rust`, like OpenTaskbar's `OpenTaskbar` value).
//! A dry run (development) only logs what it would do.

use std::path::PathBuf;

use ot_core::log;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW,
    RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};

/// `StartupRegistration.Read`: whether the HKCU Run value exists (non-empty).
pub fn starts_with_windows() -> bool {
    let key: Vec<u16> = RUN_KEY.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(Some(0)).collect();
    let mut size = 0u32;
    // SAFETY: a size query with null-terminated strings.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    status == 0 && size > 2
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "OpenTaskBar-Rust";

pub fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(r"Programs\OpenTaskBar-Rust")
}

/// Copies the running executable into the install folder (unless it already runs from there) and returns the
/// installed path.
pub fn install(dry_run: bool) -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = install_dir().join("OpenTaskBar.exe");
    if dry_run {
        log::info(format!(
            "Wizard (dry run): would copy {} to {}",
            current.display(),
            target.display()
        ));
        return Ok(target);
    }
    if current.canonicalize().ok() == target.canonicalize().ok() {
        return Ok(target);
    }
    std::fs::create_dir_all(install_dir()).map_err(|e| e.to_string())?;
    let temporary = target.with_extension("exe.tmp");
    std::fs::copy(&current, &temporary).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &target).map_err(|e| e.to_string())?;
    log::info(format!("Wizard: installed to {}", target.display()));
    Ok(target)
}

/// Adds or removes the HKCU Run entry.
pub fn set_start_with_windows(
    executable: &std::path::Path,
    enabled: bool,
    dry_run: bool,
) -> Result<(), String> {
    if dry_run {
        log::info(format!(
            "Wizard (dry run): would set start with Windows = {enabled}"
        ));
        return Ok(());
    }
    let key: Vec<u16> = RUN_KEY.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(Some(0)).collect();
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: registry calls with null-terminated strings; the key is closed below.
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut handle,
        ) != 0
        {
            return Err("ErrorStartupRegistration".to_owned());
        }
        let status = if enabled {
            let value: Vec<u16> = format!("\"{}\"", executable.display())
                .encode_utf16()
                .chain(Some(0))
                .collect();
            RegSetValueExW(
                handle,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        } else {
            let status = RegDeleteValueW(handle, name.as_ptr());
            if status == 2 { 0 } else { status } // ERROR_FILE_NOT_FOUND: already absent
        };
        RegCloseKey(handle);
        if status == 0 {
            Ok(())
        } else {
            Err("ErrorStartupRegistration".to_owned())
        }
    }
}
