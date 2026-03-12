# Contributing to Void

Thanks for your interest in contributing! Here's how to get started.

## Reporting Bugs

Open a [bug report](https://github.com/eladbash/void/issues/new?template=bug_report.yml) with steps to reproduce, expected behavior, and your OS/version.

## Suggesting Features

Open a [feature request](https://github.com/eladbash/void/issues/new?template=feature_request.yml) describing the problem and your proposed solution.

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) (LTS)
- Tauri CLI: `cargo install tauri-cli`
- macOS: Xcode Command Line Tools
- Linux: `sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libgtk-3-dev`

### Building

```bash
git clone https://github.com/eladbash/void.git
cd void

# Run tests
cargo test

# Build the core library
cargo build

# Run the Tauri app in dev mode
cargo tauri dev
```

### Project Structure

```
crates/
  deepclean-core/   # Scanning engine, models, safety checker, action executor
    src/
      scanner/      # Per-ecosystem scanners (rust.rs, node.rs, python.rs, ...)
      action/       # Clean action execution with safety gates
      model.rs      # Data types (CleanableItem, Ecosystem, RiskLevel, etc.)
      safety.rs     # Path safety checker
      staleness.rs  # Last-modified staleness detection
      config.rs     # App configuration
  deepclean-app/    # Tauri 2 desktop app
    src/
      commands.rs   # Tauri IPC commands
      tray.rs       # System tray setup
      state.rs      # App state management
```

### Adding a New Scanner

1. Create `crates/deepclean-core/src/scanner/myecosystem.rs`
2. Implement the `EcosystemScanner` trait
3. Add the ecosystem variant to `Ecosystem` enum in `model.rs`
4. Register it in `scanner/mod.rs` and `commands.rs`

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy -- -D warnings` and fix all warnings
- Write tests for new scanners and safety-critical code

## Pull Request Process

1. Fork the repo and create a feature branch
2. Make your changes with tests
3. Ensure `cargo test` and `cargo clippy` pass
4. Open a PR with a clear description of the changes

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be kind and constructive.
