# Contributing to Void

Thanks for your interest in contributing! Here's how to get started.

## Reporting Bugs

Open a [bug report](https://github.com/eladbash/void/issues/new?template=bug_report.yml) with steps to reproduce, expected behavior, and your OS/version.

## Suggesting Features

Open a [feature request](https://github.com/eladbash/void/issues/new?template=feature_request.yml) describing the problem and your proposed solution.

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) 22+ (only for the frontend tests)
- Tauri CLI: `cargo install tauri-cli`
- macOS: Xcode Command Line Tools
- Linux: `sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libgtk-3-dev`

### Building

```bash
git clone https://github.com/eladbash/void.git
cd void

# Run tests
cargo test --workspace
npm test

# Build everything (core, app, CLI)
cargo build

# Run the Tauri app in dev mode
cargo tauri dev

# Run the CLI
cargo run -p void-cli -- scan
```

### Trying things safely

Never point a work-in-progress build at your real home directory in a mode that
deletes anything. Build a sandbox instead:

```bash
cargo run -p void-cli -- dev seed /tmp/void-home   # fake home: agent worktrees in every
                                                    # state, ~/.claude, Ollama / HF stores,
                                                    # stale projects
cargo run -p void-cli -- --home /tmp/void-home scan
(cd crates/deepclean-app && VOID_HOME=/tmp/void-home cargo tauri dev)   # the real app, sandboxed
```

`VOID_HOME` runs the desktop app against the fake home: scans, settings, history and the
Trash all stay inside it, the window title says SANDBOX, and any action that could reach the
real machine (Docker prunes, `ollama rm`, system caches) is dropped.

For UI work, serve the repo root and open `dev/harness.html`: the real frontend
with a stubbed Tauri bridge (see the comment at the top of the file).

### Project Structure

```
crates/
  deepclean-core/   # Scanning engine, models, safety checker, action executor
    src/
      scanner/      # Per-ecosystem scanners (rust.rs, node.rs, worktrees.rs, models.rs, ...)
        registry.rs # Which scanner handles which ecosystem (shared by app, CLI, MCP)
      action/       # Clean action execution with safety gates
      git.rs        # Worktree remove / prune / branch delete, re-checked at run time
      dedup.rs      # Duplicate model files -> clones / hardlinks
      guard.rs      # Free-space watch and auto-clean policies
      hooks.rs      # Claude Code hooks and MCP config
      trash.rs      # Move to Trash (system or a directory, for tests)
      testkit/      # FakeHome and per-feature seeds for tests and `void dev seed`
      model.rs      # Data types (CleanableItem, Ecosystem, RiskLevel, etc.)
      safety.rs     # Path safety checker
      staleness.rs  # Last-modified staleness detection
      config.rs     # App configuration
  deepclean-app/    # Tauri 2 desktop app
    src/
      commands.rs   # Tauri IPC commands
      tray.rs       # System tray setup
      state.rs      # App state management
    frontend/       # Vanilla JS UI; tests/ holds node --test suites
  void-cli/         # `void` CLI and MCP server
```

### Adding a New Scanner

1. Create `crates/deepclean-core/src/scanner/myecosystem.rs` and declare it in
   `scanner/mod.rs`.
2. Implement the `EcosystemScanner` trait. Take the home directory as a
   constructor argument (`MyScanner::new(home: PathBuf, config: &AppConfig)`)
   instead of looking it up, so tests can point it at a fake home. Use
   `analyze_many` if one directory yields several items.
3. Add the variant to `Ecosystem` in `model.rs`, plus any new `ArtifactKind`s,
   and add it to `Ecosystem::ALL`. Order matters: the walker gives a directory to
   the *first* scanner that claims it and does not descend further.
4. Construct it in `scanner/registry.rs`. The app, the CLI and the MCP server
   all build scanners from there; there is nothing to add in `commands.rs`.
5. Add display labels for the new ecosystem and kinds in the frontend
   (`frontend/js/actions.js`, colours in `frontend/js/icons.js`). The frontend
   labels test fails if one is missing.
6. Every item's actions list the lowest-risk option first, sizes come from the
   `staleness` helpers, and facts the user needs to decide go in `details`.

#### Tests are required

- Unit tests in the module for parsing and other pure logic.
- An integration test in `crates/deepclean-core/tests/<feature>.rs` that builds
  a fake home with `testkit::FakeHome`, runs the real scanner (or the real
  `ScanOrchestrator` via `registry::build_with_home`) and the real
  `ActionExecutor` with `TrashBackend::Directory`, then asserts on disk what was
  removed **and what was preserved**. Include the negative cases: the dirty
  worktree survives, credentials survive, a blocked path is refused.
- A `testkit/<feature>.rs` seed so `void dev seed` shows your ecosystem.
- **Never touch the real home directory or the real Trash in a test.** Use
  `tempfile::tempdir()`, `SafetyChecker::with_home(home, vec![])` and
  `ActionExecutor::with_trash(safety, TrashBackend::Directory(tmp))`.
- Tests must be deterministic and offline, and pass on Linux and macOS CI. Only
  `git` is available there. For tools like `docker` or `ollama`, test the shape
  of the action you emit; never run it.

### Git Hooks

Enable the repo's hooks once per clone:

```bash
git config core.hooksPath .githooks
```

The `pre-commit` hook runs `cargo fmt --check` — the same check as CI's `fmt`
job — so formatting problems surface before you push. Bypass it for a single
commit with `git commit --no-verify`.

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy --workspace --all-targets -- -D warnings` and fix all warnings
- Write tests for new scanners and safety-critical code

## Pull Request Process

1. Fork the repo and create a feature branch
2. Make your changes with tests
3. Ensure `cargo test --workspace`, `cargo clippy` and the frontend tests pass
4. Open a PR with a clear description of the changes

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be kind and constructive.
