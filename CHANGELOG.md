# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.0.0] - 2026-08-22

### Added
- Redesigned desktop UI: new design tokens, component and screen stylesheets,
  and a modular frontend split into `store`, `actions`, `api` and per-screen modules
- Clean history: every run is recorded per item with its outcome and reclaimed
  size, and is browsable from a new History screen
- Disk usage reporting, so reclaimable space is shown as a share of the volume
  the artifacts actually live on
- Template-based tray icons for correct rendering in light and dark menu bars

### Changed
- Refreshed app icons and website assets
- Reworked scanners across all ecosystems for more accurate sizing and staleness
- Expanded configuration options for scan roots, ecosystems and thresholds

## [0.1.2] - 2026-08-22

### Fixed
- GUI-launched apps no longer fail to spawn toolchains with
  "IO error: No such file or directory (os error 2)". macOS `launchd` and Linux
  desktop launchers hand the app a bare `PATH`; the real `PATH` is now restored
  at startup from the user's login shell, so `cargo`, `go`, `npm`, `brew` and
  `docker` are found

## [0.1.1] - 2026-08-22

### Fixed
- Sentinel files such as `.env` in a project root no longer block commands like
  `cargo clean` from running there. Working directories are now checked
  separately from deletion targets — the command never deletes that directory,
  while deleting it directly is still refused

## [0.1.0] - 2026-03-12

### Added
- Initial release
- Core scanning engine with parallel filesystem walker
- 11 ecosystem scanners: Rust, Node.js, Python, Go, Java, Docker, Apple/Xcode, Homebrew, JetBrains, .NET, System
- Safety checker with blocked paths, sentinel file detection, and risk levels
- Staleness detection for build artifacts
- Action executor with safety gates for cleaning artifacts
- Tauri 2 desktop app with system tray integration
- Configurable scan roots, ecosystems, and staleness thresholds
