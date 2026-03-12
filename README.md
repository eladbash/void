# Void

A desktop app that finds and reclaims disk space from developer build artifacts across 11 ecosystems.

[![CI](https://github.com/eladbash/void/actions/workflows/ci.yml/badge.svg)](https://github.com/eladbash/void/actions/workflows/ci.yml)
[![Release](https://github.com/eladbash/void/actions/workflows/release.yml/badge.svg)](https://github.com/eladbash/void/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **11 ecosystem scanners** — Rust (`target/`), Node.js (`node_modules/`), Python (venvs, `__pycache__`), Go (build/mod cache), Java (Gradle/Maven), Docker (dangling images, build cache), Apple/Xcode (DerivedData, simulators), Homebrew, JetBrains, .NET, and system caches
- **Safety-first** — blocked-path list, sentinel file detection (`.env`, credentials), and risk-level classification (Safe / Caution / Danger) prevent accidental deletion of important files
- **Parallel scanning** — multi-threaded filesystem walker with concurrent analysis powered by Tokio and the `ignore` crate
- **Staleness detection** — identifies artifacts that haven't been touched in configurable number of days
- **Menu bar tray** — lives in your system tray with quick-scan and dashboard access
- **Configurable** — choose scan roots, enable/disable ecosystems, set staleness thresholds, and block specific paths

## Installation

### Quick Install (macOS/Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/eladbash/void/main/install.sh | bash
```

### From Releases

Download the latest release from the [Releases page](https://github.com/eladbash/void/releases).

### From Source

Requires [Rust](https://rustup.rs/) and [Node.js](https://nodejs.org/).

```bash
git clone https://github.com/eladbash/void.git
cd void
cargo build --release
```

To run the Tauri desktop app in development:

```bash
cargo tauri dev
```

## Usage

Launch Void from your Applications folder or system tray. The app will:

1. **Scan** your home directory for build artifacts across all enabled ecosystems
2. **Display** found items grouped by ecosystem, with size and staleness info
3. **Clean** selected items with a single click — each action shows its risk level before execution

### Supported Ecosystems

| Ecosystem | What it finds |
|-----------|--------------|
| Rust | `target/` dirs, cargo registry, git checkouts |
| Node.js | `node_modules/`, npm/yarn/pnpm caches |
| Python | `__pycache__/`, virtualenvs, pip/conda caches |
| Go | Build cache, module cache, test cache |
| Java | Gradle cache/build dirs, Maven `.m2/repository` |
| Docker | Dangling/unused images, build cache, stopped containers, unused volumes |
| Apple/Xcode | DerivedData, archives, device support, simulator caches |
| Homebrew | Package cache |
| JetBrains | IDE caches |
| .NET | NuGet cache, `bin/`/`obj/` dirs |
| System | Downloads, trash, system logs |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT — see [LICENSE](LICENSE) for details.

## Credits

Built by [@eladbash](https://github.com/eladbash)

[Sponsor this project](https://github.com/sponsors/eladbash)
