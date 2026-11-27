//! Pinned apps from Windows' own taskbar pins (`WindowsTaskbarPinImporter` in OpenTaskbar): the shortcuts in
//! `%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar`, ordered by the user's `PinOrder`
//! and then by name.

use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub executable_path: String,
    pub arguments: Option<String>,
    pub shortcut_path: PathBuf,
    pub icon_location: Option<String>,
    /// Packaged (Store) app identity, resolved once from the package manifest.
    pub app_user_model_id: Option<String>,
}

/// Windows' pin folder. `OT_PINS_FOLDER` points it elsewhere (tests must never edit the user's real pins).
pub fn pins_folder() -> Option<PathBuf> {
    if let Some(folder) = std::env::var_os("OT_PINS_FOLDER") {
        return Some(PathBuf::from(folder));
    }
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"),
    )
}

/// Reads the pinned shortcuts. COM must be initialized on the calling thread.
pub fn load(pin_order: Option<&[String]>) -> Vec<Pin> {
    let Some(folder) = pins_folder() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut shortcuts: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        })
        .collect();
    shortcuts.sort_by_key(|path| path.file_name().map(|n| n.to_string_lossy().to_lowercase()));

    let mut pins: Vec<Pin> = Vec::new();
    for shortcut in shortcuts {
        let Some(pin) = read_shortcut(&shortcut) else {
            continue;
        };
        if pins
            .iter()
            .any(|p| p.executable_path.eq_ignore_ascii_case(&pin.executable_path))
        {
            continue;
        }
        pins.push(pin);
    }
    if let Some(order) = pin_order {
        let rank = |pin: &Pin| {
            order
                .iter()
                .position(|path| path.eq_ignore_ascii_case(&pin.executable_path))
                .unwrap_or(usize::MAX)
        };
        pins.sort_by_key(rank); // stable: unordered pins keep their name order after the ordered ones
    }
    pins
}

/// Pins an app: writes `<name>.lnk` to the pin folder (`WindowsTaskbarPinImporter.AddPin`). COM must be
/// initialized on the calling thread. Returns the shortcut path or the catalog key of the error.
pub fn add(name: &str, executable: &str, arguments: Option<&str>) -> Result<PathBuf, &'static str> {
    let folder = pins_folder().ok_or("ErrorTaskbarPinFolderCreate")?;
    std::fs::create_dir_all(&folder).map_err(|_| "ErrorTaskbarPinFolderCreate")?;
    let invalid = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let mut safe: String = name
        .chars()
        .filter(|c| !invalid.contains(c) && !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned();
    if safe.is_empty() {
        safe = Path::new(executable)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "App".to_owned());
    }
    let shortcut = folder.join(format!("{safe}.lnk"));
    // SAFETY: COM calls on an in-process ShellLink object with valid strings.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|_| "ErrorShortcutServicePin")?;
        link.SetPath(&HSTRING::from(executable))
            .map_err(|_| "ErrorShortcutCreate")?;
        if let Some(dir) = Path::new(executable).parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
        }
        if let Some(arguments) = arguments.filter(|a| !a.trim().is_empty()) {
            let _ = link.SetArguments(&HSTRING::from(arguments));
        }
        let file: IPersistFile = link.cast().map_err(|_| "ErrorTaskbarPin")?;
        file.Save(&HSTRING::from(shortcut.as_os_str()), true)
            .map_err(|_| "ErrorTaskbarPin")?;
    }
    Ok(shortcut)
}

/// Unpins: deletes the shortcut, which must be a .lnk directly in the pin folder (`RemovePin`).
pub fn remove(shortcut: &Path) -> Result<(), &'static str> {
    let folder = pins_folder().ok_or("ErrorTaskbarPinPath")?;
    let in_folder = shortcut
        .parent()
        .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(folder.as_os_str()));
    let is_link = shortcut
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
    if !in_folder || !is_link {
        return Err("ErrorTaskbarPinPath");
    }
    std::fs::remove_file(shortcut).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => "ErrorTaskbarUnpinAccess",
        _ => "ErrorTaskbarUnpin",
    })
}

fn read_shortcut(path: &Path) -> Option<Pin> {
    // SAFETY: COM calls on an in-process ShellLink object; buffers are sized and null-terminated by the API.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(path.as_os_str()), STGM_READ)
            .ok()?;
        let mut target = [0u16; 1024];
        let mut find = WIN32_FIND_DATAW::default();
        link.GetPath(&mut target, &mut find, 0).ok()?;
        let mut arguments = [0u16; 1024];
        let _ = link.GetArguments(&mut arguments);
        let mut icon = [0u16; 1024];
        let mut icon_index = 0;
        let _ = link.GetIconLocation(&mut icon, &mut icon_index);
        let target = expand(&from_wide(&target));
        let icon = from_wide(&icon);
        let executable_path = resolve_executable(&target, &icon)?;
        let arguments = from_wide(&arguments);
        let app_user_model_id = packaged_app_id(&executable_path);
        Some(Pin {
            name: path.file_stem()?.to_string_lossy().into_owned(),
            executable_path,
            arguments: (!arguments.trim().is_empty()).then_some(arguments),
            shortcut_path: path.to_path_buf(),
            icon_location: (!icon.trim().is_empty()).then(|| format!("{icon},{icon_index}")),
            app_user_model_id,
        })
    }
}

