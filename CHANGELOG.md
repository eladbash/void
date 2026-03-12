# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
