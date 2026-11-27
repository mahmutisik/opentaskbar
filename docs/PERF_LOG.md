# Performance log

Measurements at the end of each phase, compared with the C# baseline. Method: `tools/perf` scripts (pointer sweep over the apps, idle, rest), GPU Engine counters per process, per-thread CPU.

## C# baseline (OpenTaskbar bbd866d, 2026-11-23)

| Phase | GPU (dock) | CPU (dock) | Notes |
|---|---|---|---|
| Idle | 2.4% | 8.5% | |
| Sweep (magnification) | 60–71% | 34–53% | render thread 24–31% |
| Thumbnail previews | 13.6% | 17.9% | |

Memory: ~100 MB private. Startup to features initialized: ~2.5 s. Executable: 172 MB.

## Phase 1: core (2026-11-23)

No window yet. Release build of `OpenTaskBar.exe` loading settings and the active language: 0.8–1.7 ms. With the log and the single-instance lock: ready 2.1 ms after `main` (log: `Startup +2ms: ready`). Executable: 1.5 MB, of which ~730 KB are the 14 embedded language catalogs.

## Phase 2: taskbar replacement (2026-11-23)

`OpenTaskBar.exe --shell-test` (debug build, placeholder dock window) on 1920x1080 at 100 %, Explorer taskbar 48 DIP at the bottom, state 0 before and after every test:

| Scenario | Result | Restore time |
|---|---|---|
| Normal run and stop | taskbar hidden, work area 1032 -> 1014 (48 native + 18 dock), restored and verified, journal deleted, watchdog exited | 42 ms (stop) |
| Panic inside the message loop | panic hook restored, process ended | 11 ms |
| `taskkill /F` | watchdog detected the exit and restored | 686 ms after the kill |
| Stale journal + `--restore` | user's visible state kept (not switched to auto-hide), journal deleted | 47 ms |
| Damaged journal + normal start | startup heal restored, journal deleted | 46 ms |

Open item: the first `ABM_SETPOS` that changes the work area blocked the UI thread for 1.2 s while Explorer resized windows. To measure and address in Phase 3.

## Phase 3: first dock on screen (2026-11-23)

