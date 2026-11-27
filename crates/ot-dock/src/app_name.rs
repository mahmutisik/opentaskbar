//! Display names of running apps: the executable's FileDescription as OpenTaskbar's `GetApplicationName` reads it
//! (falling back to the file name), and for packaged apps the name Windows shows in its Apps folder.

use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};
use windows::core::{HSTRING, PCWSTR};

/// FileDescription of an executable, trimmed; `None` when the file has no version resource or no description.
pub fn file_description(path: &str) -> Option<String> {
    let file = HSTRING::from(path);
    // SAFETY: the version block is sized by GetFileVersionInfoSizeW and outlives every pointer VerQueryValueW
    // returns into it.
    unsafe {
        let size = GetFileVersionInfoSizeW(&file, None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(&file, None, size, block.as_mut_ptr().cast()).ok()?;
        // The first translation (language, code page) names the string table to read.
        let mut translation: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut length = 0u32;
        let found = VerQueryValueW(
            block.as_ptr().cast(),
            &HSTRING::from(r"\VarFileInfo\Translation"),
            &mut translation,
            &mut length,
        );
        let tables: Vec<String> = if found.as_bool() && length >= 4 && !translation.is_null() {
            let words =
                std::slice::from_raw_parts(translation.cast::<u16>(), (length / 2) as usize);
            words
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| format!("{:04x}{:04x}", pair[0], pair[1]))
                .collect()
        } else {
            vec!["040904b0".to_owned(), "040904e4".to_owned()]
        };
        for table in tables {
            let key = HSTRING::from(format!(r"\StringFileInfo\{table}\FileDescription"));
            let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut chars = 0u32;
            if VerQueryValueW(block.as_ptr().cast(), &key, &mut value, &mut chars).as_bool()
                && !value.is_null()
                && chars > 0
            {
                let text = PCWSTR(value.cast()).to_string().ok()?;
                let text = text.trim();
                if !text.is_empty() {
                    return Some(text.to_owned());
                }
            }
        }
        None
    }
}

/// The name of a packaged app as the Apps folder shows it (`shell:AppsFolder\<AUMID>`).
pub fn packaged_name(app_user_model_id: &str) -> Option<String> {
    shell_display_name(&format!(r"shell:AppsFolder\{app_user_model_id}"))
}

/// The display name of a shell item by parsing name (e.g. the Recycle Bin's CLSID path).
pub fn shell_display_name(parsing_name: &str) -> Option<String> {
    // SAFETY: COM is initialized on the dock thread; the returned string is freed by `to_string` + CoTaskMemFree.
    unsafe {
        let item: IShellItem =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let text = name.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(name.0.cast()));
        text.filter(|t| !t.trim().is_empty())
    }
}

/// A running app's display name: packaged name, FileDescription, then the file name.
pub fn display_name(executable: &str, app_id: Option<&str>) -> String {
    app_id
        .and_then(packaged_name)
        .or_else(|| file_description(executable))
        .unwrap_or_else(|| {
            std::path::Path::new(executable)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_system_file_description() {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let name = file_description(&format!(r"{system}\System32\notepad.exe"));
        assert!(name.is_some_and(|n| !n.is_empty()));
        assert_eq!(
            display_name(r"C:\nowhere\Missing App.exe", None),
            "Missing App"
        );
    }
}
