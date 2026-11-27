# Optimised and Simplified OpenTaskBar

A Rust rewrite of [OpenTaskbar](https://github.com/alztrk/opentaskbar), the macOS-inspired floating dock and taskbar replacement for Windows 10 and 11.

The goal is the same dock at a fraction of the cost. The dock draws through Windows' own compositor (Windows.UI.Composition with Direct2D) instead of WPF. Measured on the development machine (details in [docs/OPTIMIZATIONS.md](docs/OPTIMIZATIONS.md) and [docs/PERF_LOG.md](docs/PERF_LOG.md)):

| | OpenTaskbar (C#) | This port |
|---|---|---|
| Dock private memory | ~128 MB | ~15–22 MB |
| CPU while sweeping the magnification | 69.3 % of one core | 4.4–6.2 % |
| CPU when idle | timers and polling | 0 % (event-driven) |

## What works

- **Dock:**
  - pinned and running apps, Start, Search, Task View, Show Desktop and the Recycle Bin;
  - magnification with the C# curve and spring, drag reordering;
  - auto-hide (Smart and Always), context menus, jump lists, live window previews;
  - notification badges, tooltips, keyboard navigation and global hotkeys;
  - UI Automation, so screen readers can read the dock.
- **Tray:** the notification area is hosted by the dock: the background apps flyout and the pinned tray icons.
- **Widgets:**
  - Clock with a calendar flyout;
  - Battery;
  - System stats: CPU, RAM, GPU and GPU temperature;
  - Audio with a mixer flyout;
  - Claude usage.
- **Setup wizard:** an animated first-run wizard that installs the dock and customizes it live.
- **Settings window:** Dock, Widgets, Themes (theme packs, editor, brush picker), Shortcuts, Preferences and About; backups that work with OpenTaskbar's.
- **Safety:** Explorer's taskbar is always restored, by a watchdog (a mode of the same executable), a recovery journal and a startup heal.

Not done yet: the remaining widgets, multi-monitor, profiles, groups, stacks, the launcher, compact mode, Settings search and the updater. See [docs/PORTING_PLAN.md](docs/PORTING_PLAN.md) (in Turkish).

## Build

Requirements: Rust (stable, MSVC toolchain) and Visual Studio Build Tools with the C++ x64 tools and the Windows SDK. The Settings window and the wizard use the WebView2 Runtime.

```powershell
cargo build --release
cargo test --workspace
```

The program is `target\release\OpenTaskBar.exe`. Its settings, log and theme packs are in `%LOCALAPPDATA%\OpenTaskBar-Rust`, separate from OpenTaskbar's, so both can be installed side by side.

## Credits and thanks

This project would not exist without [OpenTaskbar](https://github.com/alztrk/opentaskbar) by **Alican Öztürk** ([alztrk](https://github.com/alztrk)). Its design, its behaviour and its settings format are the reference this port was built against, rule by rule and pixel by pixel. Several of the fixes here came from comparing with his own Rust port. Thank you, Alican, for creating OpenTaskbar, sharing it under the MIT license and welcoming contributions.

Thanks also to everyone who contributed to OpenTaskbar.

## License

MIT. Based on OpenTaskbar, © OpenTaskbar contributors.
