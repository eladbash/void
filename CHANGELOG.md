# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

Void cleans up after your AI agents — and your agents can call Void themselves.

### Added
- **Agent worktrees** ecosystem: finds git worktrees created by Claude Code
  (`<repo>/.claude/worktrees`), Cursor (`~/.cursor/worktrees`), Codex
  (`~/.codex/worktrees`), Conductor (`~/conductor/workspaces`) and custom roots,
  with branch, uncommitted and unpushed work, merge state and lock status.
  Actions: trim build artifacts and keep the worktree; `git worktree remove` for
  clean worktrees only (re-verified when it runs, never `--force`); orphaned
  worktrees; `git worktree prune`; merged agent branches via `git branch -d`
  (never `-D`)
- **AI agent data** ecosystem: Claude Code transcripts, file history, debug logs
  and caches; Codex sessions and archived sessions; Cursor / VS Code / Windsurf
  `state.vscdb` compaction with VACUUM (never deleted, so chat history is kept)
  and orphaned workspace storage
- **Local AI models** ecosystem: Ollama per model (shared layers counted once)
  and orphaned blobs; Hugging Face cache per repo, detached revisions and
  incomplete downloads; LM Studio, torch hub and ComfyUI
- Duplicate model detection across stores. Duplicates are replaced by APFS
  copy-on-write clones (or hardlinks), freeing space without deleting anything;
  contents are re-hashed immediately before replacement
- **Stale projects** ecosystem (project graveyard): git projects with no commit
  and no edit in 90 days, showing remote, unpushed commits and dirty state; strip
  artifacts or move the project to the Trash
- Usage by agent: an Agents screen that totals disk use per AI tool and category
- `void` command line tool (`crates/void-cli`): `scan`, `plan`, `apply`,
  `worktrees`, `models`, `agents`, `usage`, `guard check`, with JSON output and
  dry-run plans that expire after an hour
- MCP server (`void mcp`) with the tools `disk_status`, `scan`, `explain_item`,
  `plan_cleanup`, `apply_plan` and `usage_by_agent`. Applying a plan requires
  explicit confirmation, and Danger actions are never available over MCP.
  `void mcp-config` prints a client config snippet
- Claude Code hooks: `void hook install` adds a SessionStart low-disk warning and
  a WorktreeRemove artifact trim to `~/.claude/settings.json` (backed up first,
  idempotent); `void hook uninstall` removes them
- Guard mode: background free-space watch with notifications (warning at 15%
  free, critical at 5%), menu-bar status, launch at login, and opt-in auto-clean
  policies that run Safe actions only
- New caches: uv; pytest, ruff, mypy and tox caches; virtualenvs detected by
  `pyvenv.cfg`; conda (Miniconda, Anaconda, Miniforge); Bun; Next.js, Turbo,
  Parcel, SvelteKit and Nuxt build caches; Playwright and Puppeteer browsers
  (newest build kept); Maven `target/`; Docker build cache via
  `docker builder prune`; Docker Desktop, OrbStack and Colima VM disk sizes
  (informational); Linux Trash
- Move to Trash alongside permanent deletion for items worth recovering
- Item details (branch, dirty state, model tag, agent…) shown in the item drawer
- Developer harness: `void dev seed <dir>` builds a realistic fake home and
  `void --home <dir> scan` runs every scanner against it; frontend logic tests
  run with `node --test`
- CI: frontend test job and a macOS test job for `deepclean-core` and `void-cli`

### Changed
- Sizes count hardlinked files once and sparse files by allocated size
- Scanner construction moved to `scanner/registry.rs`; ecosystems are listed in
  display order in `Ecosystem::ALL`
- Safety checker now refuses the home directory, `/` and any ancestor of home,
  protects agent configuration (`~/.claude.json`, `~/.claude/settings.json`,
  memory, skills, agents, commands, plugins, `history.jsonl`; Codex `auth.json`,
  `config.toml`, memories), and treats `.claude.json`, `auth.json`, `CLAUDE.md`
  and `AGENTS.md` as sentinel files
- CI clippy now checks all targets, including tests

### Fixed
- The cargo registry was mislabelled as a `target/` directory
- Docker sizes were always reported as unknown; real sizes are now read from
  Docker, capped at the Docker Desktop / OrbStack disk image's real size
  (Docker's own per-category figures overlap and could exceed the disk)
- "Reclaimable" no longer counts informational items (container VM disk images,
  locked worktrees) or items nested inside another counted item — a Docker.raw
  was being added on top of the Docker data it contains, showing more
  reclaimable space than the disk holds
- Linux was offered a PowerShell action
- The README promised `docker builder prune`, which Void did not actually offer

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
