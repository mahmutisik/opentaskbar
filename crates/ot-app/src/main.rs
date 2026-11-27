//! OpenTaskBar-Rust entry point.
//!
//! Modes:
//! - `--watchdog=<pid>:<token>`: out-of-process recovery for a running dock (started by the dock itself).
//! - `--restore`: puts Explorer's taskbar back from the recovery journal and exits.
//! - `--shell-test[=seconds]` (dev): placeholder dock window driven by the real taskbar session;
//!   `--crash-after=<seconds>` panics inside it to test the panic restore path.
//! - `--wizard` runs the setup wizard again; `--wizard-dry-run` (dev) shows it without copying the program,
//!   registering startup or saving settings (the dock still appears after the install step and closes at the end);
//!   `--wizard-demo` (dev) is a dry run that walks through every screen by itself.
//! - `--info`: prints settings, log and language details and exits.
//! - default: log, single instance, startup heal, settings; the setup wizard on first run; then the dock.
//!
//! The executable is a Windows (GUI) program, so starting it never opens a console window. When it is started
//! with arguments from a terminal (the development flags above), it attaches to that terminal for its output.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod single_instance;

#[cfg(windows)]
mod shell_test;

use std::time::Duration;

use ot_core::store::{LOG_FILE, app_data_dir};
use ot_core::{Catalog, SettingsStore, log, startup};
use single_instance::SingleInstance;