Release build, `OpenTaskBar.exe --dock --use-settings=<OpenTaskbar settings.json>` (same settings as the C# dock: Compact, Dark, Smart auto-hide), 11 pinned/running apps. Idle, 3–6 s samples:

| | OpenTaskbar (C#) | OpenTaskBar-Rust |
|---|---|---|
| CPU | 4.9 % | 0.0 % |
| GPU (3D engine) | 0 % | 0 % |
| Private memory | 103 MB | 62–67 MB |
| Threads | 22 | 18–29 |
| Executable | 172 MB | 2.2 MB |

The C# dock was auto-hidden during its sample (Smart auto-hide); the Rust dock has no auto-hide yet and was visible.

Startup to "dock visible": 1,672 ms, of which ~120 ms is our own work (renderer, pins) and ~1.4 s is Explorer applying the taskbar state (`ABM_SETSTATE` 1,047 ms) and registering the AppBar (`ABM_NEW` 350 ms). The C# dock waits for the same calls.

Update (same day): with Direct2D on the WARP device the dock uses **15 MB** private memory (2.1 MB before the window, +1.3 MB renderer, ~9–11 MB Shell icon extraction). UI Automation finds all 15 dock buttons in 41 ms.

Open items (superseded): memory is above the 40 MB target (Direct3D/Direct2D/DirectWrite and composition setup dominate; to profile); magnification, auto-hide and menus are not implemented yet, so no sweep comparison is possible.

## Phase 4: setup wizard (2026-11-23)

Release build, `OpenTaskBar.exe --wizard-demo` on a first run (no settings file): the wizard window is visible 461 ms after `main` (including the WebView2 environment). The install step shows the dock 1.34 s after the click (Explorer applying the taskbar state, as in Phase 3). Live changes (theme, corner radius, opacity, buttons) reach the dock on the next message. After the demo the taskbar was restored (state 0, work area unchanged) and the process exited with code 0. Executable: 2.77 MB (+0.55 MB for wry/WebView2 and the embedded page).

WebView2 while the wizard is open (welcome screen, idle): 6 processes, 237 MB private (browser 39, GPU 130, renderer 46, utilities 19, crash handler 3), rising to ~306 MB on later screens; the dock process itself 4–20 MB. All WebView2 processes exit when the wizard closes. Idle CPU of WebView2 on the welcome screen: ~44 % of one core, caused by the endless background animations (0 % without them).

Release after the wizard (`--wizard-demo --wizard-keep-dock=25`, the dock keeps running in the same process): 1 s after the wizard closed no WebView2 process was left (6 before, 308 MB) and the process used 0 % CPU; 17.6–17.7 MB private, 23 threads, against 16.0 MB and 22 threads for a dock started without the wizard. The ~1.6 MB difference is the WebView2 loader and heap left in the process. The user decided the wizard itself needs no further optimization; what matters is that nothing stays behind once it closes.

## Phase 5: magnification (2026-11-23)

Release build with the user's OpenTaskbar settings (Compact, 11 apps), pointer swept back and forth over the apps for 6 s (6 px every ~8 ms, `tools/perf/mag-sweep.ps1`), same method for both docks:

| | OpenTaskbar (C#) | OpenTaskBar-Rust |
|---|---|---|
| CPU while sweeping (one core) | 69.3 % | 6.2 % |
| CPU 1–4 s after the pointer left | 6.25 % | 0.00 % |
| Private memory | 128 MB | 15 MB |

Frame captures of a slow sweep show the bell-shaped growth, the bar widening on both sides, separators and running indicators moving with their slots, sharper large icons while magnified and an exact return to rest.

Auto-hide (Smart, the user's setting), checked on screen with a maximized test window: visible without overlap; still visible 300 ms after the window covered it, hidden at 1 s (400 ms delay + 220 ms slide); revealed within 120 ms of the pointer reaching the screen edge; hidden again after the pointer left; revealed when the window was minimized or closed, hidden when it was restored. The Rust dock used 672 ms of CPU over the whole 14 s scenario including startup.

Compatibility: OpenTaskbar bbd866d loads both a Rust-written default file and the user's real settings re-saved by Rust without errors (theme, 4 widgets, 7 pins, 29 dock order entries preserved).

## Phase 5: window previews (2026-11-24)

Release build, auto-hide off copy of the user's settings, two test forms and Notepad (`preview-perf.ps1` in the session scratchpad, 40 open/close cycles alternating between two apps, 550 ms over the item, 650 ms away):

| | OpenTaskBar-Rust |
|---|---|
| Private memory before the first preview | 16.9 MB |
| After the first preview (window, Inter font collection, brushes) | 17.9 MB |
| After 40 open/close cycles | 17.1 MB (no growth) |
| CPU with a preview open for 5 s (pointer resting on a card, live thumbnails) | 0.0 ms |
| CPU in the 5 s after it closed | 0.0 ms |

Timing (frame captures, same script for both docks): the preview is on screen 300 ms after the pointer reaches the item in Rust and ~347 ms in OpenTaskbar (its nominal 300 ms plus dispatcher and WPF window latency); the thumbnails and the card chrome then fade in over ~150 ms in both. Interaction checks (`preview-actions.ps1`): open at 300 ms; still open 300 ms after leaving, closed at 700 ms; stays open while the pointer is over it; the close button and a middle click close their windows and the cards fade out and the preview re-lays out; clicking a card brings its window to the front (foreground checked) and closes the preview.

## Phase 5: jump lists (2026-11-24)

Release build, auto-hide off copy of the user's settings, File Explorer and Chrome (`jump-invoke.ps1`, `jump-mem.ps1` in the session scratchpad). The menu opens with the loading line 26–36 ms after the right click and holds the full File Explorer list (pinned and frequent folders, 1146 px tall) at 88–93 ms. Down + Enter opened the first entry in a new Explorer window. Private memory: 14.7–15.6 MB before the first menu, 23.5–25.7 MB across 80 menus alternating between the two apps with no upward trend; the first jump list read loads the Shell's jump list components (~8 MB, kept by the process). Releasing the menu surface when it closes was tried and measured: no difference in private bytes (the WARP allocations return to its pool), so it was not kept. The executable grows by ~410 KB for Inter SemiBold.

## Phase 5: activation fix (2026-11-24)

`activation-test.ps1` (session scratchpad): two test apps, 16 clicks alternating between them, every other pair minimized first, time from the click to the app's window in front and not minimized (1 ms polling).

| | Click to foreground (median / max) | Failures |
|---|---|---|
| OpenTaskbar (C#) | 3.4 / 71.1 ms | 0 of 16 |
| Rust before the fix | 11 / 26 ms | 9 of 16 (minimized windows never came back while another app was in front; running apps had moved) |
| Rust after the fix | 3.6 / 5.9 ms | 0 of 15 (the 16th click minimized the app already in front, as intended) |

Focus check (`focus-probe.ps1`): a click on an empty spot of the dock leaves Notepad in front; a click on the item of the app in front minimizes it; a second click brings it back in front.

## Phase 6: tray UI (2026-11-24)

`tray-shot.ps1` and `tray-rects.ps1` (session scratchpad), 100 % display scale, auto-hide off copy of the user's settings. Flyout window: OpenTaskbar 344×263 px at 1382,769 for a button at 1521,1027 (45×44); Rust 344×263 px at 1074,768 for a button at 1213,1026: the same offsets from the button (−139, −258 px). Before the placement fix Rust sat 12 px higher and 10 px further left, because WPF passes the placement callback the popup's size without the root margins (8 + 4 DIP vertically, 10 + 10 horizontally). Rows, heading, icons and buttons line up pixel for pixel in the captures (row pitch 40 px, first row 56 px below the frame top). Registry list (`cargo test -p ot-dock --lib live -- --ignored --nocapture`): 7 running apps found in 36 ms in a debug build, 2 of them without a live icon (Intel DSA, NVIDIA).

## Phase 7: widget host and clock (2026-11-24)

Test dock with a copy of the user's settings plus the Clock widget (`widget-shot.ps1`, scratchpad). The card is 134×42 DIP (UI Automation), noon theme at 15:09; tooltip "24 Kasım 2026 Salı"; the context menu shows "Widget'ı Kaldır". Idle for 60 s with the clock ticking: 46.9 ms CPU (0.078 % of one core) for the whole dock process, private memory 18.7 → 18.5 MB (Inter Bold for the figures is loaded only when a widget is shown).

Phase 7, the user's widgets (System meters, Audio, Battery, Clock) on the same test dock: 46.9 ms CPU in 60 s (0.078 %), private memory 19.0 -> 18.9 MB, 18 threads (one for the GPU sensors). The System meters card redraws once a second when a value changes; GPU usage at the time read 96 % (Windows' `\GPU Engine` counter agreed: 3D engine 96.3 %).

Audio mixer flyout (`audio-flyout-perf.ps1`, scratchpad; test dock with a copy of the user's settings, 10 s samples). First version: closed 21.2 MB, open 23.6 MB, closed again 23.5 MB (the flyout's own Inter collection, 1.7 MB, and its surfaces stayed). After sharing the widget fonts and releasing the surfaces on close: closed 21.8 MB (Inter Medium now in the shared collection), open 23.4 MB, closed again 21.2 MB; idle CPU with the flyout open 0 ms in 10 s. Opening (device lists, app sessions and their icons, drawing) took about 140 ms of CPU in the first sample window.

Settings window (`settings-mem.ps1`, scratchpad; test dock on a copy of the user's settings with `--settings`): dock process 22.1 MB private with Settings open, six WebView2 processes 177.3 MB private; 5 s after closing: dock 22.7 MB, no WebView2 process.

