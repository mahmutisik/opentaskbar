//! What dock buttons do: the Windows shortcuts behind Start, Search, Task View and Show Desktop, opening the
//! Recycle Bin, launching or activating apps, and the dock menus' commands (system tools, power, close and
//! terminate).

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_D, VK_LWIN, VK_R, VK_S, VK_TAB,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::pins::Pin;

fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Presses `keys` in order and releases them in reverse (a chord such as Win+S). When Windows inserts only part of
/// the sequence (input blocked by UIPI or a secure desktop), the keys left pressed are released, so Win can never
/// stay held down.
pub fn send_chord(keys: &[VIRTUAL_KEY]) -> bool {
    let mut inputs: Vec<INPUT> = keys.iter().map(|&k| key(k, 0)).collect();
    inputs.extend(keys.iter().rev().map(|&k| key(k, KEYEVENTF_KEYUP)));
    // SAFETY: a valid array of keyboard inputs.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    } as usize;
    if sent == inputs.len() {
        return true;
    }
    let held = held_after_partial(keys.len(), sent);
    if held > 0 {
        let release: Vec<INPUT> = keys[..held]
            .iter()
            .rev()
            .map(|&k| key(k, KEYEVENTF_KEYUP))
            .collect();
        // SAFETY: as above.
        let released = unsafe {
            SendInput(
                release.len() as u32,
                release.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        ot_core::log::warn(format!(
            "Key chord partly sent ({sent}/{}); released {released} of {held} held keys.",
            inputs.len()
        ));
    }
    false
}

/// How many of a chord's first keys are still down after `sent` of its 2·`count` events (downs, then ups in
/// reverse) went through.
fn held_after_partial(count: usize, sent: usize) -> usize {
    if sent <= count {
        sent
    } else {
        (2 * count).saturating_sub(sent)
    }
}

/// After unlocking the session Windows can report Win, Ctrl, Shift or Alt as still pressed (the Win+L chord is cut
/// by the secure desktop): releases the ones that read as down.
pub fn release_stuck_modifiers() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    let stuck: Vec<INPUT> = [VK_LWIN, VK_RWIN, VK_CONTROL, VK_SHIFT, VK_MENU]
        .into_iter()
        // SAFETY: plain Win32 call.
        .filter(|&k| unsafe { GetAsyncKeyState(i32::from(k)) } < 0)
        .map(|k| key(k, KEYEVENTF_KEYUP))
        .collect();
    if stuck.is_empty() {
        return;
    }
    // SAFETY: a valid array of keyboard inputs.
    unsafe {
        SendInput(
            stuck.len() as u32,
            stuck.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    ot_core::log::info(format!(
        "Released {} modifier key(s) still down after unlock.",
        stuck.len()
    ));
}

pub fn open_start() -> bool {
    send_chord(&[VK_LWIN])
}

pub fn open_search() -> bool {
    send_chord(&[VK_LWIN, VK_S])
}

pub fn open_task_view() -> bool {
    send_chord(&[VK_LWIN, VK_TAB])
}

pub fn show_desktop() -> bool {
    send_chord(&[VK_LWIN, VK_D])
}

fn shell_execute_verb(verb: &str, target: &str, arguments: Option<&str>) -> bool {
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let verb = wide(verb);
    let target = wide(target);
    let arguments = arguments.map(wide);
    // SAFETY: null-terminated strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            arguments.as_ref().map_or(std::ptr::null(), |a| a.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as isize > 32
}

/// ShellExecute on a helper thread with COM initialized: launching can take long while shell extensions load.
fn spawn_shell_execute(target: String) {
    spawn_verb("open", target, None, "ErrorAppLaunch");
}

/// `failure`: the catalog key logged when the start fails (a cancelled elevation prompt is not a failure).
fn spawn_verb(
    verb: &'static str,
    target: String,
    arguments: Option<String>,
    failure: &'static str,
) {
    std::thread::spawn(move || {
        // SAFETY: COM for this short-lived thread; ShellExecute of shortcuts needs it.
        let _ = unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
        };
        if !shell_execute_verb(verb, &target, arguments.as_deref()) {
            // SAFETY: plain Win32 call.
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if error != 1223 {
                // ERROR_CANCELLED: the user declined the elevation prompt.
                ot_core::log::warn(format!(
                    "{failure}: failed to start {target} (error {error})"
                ));
            }
        }
    });
}

/// On a helper thread: opening a shell folder can block for a while, and the UI thread must stay free so the
/// click's bounce animation starts at once.
/// Files dropped on the Recycle Bin item go to the Recycle Bin, on a helper thread (the Shell may show progress or
/// a question): undoable and without the "move to Recycle Bin?" question, as a drop on Explorer's Recycle Bin, but
/// Windows still warns before a file that cannot be recycled would be destroyed.
pub fn recycle_files(files: Vec<String>) {
    let spawned = std::thread::Builder::new()
        .name("ot-recycle".into())
        .spawn(move || {
            use windows::Win32::UI::Shell::{
                FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW,
                SHFileOperationW,
            };
            // SAFETY: COM for the Shell on this thread.
            let _ = unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                )
            };
            // Paths separated by NULs and ended by two.
            let mut from: Vec<u16> = Vec::new();
            for file in &files {
                from.extend(file.encode_utf16());
                from.push(0);
            }
            from.push(0);
            let mut operation = SHFILEOPSTRUCTW {
                wFunc: FO_DELETE,
                pFrom: windows::core::PCWSTR(from.as_ptr()),
                fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING).0 as u16,
                ..Default::default()
            };
            // SAFETY: the structure and the path list outlive the call.
            let result = unsafe { SHFileOperationW(&mut operation) };
            if result == 0 && !operation.fAnyOperationsAborted.as_bool() {
                ot_core::log::info(format!(
                    "{} dropped item(s) moved to the Recycle Bin.",
                    files.len()
                ));
            } else {
                ot_core::log::warn(format!(
                    "Recycle Bin drop: {} item(s) not all recycled (code {result}).",
                    files.len()
                ));
            }
        });
    if let Err(error) = spawned {
        ot_core::log::warn(format!("Recycle Bin drop failed to start: {error}"));
    }
}