fn main() {
    // The icon worker (started by the dock with pipes): nothing else runs, no log, no console attachment.
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some(ot_dock::icon_loader::WORKER_FLAG) {
        ot_dock::icon_loader::run_worker();
        return;
    }
    startup::begin();
    // Settings' "Hardware acceleration" (`RenderModePreference`): the renderer reads it as `OT_RENDER_HARDWARE`.
    #[cfg(windows)]
    if std::env::var_os("OT_RENDER_HARDWARE").is_none() && ot_ui::settings::hardware_rendering() {
        // SAFETY: the first statement of main, before any other thread exists.
        unsafe { std::env::set_var("OT_RENDER_HARDWARE", "1") };
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    attach_parent_console(&args);
    let dev = args.iter().any(|arg| arg == "--dev");
    log::init(&app_data_dir().join(LOG_FILE));

    #[cfg(windows)]
    {
        if ot_shell::watchdog::is_watchdog_invocation(&args) {
            ot_shell::watchdog::run(&args);
            log::shutdown();
            return;
        }
        enable_per_monitor_dpi();
    }

    log::info(format!(
        "OpenTaskBar-Rust {} started.",
        env!("CARGO_PKG_VERSION")
    ));
    // Icons are extracted in short-lived worker processes of this executable.
    #[cfg(windows)]
    if !args.iter().any(|a| a == "--icon-test=inproc")
        && let Ok(exe) = std::env::current_exe()
    {
        ot_dock::icon_loader::enable_worker(exe);
    }
    // Dev: `--icon-test=worker|inproc` loads the pinned apps' icons (rest and hover sizes) and prints the time and
    // this process's private memory, then again after the worker's idle exit. `OT_ICON_CACHE=0` skips the disk cache.
    #[cfg(windows)]
    if args.iter().any(|a| a.starts_with("--icon-test")) {
        icon_test();
        log::shutdown();
        return;
    }
    install_panic_hook();
    startup::mark("main entered");

    #[cfg(windows)]
    if args.iter().any(|arg| arg == "--restore") {
        let result = ot_shell::restore::restore("cli-restore");
        // Tray icons left with a dock that did not exit cleanly go back to Explorer.
        ot_shell::tray::broadcast_taskbar_created(true);
        if let Err(error) = result {
            log::error(format!("--restore failed: {error}"));
        }
        println!("restore: {}", result.err().unwrap_or("ok"));
        log::shutdown();
        std::process::exit(i32::from(result.is_err()));
    }

    let phase = startup::Phase::start("settings");
    let store = SettingsStore::for_current_user();
    let outcome = store.load();
    phase.finish("loaded");
    let language = outcome.settings.language.clone();

    let lock_name = if dev {
        single_instance::DEV_NAME
    } else {
        single_instance::NAME
    };
    // `--reset-setup`: started by the dock that is closing for "Reset and run setup again"; wait for it to let go.
    let reset_setup = args.iter().any(|arg| arg == "--reset-setup");
    let mut instance = SingleInstance::acquire(lock_name);
    if reset_setup {
        for _ in 0..100 {
            if instance.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            instance = SingleInstance::acquire(lock_name);
        }
    }
    let Some(_instance) = instance else {
        log::report_error("AlreadyRunning", &language, None);
        println!("{}", Catalog::get("AlreadyRunning", &language));
        log::shutdown();
        return;
    };

    // Heal only after winning the single-instance race: a second instance must never consume the live
    // instance's recovery journal.
    #[cfg(windows)]
    if let Err(error) = ot_shell::restore::heal_pending("startup-heal") {
        log::report_error(error, &language, None);
    }

    // The reset itself: the old settings are kept beside the file, then it goes, so setup runs as on first run.
    let outcome = if reset_setup {
        let path = store.path().to_path_buf();
        if path.exists() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = path.with_file_name(format!("settings.json.before-reset-{stamp}"));
            match std::fs::copy(&path, &backup).and_then(|_| std::fs::remove_file(&path)) {
                Ok(()) => log::info(format!(
                    "Settings reset for setup; previous settings kept at {}.",
                    backup.display()
                )),
                Err(error) => log::warn(format!("Settings reset failed: {error}")),
            }
        }
        store.load()
    } else {
        outcome
    };
    if let Some(error) = outcome.error {
        let details = outcome
            .backup
            .as_ref()
            .map(|path| format!("unusable file kept at {}", path.display()));
        log::report_error(error.key(), &language, details.as_deref());
    }
    if outcome.first_run {
        log::info("First run: no settings file yet; the setup wizard will run.");
    }
    startup::mark("ready");

    #[cfg(windows)]
    if let Some(run_for) = args.iter().find_map(|a| {
        if a == "--dock" {
            Some(None)
        } else {
            a.strip_prefix("--dock=").map(|v| v.parse().ok())
        }
    }) {
        // Dev: `--use-settings=<path>` reads another settings file (e.g. OpenTaskbar's) without writing it.
        let settings = match args.iter().find_map(|a| a.strip_prefix("--use-settings=")) {
            Some(path) => {
                SettingsStore::new(path.into(), outcome.settings.language.clone())
                    .load()
                    .settings
            }
            None => {
                // The app's own settings: what the dock changes itself (pins) is saved.
                ot_dock::set_persist(save_settings);
                outcome.settings.clone()
            }
        };
        ot_dock::set_settings_opener(ot_ui::settings::open);
        if args.iter().any(|a| a == "--settings") {
            ot_dock::open_settings_on_start();
        }
        let result = ot_dock::run(settings, run_for);
        println!("dock: {}", result.err().unwrap_or("ok"));
        log::shutdown();
        std::process::exit(i32::from(result.is_err()));
    }

    #[cfg(windows)]
    if let Some(seconds) = shell_test_seconds(&args) {
        let crash_after = args
            .iter()
            .find_map(|a| a.strip_prefix("--crash-after="))
            .and_then(|v| v.parse().ok());
        let result = shell_test::run(seconds, crash_after);
        println!("shell test: {}", result.err().unwrap_or("ok"));
        log::shutdown();
        std::process::exit(i32::from(result.is_err()));
    }

    #[cfg(windows)]
    if let Some(seconds) = args.iter().find_map(|a| {
        if a == "--tracker-test" {
            Some(5)
        } else {
            a.strip_prefix("--tracker-test=")
                .and_then(|v| v.parse().ok())
        }
    }) {
        tracker_test(seconds);
        log::shutdown();
        return;
    }
    #[cfg(windows)]
    if let Some(seconds) = args.iter().find_map(|a| {
        a.strip_prefix("--tray-test=")
            .and_then(|v| v.parse::<u64>().ok())
    }) {
        tray_test(seconds);
        log::shutdown();
        return;
    }

    #[cfg(windows)]
    if !args.iter().any(|arg| arg == "--info") {
        let flag = |name: &str| args.iter().any(|arg| arg == name);
        let demo = flag("--wizard-demo");
        let dry_run = demo || flag("--wizard-dry-run");
        let needs_wizard = outcome.first_run
            || outcome.settings.onboarding_version < ot_ui::wizard::ONBOARDING_VERSION;
        let settings = if dry_run || flag("--wizard") || needs_wizard {
            match ot_ui::run(ot_ui::WizardOptions {
                store: SettingsStore::for_current_user(),
                settings: outcome.settings.clone(),
                dry_run,
                demo,
            }) {
                ot_ui::WizardOutcome::DockRunning(settings)
                | ot_ui::WizardOutcome::Skipped(settings) => {
                    log::info(format!(
                        "Wizard closed: {:.1} MB private.",
                        ot_dock::dock::private_mb()
                    ));
                    // Dev: `--wizard-keep-dock=<seconds>` keeps the dock the dry run started (nothing saved), to
                    // check that the wizard's memory and CPU are gone once it closes.
                    let keep = args
                        .iter()
                        .find_map(|a| a.strip_prefix("--wizard-keep-dock="))
                        .and_then(|v| v.parse::<u32>().ok());
                    if dry_run && let Some(seconds) = keep {
                        let result = ot_dock::run(settings, Some(seconds));
                        println!(
                            "wizard (dry run, dock kept): {}",
                            result.err().unwrap_or("ok")
                        );
                        log::shutdown();
                        return;
                    }
                    if dry_run {
                        let result = ot_dock::stop();
                        println!("wizard (dry run): {}", result.err().unwrap_or("ok"));
                        log::shutdown();
                        return;
                    }
                    settings
                }
                ot_ui::WizardOutcome::Cancelled => {
                    if dry_run {
                        let _ = ot_dock::stop();
                    }
                    println!("wizard: cancelled");
                    log::shutdown();
                    return;
                }
            }
        } else {
            outcome.settings.clone()
        };
        // Starts the dock, or keeps the one the wizard started, and runs until it closes.
        ot_dock::set_persist(save_settings);
        ot_dock::set_settings_opener(ot_ui::settings::open);
        if args.iter().any(|a| a == "--settings") {
            ot_dock::open_settings_on_start();
        }
        let result = ot_dock::run(settings, None);
        if let Err(error) = result {
            log::report_error(error, &language, None);
        }
        if ot_dock::setup_restart_requested() {
            // The new instance waits for this one's lock, resets the settings and runs setup.
            drop(_instance);
            match std::env::current_exe()
                .and_then(|exe| std::process::Command::new(exe).arg("--reset-setup").spawn())
            {
                Ok(_) => log::info("Restarting into setup."),
                Err(error) => log::error(format!("Setup restart failed: {error}")),
            }
        }
        log::shutdown();
        std::process::exit(i32::from(result.is_err()));
    }

    println!("settings: {}", store.path().display());
    println!("log: {}", app_data_dir().join(LOG_FILE).display());
    println!("first run: {}", outcome.first_run);
    if let Some(error) = outcome.error {
        println!(
            "load error: {} ({})",
            error.key(),
            Catalog::get(error.key(), &language)
        );
    }
    println!(
        "language: {} ({})",
        language,
        Catalog::get("Settings", &language)
    );
    println!("ready in {:.2} ms", startup::elapsed_ms());

    if args.iter().any(|arg| arg == "--hold") {
        // Keeps the instance lock so a second start can be tried by hand.
        std::thread::sleep(Duration::from_secs(10));
    }
    log::shutdown();
}

