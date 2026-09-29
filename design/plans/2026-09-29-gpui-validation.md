# GPUI migration — validation record

Companion to `2026-09-29-gpui-migration.md`. Every row of that plan's feature
matrix, what was run, and the result. "Real app" means the built `void-app`
binary launched against a seeded sandbox (`VOID_HOME`) on macOS 26 (Apple
Silicon), driven by `scripts/drive.py` with real keystrokes and mouse clicks
and checked from window screenshots. "Headless" means `src/ui/tests.rs` on
GPUI's test platform. "Baseline" means the same step captured from the Tauri
build before the migration and compared side by side (`scripts/compare.py`).

## How to reproduce

```bash
cargo build -p deepclean-app -p void-cli
python3 scripts/drive.py target/debug/void-app /tmp/shots screens            # 4 theme × density combos
COMBOS=dark:comfortable python3 scripts/drive.py target/debug/void-app /tmp/shots matrix
COMBOS=light:compact    python3 scripts/drive.py target/debug/void-app /tmp/shots matrix
COMBOS=light:comfortable python3 scripts/drive.py target/debug/void-app /tmp/shots persistence
cargo test --workspace
docker build -f scripts/linux-smoke.Dockerfile -t void-linux . && docker run --rm -v "$PWD/target/linux-smoke:/out" void-linux
```

## Visual comparison with the Tauri build

44 baseline screenshots (11 states × light/dark × comfortable/compact) were
captured from the Tauri build and paired with the GPUI build's. Checked per
pair: region layout, token colours, type hierarchy, row height per density,
size-column alignment, path truncation, badge colours, empty states, modal
centring.

| State | Result |
|---|---|
| Results empty, scanning, results, compact | Match. Fixed during review: rows did not span the list (columns drifted); filter field and number fields rendered light on dark; logo ring lost its gap (`pathLength` unsupported by the SVG renderer — redrawn as an arc) |
| Drawer | Match. Fixed: scrim now also dims the rail; agent badge glyph order |
| Confirm (safe, with danger) | Match |
| Agents | Match |
| History empty | Match |
| Settings | Match. Fixed: "Add folder…" and "Restore defaults" stretched full width; empty label gap above the staleness field |
| Title bar | Now follows Void's theme on macOS (it followed only the OS before the fix) |

Deliberate differences: the About version is the real crate version (the old
screen had `1.0.0` hard-coded); group headers are not sticky while scrolling
(GPUI's virtual list has no sticky rows).

## Feature matrix

| # | Area | How | Result |
|---|---|---|---|
| R1 | Results empty ×T×D | real app, 4 combos | ✅ |
| R2 | Scan (button, ⌘R, tray) | real app; headless `scan_select_clean…` | ✅ |
| R3 | Scan while scanning | `start_scan` guard; headless | ✅ |
| R4 | Group by ecosystem/project/risk | real app (chip) | ✅ AI ecosystems first; risk order safe→… |
| R5 | Sort size/stale/name/risk | real app (chip) | ✅ |
| R6 | Filter text, clear, Esc | real app; headless `typing_in_the_filter…` | ✅ **Bug found and fixed**: the toolbar (and the field being typed in) disappeared when nothing matched, cutting typing off mid-word — same flaw in the Tauri build |
| R7 | Filters chip (large only) | real app | ✅ "No items match", count pill |
| R8 | Collapse / ⌘⇧E | real app; headless | ✅ |
| R9 | Select: row, group, safe, ⌘A, ⌘⇧A, ⌥⌘A | real app; headless | ✅ 21 selected, 3 nested counted once. ⌥⌘A never worked on macOS in the Tauri build (`e.key` was `å`); works now |
| R10 | Informational item not selectable | headless | ✅ |
| R11 | ↓/↑, Space, Enter, ⌥↓ | real app; headless | ✅ |
| R12 | Drawer ×T | real app, both themes | ✅ |
| R13 | Drawer Copy / Reveal / Exclude + Undo / Clean one | real app (Copy verified via `pbpaste`); headless `exclude_then_undo` | ✅ |
| R14 | Confirm, safe | real app | ✅ |
| R15 | Confirm with danger, type `delete` | real app; headless `danger_needs_typed_confirmation` | ✅ button disabled until typed |
| R16 | Prefer Trash in modal | headless (config round trip) | ✅ |
| R17 | Cancel / Esc / backdrop | real app (Esc); headless | ✅ |
| R18 | Clean progress, Stop | real app (21 items) | ✅ |
| R19 | Result card, View report, Scan again | real app | ✅ "Reclaimed 130 kB · 21 of 21 cleaned", estimated badge |
| R20 | Failure + fallback | model test `action_event_failure_offers_fallback…` | ✅ (not reproducible in the sandbox, which confines commands) |
| R21 | Issues banner and sheet | headless `renders_every_overlay…` | ✅ renders |
| R22 | Nothing to reclaim / no match | real app (no match) | ✅ |
| A1 | Agents ×T | real app | ✅ |
| A2 | Check now | real app | ✅ |
| A3 | Jump to preset | real app via tray "Trim idle worktrees"; headless | ✅ |
| A4 | Hooks install/uninstall | headless with a stand-in `void` on PATH, sandbox home | ✅; CLI-missing hint shown in the real app |
| A5 | MCP copy | headless (snippet contents) | ✅ |
| H1–H4 | History empty, runs, run detail, clear | real app; headless | ✅ |
| S1 | All 8 settings sections ×T | real app | ✅ |
| S2–S7 | Roots (picker), ecosystems, AI, Guard, Safety, Performance | real app renders; headless `settings_are_saved…` (injected folder picker, debounced save) | ✅ |
| S8 | Theme, density, menu-bar icon, launch at login | real app: theme switch live (title bar too), menu-bar icon hides/shows the tray | ✅ Launch at login is not touched in a sandbox by design |
| S9 | About | real app | ✅ real version and settings path |
| P1 | Persistence across relaunch | real app `persistence` | ✅ |
| P2 | Corrupt settings | real app; headless | ✅ toast "Settings reset to defaults — …" (reappears while the file stays broken, as before) |
| P3 | Sandbox title | real app | ✅ |
| T1 | Tray status line | real app | ✅ "115 GB free" → "115 GB free · no idle worktrees" after a scan |
| T2 | Tray menu | real app: Trim idle worktrees…, Quit | ✅ preset applied + scan started; Quit exits |
| T3 | Tray visibility | real app | ✅ icon removed and restored |
| N1 | Low-disk notification | real app with warn at 99% | ✅ Guard shows LOW, notification posted without error. **Bug found and fixed**: on macOS the notification library opened a "Choose Application" dialog because no sender was set |
| K1 | Shortcuts | real app; headless | ✅ |
| X1 | 720×560 minimum ×T | real app | ✅ disk meter drops below 860pt and Agents stacks below 760pt, as the stylesheet did |
| X2 | Linux | Docker (Debian bookworm, arm64): build, full test suite, Xvfb launch | see below |
| X3 | Windows | CI only (no Windows machine here) | pending CI |

## Platforms

- **macOS**: everything above; `cargo test --workspace` green; `cargo clippy
  -D warnings` clean; `cargo packager --release` produced `Void.app`
  (bundle id `com.void.app`, min macOS 11) and `Void_1.1.0_aarch64.dmg`.
- **Linux**: the whole workspace builds in Docker with the libraries in
  `scripts/linux-deps.sh`. The first test run caught three window tests that
  sent `cmd-…` (Super on Linux) instead of the portable `secondary-…`; fixed.
  The tray reports a D-Bus error without a session bus and the app carries on.
- **Windows**: compiled and tested by CI only.
