# Changelog

All notable changes to PulseForge are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the version numbers follow
[Semantic Versioning](https://semver.org/).

## [1.1.0] - 2026-11-24

### Added

- **Group processes by name.** Tick "Group by name" on the Processes page to get one row per program, with the processor, memory, disk and graphics use of all its processes added up and a count such as ×12. A browser with dozens of processes becomes a single line you can sort and compare.
- **Details of the selected process** under the table: which program started it, the user it runs as, how long it has been running, its handles and its graphics memory.
- **Drive encryption state.** The Storage page and the exported report now say for every volume whether it is encrypted with BitLocker. No administrator rights are needed.
- **Alert history that survives a restart.** The Alerts page keeps the last 200 alerts between runs, and a new Clear button empties the list.
- **Copy summary.** A button on the System page (or Ctrl+Shift+C) puts a short description of the computer on the clipboard: model, Windows version, processor, memory, graphics, drives and displays. It contains no serial numbers, names or addresses, so it is safe to paste into a message or a forum post.
- **Keyboard shortcuts.** Ctrl+1 to Ctrl+0 open the pages, Ctrl+Tab and Ctrl+Shift+Tab step through them, Ctrl+F searches the processes, Ctrl+E exports a report, Ctrl+M switches the mini widget on or off and F5 reads the hardware and the drives again. They are listed in Settings and in the tooltips of the navigation.
- **Automatic clean-up of CSV logs.** A new "Keep the files" setting deletes log files older than 7, 30 or 90 days. The default is still to keep them for ever, and only PulseForge's own log files are ever removed.
- **Mini widget: back to the main window.** Double-click the widget, or right-click it and choose "Open PulseForge".
- **The window opens the way you left it**: at the same size and on the same page. The page part can be switched off in Settings ("Open where I left off").

### Changed

- Starting PulseForge while it is already running now shows the window of the running copy (also when it is hidden in the notification area) instead of starting a second copy with a second icon.
- The volume tables in the exported report have a new "Encryption" column.
- The empty alert history no longer says "since PulseForge started", because the history is now kept.
- For people who build from source: `scripts/package.ps1` now respects `CARGO_TARGET_DIR`.

### Fixed

- The notification-area icon could disappear while the window was hidden, leaving no way to bring PulseForge back. This happened when the icon was switched off in Settings, PulseForge was started in the notification area, and the mini widget was then switched from the icon's menu. The icon now always stays while the window is hidden.
- The mini widget could open outside the screen after the display it was left on had been unplugged. It now opens in a visible place.
- The "Encryption" column on the Storage page always said "Unknown".
- The Memory screenshot in the README was a broken image.

## [1.0.0] - 2026-11-17

First release.

### Added

- Live dashboard with history graphs from 1 minute to 24 hours for processor, memory, graphics, drives, network and battery.
- Pages for the processor, memory, graphics, storage with SMART and NVMe health, network, battery, system, displays, sensors and processes.
- Alerts with limits, CSV logging, a tray icon with a live number and an always-on-top mini widget.
- A full system report as HTML, text or JSON, with serial numbers and names masked on request.
- Dark and light themes, Celsius or Fahrenheit. No kernel driver, no telemetry.

[1.1.0]: https://github.com/zheerkhalid/pulseforge/releases/tag/v1.1.0
[1.0.0]: https://github.com/zheerkhalid/pulseforge/releases/tag/v1.0.0