#[cfg(windows)]
fn shell_test_seconds(args: &[String]) -> Option<u32> {
    args.iter().find_map(|arg| {
        if arg == "--shell-test" {
            Some(10)
        } else {
            arg.strip_prefix("--shell-test=")
                .and_then(|v| v.parse().ok())
        }
    })
}

/// Physical-pixel coordinates on every monitor (Per-Monitor v2), as the WPF app declares in its manifest.
#[cfg(windows)]
fn enable_per_monitor_dpi() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: plain Win32 call; fails harmlessly if awareness was already set.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Logs panics and, when the taskbar may be hidden, restores it before the process ends. The restore is
/// journal-based, so it works even if the panicking thread was in the middle of a session call.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    // Installed from `main`: this is the dock's UI thread.
    let main_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        let on_main = std::thread::current().id() == main_thread;
        log::error(format!(
            "Unhandled panic on {}: {info}",
            std::thread::current().name().unwrap_or("an unnamed thread")
        ));
        // Only a panic on the UI thread ends the dock (it cannot unwind out of the window procedure). A worker
        // thread's panic leaves the dock running: restoring here would bring Explorer's taskbar back under a live
        // dock and drop its AppBar reservation.
        #[cfg(windows)]
        if on_main && ot_shell::journal::has_pending_recovery() {
            // No clean-exit signal afterwards: the watchdog still sees the dock die and then broadcasts
            // TaskbarCreated, once this process's tray window is gone, so tray icons return to Explorer.
            if let Err(error) = ot_shell::restore::restore("panic") {
                log::error(format!(
                    "Panic restore failed: {error}; the watchdog will retry."
                ));
            }
        }
        log::flush(Duration::from_secs(1));
        previous(info);
    }));
}