pub fn open_recycle_bin() -> bool {
    spawn_verb(
        "open",
        "shell:RecycleBinFolder".to_owned(),
        None,
        "ErrorSystemToolOpen",
    );
    true
}

/// Starts a pinned app through its shortcut, so arguments, working folder and identity match the taskbar's.
pub fn launch_pin(pin: &Pin) {
    spawn_shell_execute(pin.shortcut_path.to_string_lossy().into_owned());
}

/// Starts an executable (a running-only app with no pin).
pub fn launch_executable(path: &str) {
    spawn_shell_execute(path.to_owned());
}

/// Runs a pinned app's shortcut or an executable elevated ("Run as administrator").
pub fn launch_elevated(target: &str) {
    spawn_verb("runas", target.to_owned(), None, "ErrorAppLaunch");
}

/// Starts a system tool (`ShellShortcuts.OpenSystemTool`): an executable or console snap-in plus arguments.
pub fn open_system_tool(file: &str, arguments: Option<&str>, elevated: bool) {
    spawn_verb(
        if elevated { "runas" } else { "open" },
        file.to_owned(),
        arguments.map(str::to_owned),
        "ErrorSystemToolOpen",
    );
}

pub fn open_task_manager() {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    spawn_verb(
        "open",
        format!(r"{system}\System32\Taskmgr.exe"),
        None,
        "ErrorTaskManagerOpen",
    );
}

pub fn open_run_dialog() -> bool {
    send_chord(&[VK_LWIN, VK_R])
}