/// An .exe target, or Explorer for the File Explorer pin (whose target is empty and icon is explorer.exe).
fn resolve_executable(target: &str, icon: &str) -> Option<String> {
    let target = target.trim().trim_matches('"');
    if target.to_ascii_lowercase().ends_with(".exe") && Path::new(target).exists() {
        return Some(target.to_owned());
    }
    if target.is_empty() {
        let icon = expand(
            icon.split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"'),
        );
        if icon.to_ascii_lowercase().ends_with("\\explorer.exe") && Path::new(&icon).exists() {
            return Some(icon);
        }
    }
    None
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Expands `%VARIABLE%` references.
pub fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The AppUserModelID of an executable inside `WindowsApps\<PackageFullName>\...`, from its `AppxManifest.xml`
/// (`TryResolvePackagedAppUserModelId` in OpenTaskbar). Resolved once per pin, not per window event.
pub fn packaged_app_id(executable_path: &str) -> Option<String> {
    let lower = executable_path.to_lowercase();
    if !lower.contains(r"\windowsapps\") {
        return None;
    }
    let exe = Path::new(executable_path);
    let mut dir = exe.parent();
    let root = loop {
        let current = dir?;
        if current
            .file_name()?
            .to_string_lossy()
            .eq_ignore_ascii_case("WindowsApps")
        {
            return None;
        }
        if current.join("AppxManifest.xml").exists() {
            break current;
        }
        dir = current.parent();
    };
    // Package folder: <Name>_<Version>_<Architecture>_<ResourceId>_<PublisherId>
    let folder = root.file_name()?.to_string_lossy().into_owned();
    let parts: Vec<&str> = folder.split('_').collect();
    if parts.len() < 5 || parts[0].is_empty() || parts[parts.len() - 1].is_empty() {
        return None;
    }
    let family = format!("{}_{}", parts[0], parts[parts.len() - 1]);
    let manifest = std::fs::read_to_string(root.join("AppxManifest.xml")).ok()?;
    let relative = exe
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('/', "\\");
    let applications = manifest_applications(&manifest);
    let id = applications
        .iter()
        .find(|(_, executable)| {
            executable
                .as_deref()
                .is_some_and(|e| e.replace('/', "\\").eq_ignore_ascii_case(&relative))
        })
        .or_else(|| (applications.len() == 1).then(|| &applications[0]))
        .map(|(id, _)| id.clone())?;
    Some(format!("{family}!{id}"))
}

/// `(Id, Executable)` of every `<Application>` element (any namespace prefix) in a package manifest.
fn manifest_applications(manifest: &str) -> Vec<(String, Option<String>)> {
    let mut applications = Vec::new();
    let mut rest = manifest;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        rest = &rest[end..];
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        if local != "Application" {
            continue;
        }
        if let Some(id) = attribute(tag, "Id").filter(|id| !id.trim().is_empty()) {
            applications.push((
                id.trim().to_owned(),
                attribute(tag, "Executable").map(|e| e.trim().to_owned()),
            ));
        }
    }
    applications
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(position) = rest.find(name) {
        let before_ok = position == 0 || rest.as_bytes()[position - 1].is_ascii_whitespace();
        let after = rest[position + name.len()..].trim_start();
        if before_ok && let Some(value) = after.strip_prefix('=') {
            let value = value.trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &value[1..];
                return inner.find(quote).map(|end| inner[..end].to_owned());
            }
        }
        rest = &rest[position + name.len()..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_applications_are_read() {
        let manifest = r#"<Package><Applications>
            <Application Id="App" Executable="app\Claude.exe" EntryPoint="Windows.FullTrustApplication">
              <uap:VisualElements DisplayName="Claude" />
            </Application>
            <Application Id='Helper' Executable='helper.exe'/>
            <uap10:Application Id="Other" />
        </Applications></Package>"#;
        let apps = manifest_applications(manifest);
        assert_eq!(
            apps,
            vec![
                ("App".to_owned(), Some(r"app\Claude.exe".to_owned())),
                ("Helper".to_owned(), Some("helper.exe".to_owned())),
                ("Other".to_owned(), None),
            ]
        );
    }

    #[test]
    fn non_packaged_paths_are_skipped() {
        assert_eq!(packaged_app_id(r"C:\Program Files\App\app.exe"), None);
    }
}