#[cfg(windows)]
fn icon_test() {
    use std::sync::mpsc;
    // SAFETY: COM for reading the pinned shortcuts on this thread.
    let _ = unsafe {
        windows_sys::Win32::System::Com::CoInitializeEx(
            std::ptr::null(),
            windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32,
        )
    };
    let names: Vec<String> = ot_dock::pins::load(None)
        .iter()
        .map(|p| ot_dock::icons::parsing_name(&p.executable_path, p.app_user_model_id.as_deref()))
        .collect();
    let before = ot_dock::dock::private_mb();
    let started = std::time::Instant::now();
    let (done, finished) = mpsc::channel();
    let mut loader = ot_dock::icon_loader::IconLoader::new(move || {
        let _ = done.send(());
    });
    let mut wanted = 0;
    for name in &names {
        for size in [28, 42] {
            loader.get(name, size);
            wanted += 1;
        }
    }
    let mut answered = 0;
    while answered < wanted && finished.recv_timeout(Duration::from_secs(30)).is_ok() {
        answered += 1;
    }
    let elapsed = started.elapsed();
    loader.drain();
    let loaded = names
        .iter()
        .flat_map(|n| [28, 42].map(|size| loader.get(n, size)))
        .filter(Option::is_some)
        .count();
    let after = ot_dock::dock::private_mb();
    std::thread::sleep(Duration::from_secs(6));
    let idle = ot_dock::dock::private_mb();
    println!(
        "icons: {loaded}/{wanted} in {} ms; private memory {before:.1} MB before, {after:.1} MB after, {idle:.1} MB 6 s later",
        elapsed.as_millis()
    );
}