/// Asks every window to close (`Process.CloseMainWindow` sends WM_CLOSE). Returns how many accepted the message.
pub fn close_windows(windows: &[isize], owners: &[u32]) -> usize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    windows
        .iter()
        .zip(owners)
        // A window handle that now belongs to another process (closed and reused) is skipped.
        .filter(|&(&window, &owner)| owner != 0 && ot_shell::win::process_id(window) == owner)
        // SAFETY: posting to windows of other processes; a stale handle just fails.
        .filter(|&(&window, _)| unsafe { PostMessageW(window as _, WM_CLOSE, 0, 0) } != 0)
        .count()
}

/// `CloseApplication`: asks each process of `executable` to close its main window (`Process.CloseMainWindow`).
/// Returns (closed, failed); processes without a main window or that cannot be opened count as failed.
pub fn close_main_windows(executable: &str) -> (usize, usize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    let (processes, inaccessible) = processes_of(executable);
    let mut closed = 0;
    let mut failed = inaccessible;
    for pid in processes {
        match ot_shell::win::main_window_of_process(pid) {
            // SAFETY: posting to another process's window; a stale handle just fails.
            Some(window) if unsafe { PostMessageW(window as _, WM_CLOSE, 0, 0) } != 0 => {
                closed += 1
            }
            _ => failed += 1,
        }
    }
    (closed, failed)
}

/// Processes running `executable` (full path, case-insensitive): (process ids we may terminate, inaccessible count).
pub fn processes_of(executable: &str) -> (Vec<u32>, usize) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW,
    };
    let mut ids = vec![0u32; 4096];
    let mut bytes = 0u32;
    // SAFETY: the buffer size is passed in bytes.
    if unsafe { K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut bytes) } == 0 {
        return (Vec::new(), 0);
    }
    ids.truncate(bytes as usize / 4);
    let own = std::process::id();
    let mut found = Vec::new();
    let mut inaccessible = 0;
    for pid in ids.into_iter().filter(|&p| p != 0 && p != own) {
        // SAFETY: handles are closed below.
        unsafe {
            let query = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if query.is_null() {
                continue;
            }
            let mut buffer = [0u16; 1024];
            let mut size = buffer.len() as u32;
            let named = QueryFullProcessImageNameW(
                query,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            ) != 0;
            CloseHandle(query);
            if !named
                || !String::from_utf16_lossy(&buffer[..size as usize])
                    .eq_ignore_ascii_case(executable)
            {
                continue;
            }
            let terminate = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if terminate.is_null() {
                inaccessible += 1;
            } else {
                CloseHandle(terminate);
                found.push(pid);
            }
        }
    }
    (found, inaccessible)
}

/// Terminates the processes; returns how many were ended.
/// Ends the processes, each only if it still runs `executable`: the image is checked on the same handle that ends it,
/// so an id reused by another program after the prompt is never terminated.
pub fn terminate(pids: &[u32], executable: &str) -> usize {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess,
    };
    pids.iter()
        .filter(|&&pid| {
            // SAFETY: the handle is closed right after use; the name buffer and its size are passed together.
            unsafe {
                let handle = OpenProcess(
                    PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                );
                if handle.is_null() {
                    return false;
                }
                let mut buffer = [0u16; 1024];
                let mut size = buffer.len() as u32;
                let same = QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_WIN32,
                    buffer.as_mut_ptr(),
                    &mut size,
                ) != 0
                    && String::from_utf16_lossy(&buffer[..size as usize])
                        .eq_ignore_ascii_case(executable);
                let ended = same && TerminateProcess(handle, 1) != 0;
                if !same {
                    ot_core::log::warn(format!(
                        "End task: process {pid} no longer runs {executable}; skipped."
                    ));
                }
                CloseHandle(handle);
                ended
            }
        })
        .count()
}

#[cfg(test)]
mod chord_tests {
    #[test]
    fn held_keys_after_a_partial_chord() {
        // Win+S: down Win, down S, up S, up Win.
        assert_eq!(super::held_after_partial(2, 0), 0);
        assert_eq!(super::held_after_partial(2, 1), 1);
        assert_eq!(super::held_after_partial(2, 2), 2);
        assert_eq!(super::held_after_partial(2, 3), 1);
        assert_eq!(super::held_after_partial(2, 4), 0);
    }
}