/// `--tray-test=<seconds>` (dev): hosts the notification area alone for a while and prints the icons it collected.
#[cfg(windows)]
fn tray_test(seconds: u64) {
    use windows_sys::Win32::UI::WindowsAndMessaging as wm;
    let started = std::time::Instant::now();
    if !ot_shell::tray::start(|| {}) {
        println!("tray host failed to start");
        return;
    }
    let mut msg: wm::MSG = unsafe { std::mem::zeroed() };
    while started.elapsed().as_secs() < seconds {
        // SAFETY: standard non-blocking message pump.
        unsafe {
            while wm::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, wm::PM_REMOVE) != 0 {
                wm::TranslateMessage(&msg);
                wm::DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    ot_shell::tray::with_icons(|icons| {
        println!(
            "icons: {} ({} app icons)",
            icons.len(),
            icons.iter().filter(|i| i.is_app_icon()).count()
        );
        for icon in icons {
            println!(
                "  v{} hidden={} icon={} guid={} uid={} tip={:?} exe={}",
                icon.version,
                icon.hidden,
                icon.icon.is_some(),
                if icon.guid.is_empty() {
                    "-"
                } else {
                    &icon.guid
                },
                icon.uid,
                icon.tip,
                icon.executable_path.as_deref().unwrap_or("?")
            );
        }
    });
    ot_shell::tray::stop();
}

/// `--tracker-test[=seconds]` (dev): runs the window tracker and prints what it found and how long scans took.
#[cfg(windows)]
fn tracker_test(seconds: u64) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ot_shell::tracker::{TrackerEvent, WindowTracker};

    let changes = Arc::new(AtomicUsize::new(0));
    let foregrounds = Arc::new(AtomicUsize::new(0));
    let (c, f) = (changes.clone(), foregrounds.clone());
    let started = std::time::Instant::now();
    let tracker = match WindowTracker::start(0, move |event| match event {
        TrackerEvent::WindowsChanged(_) => {
            c.fetch_add(1, Ordering::Relaxed);
        }
        TrackerEvent::ForegroundChanged(_) => {
            f.fetch_add(1, Ordering::Relaxed);
        }
        TrackerEvent::TaskbarShown { .. } => {}
    }) {
        Ok(tracker) => tracker,
        Err(error) => {
            println!("tracker failed: {error}");
            return;
        }
    };
    println!(
        "tracker ready in {:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    std::thread::sleep(Duration::from_secs(seconds));
    let snapshot = tracker.snapshot();
    let stats = tracker.stats();
    println!("windows: {}", snapshot.len());
    for window in snapshot.iter() {
        let exe = window
            .executable_path
            .as_deref()
            .and_then(|p| p.rsplit(['\\', '/']).next())
            .unwrap_or("?");
        println!(
            "  {exe:<28} aumid={}",
            window.app_user_model_id.as_deref().unwrap_or("-")
        );
    }
    println!(
        "scans: {} (avg {:.2} ms, max {:.2} ms, last {:.2} ms); list changes: {}; foreground events: {}",
        stats.scans,
        stats.total_micros as f64 / stats.scans.max(1) as f64 / 1000.0,
        stats.max_micros as f64 / 1000.0,
        stats.last_micros as f64 / 1000.0,
        changes.load(Ordering::Relaxed),
        foregrounds.load(Ordering::Relaxed)
    );
    let (current, count) = ot_shell::desktops::read();
    println!("virtual desktop: {current} of {count}");
}

/// Development flags print results: write them to the terminal that started us, if any. A plain start (no
/// arguments, e.g. from Explorer or the startup entry) and the watchdog never touch a console.
#[cfg(windows)]
fn attach_parent_console(args: &[String]) {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    if args.is_empty() || args.iter().any(|a| a.starts_with("--watchdog")) {
        return;
    }
    // SAFETY: plain Win32 call; fails harmlessly when the parent has no console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Saves settings the dock changed (pin order) to the app's own settings file.
#[cfg(windows)]
fn save_settings(settings: &ot_core::settings::DockSettings) {
    // The dock only runs after setup (finished or skipped), so what it saves never asks for setup again.
    let mut settings = settings.clone();
    settings.onboarding_version = settings
        .onboarding_version
        .max(ot_ui::wizard::ONBOARDING_VERSION);
    if let Err(error) = SettingsStore::for_current_user().save(&settings) {
        log::report_error(error.key(), &settings.language, None);
    }
}
