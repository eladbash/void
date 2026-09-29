# Void desktop app: Tauri → GPUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> (chosen: native — the tasks share one view tree and one state model, so
> they are implemented in order in one session, with one whole-branch review
> at the end). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Tauri 2 + vanilla-JS desktop app (`crates/deepclean-app`)
with a native GPUI app that has the same screens, behaviour, settings files and
look, and ships for macOS, Linux and Windows.

**Architecture:** Three layers inside `crates/deepclean-app`:
1. `model/` — pure Rust UI logic ported 1:1 from `frontend/js/*` (formatting,
   labels, action choice, selection math, filtering, grouping, clean plan,
   scan/action event reducers). No GPUI types, so it is unit-tested on every
   CI runner without a GPU.
2. `backend/` — the old `#[tauri::command]` bodies as plain functions over an
   `Arc<AppState>`, running on a tokio runtime owned by the app and reporting
   progress as `UiEvent`s on a channel.
3. `ui/` + `platform/` — GPUI views that render `model` state and dispatch
   intents, plus OS integrations (tray, notifications, login item, reveal,
   folder picker, clipboard).

`deepclean-core` and `void-cli` do not change.

**Tech Stack:** Rust 1.95+, `gpui-kit` 0.7 (GPUI snapshot `gpui-pre` 0.3.7 +
`gpui-component` 0.7), tokio, `tray-icon` 0.25 (+ `muda` menus, GTK on
Linux), `notify-rust` 4, `auto-launch` 0.6, `png` 0.17, `cargo-packager` for
bundles.

**Spec:** the user's request in this session: replace Tauri with GPUI, keep
every feature working, support macOS/Linux/Windows, rewrite tests and CI,
update docs, website and changelog, validate as a user would across a feature
matrix. The existing app (`frontend/`, `src/*.rs`) is the behavioural spec:
where this plan and the old code disagree, the old code wins unless the plan
says the change is deliberate.

## Global Constraints

- MSRV stays `rust-version = "1.95"` (workspace). gpui-kit must build on it.
- Settings and history keep their exact locations so upgrades keep data:
  macOS `~/Library/Application Support/com.void.app/`, Linux
  `~/.config/com.void.app/`, Windows `%APPDATA%\com.void.app\`
  (`dirs::config_dir().join("com.void.app")` — same as Tauri's
  `app_config_dir()` for identifier `com.void.app`).
- `VOID_HOME=<dir>` sandbox behaviour is unchanged (title shows
  `Void — SANDBOX <dir>`, no real login item, confined scans/actions).
- Binary name `void-app`, product name `Void`, bundle id `com.void.app`.
- All copy (labels, help text, toasts, button text) is carried over verbatim
  from the JS screens.
- Sizes: base-1000, one formatter (`model::format`), `size_display` ignored.
- The risk shown is always the selected action's risk, never the item's.
- Danger actions require typing `delete`; Guard auto-clean runs Safe only.
- No new network access; no telemetry.
- Commits authored as the user with no Claude attribution trailers.

## Review Focus

1. **Scan streaming thousands of items** — the list must stay responsive:
   render only visible rows (`uniform_list`/virtual list), coalesce UI
   refreshes while scanning (≤ 4/s like the JS `scheduleRender`).
   Test: `ui_state::tests::many_items_group_and_filter_under_budget` (10k items
   grouped+filtered < 50 ms in release, < 500 ms debug).
2. **Nested selections** — a project and its `node_modules` selected together
   count bytes once and clean innermost first. Tests ported from
   `selection.test.mjs` + `build_plan_runs_nested_items_innermost_first`.
3. **A clean that fails because the tool is not on PATH** offers “Use Remove
   directory instead”. Test: `action_event_failure_offers_fallback`.
4. **Window closed while a scan/clean runs** — background tasks must not
   panic when the UI entity is gone (weak handles; sends to a closed channel
   are ignored). Test: `backend::tests::scan_survives_dropped_receiver`.
5. **Corrupt settings file** — app starts with defaults and shows the
   “Settings reset to defaults” toast once. Test: existing
   `state::tests::corrupt_config_warns…` + view test
   `startup_warning_is_toasted_once`.

---

## Libraries (decision record)

| Need | Library | Why |
|---|---|---|
| UI framework | `gpui-kit = "0.7"` | Pins a matching GPUI snapshot + `gpui-component`: text input (filter, `delete` confirm, number fields), slider, scroll areas, virtual list, focus/keybinding infra, headless test support. Hand-rolling a text input on bare `gpui 0.2` is ~800 lines. |
| Icons | our own SVGs (from `frontend/js/icons.js` sprite) via a `gpui::AssetSource` | Keeps the product's custom icon set; gpui-kit's Lucide pack disabled. |
| Async runtime | `tokio` (existing) | `deepclean-core` scanners/executor are tokio-based. One multi-thread runtime owned by the app; results cross into GPUI through `futures::channel::mpsc`. |
| Tray / menu bar | `tray-icon = "0.25"` (re-exports `muda`) | Same crate family Tauri used internally → same native behaviour. macOS/Windows on the GPUI main thread; Linux on a dedicated GTK thread (`gtk = "0.18"`). |
| Notifications | `notify-rust = "4"` | macOS (NSUserNotification via mac-notification-sys), Linux (D-Bus), Windows (WinRT toast). |
| Launch at login | `auto-launch = "0.6"` | LaunchAgent on macOS (as Tauri's `MacosLauncher::LaunchAgent`), registry on Windows, XDG autostart on Linux. |
| Folder picker | GPUI `cx.prompt_for_paths` | Native dialogs on all three OSes, no extra crate. |
| Clipboard | GPUI `cx.write_to_clipboard` | Built in. |
| Tray icon decode | `png = "0.17"` | Decode the template PNG to RGBA for `tray_icon::Icon`. |
| Packaging | `cargo-packager` (CLI, CI only) | `.app`/`.dmg`, `.deb`/`.AppImage`, `.msi`/NSIS from `[package.metadata.packager]`; replaces `tauri-action`. |

Removed: `tauri`, `tauri-build`, `tauri-plugin-{dialog,notification,autostart}`,
`tauri.conf.json`, `capabilities/`, `gen/`, `build.rs`, `frontend/`,
`dev/harness.html`, `package.json`, the Node CI job.

## File structure (after)

```
crates/deepclean-app/
  Cargo.toml                 gpui-kit, tray-icon, notify-rust, auto-launch, png; packager metadata
  assets/icons/*.svg         one file per sprite symbol + mark.svg
  icons/                     (unchanged) app + tray icons
  src/
    main.rs                  fn main() { void_app_lib::run() }
    lib.rs                   startup: PATH, sandbox, AppState, runtime, window, tray, guard
    state.rs                 AppState (unchanged API; doc tweaks)
    backend/
      mod.rs                 UiEvent enum, Backend handle (runtime + event sender)
      scan.rs                start_scan (from commands::start_scan)
      clean.rs               execute_clean, run_batch, describe_method, cancel
      guard.rs               guard loop/check (from guard_mode.rs, Tauri removed)
      integrations.rs        hooks, MCP snippet, CLI lookup, agent usage
      history.rs             HistoryView, clear
      disk.rs                disk usage, reveal_path
    model/
      mod.rs
      format.rs              split_bytes, format_bytes, magnitude, stale_short/long, count, percent, duration, relative_time, tildify, path_segments
      labels.rs              ECO names/order, AI_ECOSYSTEMS, KIND_LABELS, AGENT_NAMES, kind_label, project_label
      actions.rs             risk_rank, is_recoverable, worth_recovering, is_actionable, default_action, selected_action, effective_risk, describe_method, method_class, outcome_text, safety_lines, worktree_info
      selection.rs           contains_path, outermost, total_bytes_of, depth, innermost_first
      ui_state.rs            UiState (the JS `store`) + visible_items, groups, selection_summary, build_plan, presets, reducers for ScanEvent/ActionEvent
      agents.rs              category_slot, quick filter hints
      fixtures.rs            (cfg(test)) realistic items, ported from tests/fixtures.mjs
    platform/
      mod.rs
      tray.rs                Tray trait-free handle: set_status, set_tooltip, set_visible; menu → TrayCommand channel
      notify.rs              notify(title, body)
      autostart.rs           apply_launch_at_login(enabled)
      reveal.rs              reveal_path (moved from commands.rs)
    ui/
      mod.rs
      theme.rs               Tokens (light/dark), Density, typography helpers, eco colours
      icons.rs               Assets (AssetSource), icon(name, size, color)
      components.rs          btn, chip, badge, checkbox, toggle, seg, card, spinner, kbd, countpill, bars
      app_view.rs            AppView: rail, screen switch, drawer, overlays, toast, keybindings, actions, event pump
      results.rs             cmdbar, summary/progress/result card, banner, ticker, toolbar, table head, groups+rows (virtual), empty states, statusbar
      drawer.rs              item detail drawer
      confirm.rs             pre-clean confirmation modal
      issues.rs              scan issues sheet
      history.rs             history screen + run detail modal
      agents.rs              agents screen
      settings.rs            settings screen (8 sections)
  tests/
    e2e_backend.rs           seeded FakeHome: scan → plan → clean → history, persistence, sandbox confinement
    ui_views.rs              GPUI TestAppContext: navigation, selection, confirm flow, settings persistence, keyboard
scripts/
  e2e-macos.sh               launches the real app in a sandbox, drives it (cliclick/keys), captures window screenshots per matrix row
  linux-smoke.Dockerfile     Xvfb + Mesa lavapipe: build, run, screenshot (best effort)
```

## Verification strategy

**Unit (every OS in CI):** all seven `node --test` suites are ported to Rust
tests in `model/` with the same cases and expectations (format, labels,
actions, selection, store, screens-derived logic). Existing Rust tests in
`state.rs`, `guard_mode.rs`, `integrations.rs`, `tray.rs` move with their code.

**Backend e2e (every OS in CI):** `tests/e2e_backend.rs` builds a
`deepclean_core::testkit` FakeHome, runs the real scan pipeline through
`backend::scan`, feeds events through the `UiState` reducer, builds a plan for
Safe items, runs `backend::clean::run_batch`, and asserts: paths gone from
disk, history written with one run, items dropped from results, config round
trip, sandbox confinement.

**UI headless (every OS in CI):** `tests/ui_views.rs` uses GPUI's
`TestAppContext`/`VisualTestContext` (`gpui-kit` `test-support`) to open the
real `AppView` over a fixture state, dispatch the same actions and keystrokes
a user would (⌘1–4, ⌘R, ⌘A, ⌥⌘A, Space, Enter, Esc, ⌘↵, clicks by element
id), and assert on `UiState` and rendered text.

**Real-app functional validation (macOS locally):** the feature matrix below,
run against `VOID_HOME=<seeded sandbox>` with the real binary. Driven by
keyboard shortcuts and `cliclick` on element positions, every row gets a
before/after window screenshot (`screencapture -l <window id>` — window only,
never the desktop). Linux: the Docker smoke (Xvfb + lavapipe) for launch +
screenshot, if the renderer starts under software Vulkan. Windows: CI build,
tests, and a packaged `.msi` artifact.

**Visual validation:** baseline screenshots of the *Tauri* app are captured
first (same sandbox, same window size 900×700 and min size 720×560, light and
dark, comfortable and compact, each screen). The GPUI build is captured with
the same script, and each pair is reviewed side by side against a checklist:
region layout (rail/cmdbar/summary/toolbar/list/statusbar), token colours,
type hierarchy, row height per density, alignment of size column, truncation
of long paths, badge colours per risk, empty states, modal centering, no
clipped text at 720×560.

## Feature validation matrix

Each row: preconditions → user steps → expected result. "T" = light/dark
theme, "D" = comfortable/compact density; rows marked ×T×D are captured in
all four combinations.

| # | Area | Steps | Expected |
|---|---|---|---|
| R1 | Results empty (×T×D) | fresh sandbox, launch | “Find what your builds left behind”, scan roots card shows sandbox path, “Start first scan” |
| R2 | Scan | click Start / ⌘R / tray Scan Now | spinner in button + indeterminate bar, ticker per ecosystem, count rises, then summary strip |
| R3 | Scan while scanning | ⌘R twice | second ignored, no error |
| R4 | Group by | cycle Group chip ecosystem→project→risk | headers change; AI ecosystems first in ecosystem mode; risk order safe→caution→danger |
| R5 | Sort | cycle Size→Last modified→Name→Risk | rows reorder inside groups |
| R6 | Filter text | type “node” | only matching rows; clear (x) restores; Esc in field clears |
| R7 | Filters chip | click Filters | large-only (≥1 GB) toggles, count pill shows 1 |
| R8 | Collapse | click group header; ⌘⇧E | group hides/shows all |
| R9 | Select | click checkbox, group checkbox (mixed state), “Select all safe (n)”, ⌘A, ⌘⇧A, ⌥⌘A | selection count + bytes (nested counted once, “(n nested)”) |
| R10 | Informational item | click its checkbox | cannot be selected, shows “informational” |
| R11 | Keyboard nav | ↓/↑ focus, Space toggles, Enter opens drawer, ⌥↓/⌥↑ in drawer | focused row highlighted and scrolled into view |
| R12 | Drawer (×T) | open item | eco + agent badge, title, path plate, facts, details/worktree chips, actions list with default badge, safety lines |
| R13 | Drawer actions | choose other action, Copy, Reveal, Exclude (+Undo toast), Clean this item | override changes row risk; clipboard; Finder opens; item removed/restored; single clean runs |
| R14 | Confirm safe | select safe items, Review & Clean | effects rows with sizes, no attention list, Clean enabled |
| R15 | Confirm caution/danger | include caution+danger | attention list sorted danger first, commands shown; Clean disabled until `delete` typed |
| R16 | Confirm prefer-trash | toggle checkbox in modal | recoverable counts move to “moved to the Trash”, setting persisted |
| R17 | Confirm close | Cancel / Esc / click backdrop | modal closes, selection kept |
| R18 | Clean progress | run batch | progress strip “Cleaning x of n”, per-row spinner/check, bytes freed so far; Stop halts after current item |
| R19 | Result card | after clean | “Reclaimed …”, counts, duration, “estimated” badge when relevant; View report → History; Scan again; dismiss |
| R20 | Clean failure + fallback | item whose command tool is missing | row shows error + “Use Remove directory instead”, which cleans |
| R21 | Issues | scan with unreadable folder | banner “Some folders couldn't be read”, See which → sheet, Copy all, dismiss |
| R22 | Nothing to reclaim / no match | empty sandbox; impossible filter | respective empty states with buttons working |
| A1 | Agents (×T) | ⌘2 | Guard card (free, meter, threshold tick, state badge), Jump to list with counts, usage bars per agent with legend, hooks + MCP cards |
| A2 | Guard check now | click | “Checking…” then updated time |
| A3 | Jump to | click Idle worktrees | Results with preset chip; clear chip |
| A4 | Hooks | Install then Uninstall (sandbox home) | badge installed/not installed; toasts; CLI-missing hint when `void` not on PATH |
| A5 | MCP | copy command / JSON | clipboard content matches |
| H1 | History empty (×T) | fresh sandbox ⌘3 | “No cleans yet”, Go to results |
| H2 | History runs | after cleans | tiles, sparkline, rows with auto badge for guard runs, `*` estimates |
| H3 | Run detail | click row / Esc | modal with per-item outcome, command, errors |
| H4 | Clear history | click | empty state, toast |
| S1 | Settings nav (×T) | ⌘4 / ⌘, each of 8 sections | each renders |
| S2 | Scanning | Add folder… (picker), remove; staleness days | list updates; “Changes apply to your next scan” + Rescan now |
| S3 | Ecosystems | toggle one, Enable all, Disable all | persisted; stats per ecosystem after scan |
| S4 | AI | edit numbers, toggle duplicate detection (reveals min MB), add/remove worktree folder | persisted |
| S5 | Guard | toggle watch, interval, warn/critical sliders (critical ≤ warn), auto-clean (warning line), policies | persisted |
| S6 | Safety | protected list shown, add/remove blocked folder | persisted |
| S7 | Performance | sliders, Restore defaults | persisted |
| S8 | General | theme System/Light/Dark, density, group by, confirm, prefer trash, menu bar icon (tray hides/shows), launch at login (sandbox: stored only) | UI updates live; persisted |
| S9 | About | shows version and settings file path | path correct |
| S10 | Saved flash | any change | “Saved” appears then fades |
| P1 | Persistence | change settings, quit, relaunch | same settings; history kept |
| P2 | Corrupt config | write `{ broken` to settings.json, launch | toast “Settings reset to defaults — …” once |
| P3 | Sandbox title | `VOID_HOME` launch | window title `Void — SANDBOX <dir>` |
| T1 | Tray | status line after guard check + scan | “12.4 GB free · n agent worktrees idle” |
| T2 | Tray menu | Scan Now / Open Dashboard / Trim idle worktrees… / Quit | scan starts / window focused / preset applied (+scan if none yet) / app exits |
| T3 | Tray visibility | toggle “Show menu bar icon” | icon hides/shows immediately |
| N1 | Notification | set warn to 99% free, Check now | “Disk space low/critical” notification once (not repeated) |
| K1 | Shortcuts | ⌘1–4, ⌘, ⌘R, ⌘F, /, ⌘D, ⌘↵, Esc chain | as in JS keydown handler |
| X1 | Min size | resize to 720×560 (×T) | nothing clipped/overlapping |
| X2 | Linux smoke | Docker Xvfb | window renders results screen |
| X3 | Windows | CI | build + tests green, `.msi` artifact produced |

---

## Tasks

### Task 1: Baseline capture of the Tauri app

**Files:** Create `scripts/capture.py` (window-only screenshot helper),
`scripts/e2e-macos.sh` (first version: launch, wait for window, keystrokes,
capture). Output to the session scratchpad (not committed).

- [ ] Seed sandbox: `target/debug/void dev seed $SP/home`.
- [ ] Launch `VOID_HOME=$SP/home target/debug/void-app`, resize window to 900×700.
- [ ] Capture matrix rows marked ×T×D plus R12, R14, R15, R19, A1, H2, H3, S1 (each section) in light and dark → `$SP/baseline/<row>-<theme>-<density>.png`.
- [ ] Commit the scripts: `git commit -m "Add window capture and macOS e2e scripts"`.

### Task 2: Pure model layer (port of frontend logic) — TDD

**Files:** Create `src/model/{mod,format,labels,actions,selection,ui_state,agents,fixtures}.rs`.

**Produces:**
```rust
// format
pub fn split_bytes(bytes: Option<u64>) -> (String, &'static str);
pub fn format_bytes(bytes: u64) -> String;
pub enum Magnitude { Unknown, Sm, Md, Lg, Xl } pub fn magnitude(bytes: u64) -> Magnitude;
pub fn stale_short(days: Option<u64>) -> String;
pub fn stale_long(days: Option<u64>, last_modified: Option<DateTime<Utc>>) -> String;
pub fn count(n: usize) -> String;           // thousands separators
pub fn percent(part: u64, total: u64) -> String;
pub fn duration(ms: u64) -> String;
pub fn relative_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String;
pub fn tildify(path: &str, home: &str) -> String;
pub enum SegRole { Root, Dim, Project, Leaf, Sep }
pub fn path_segments(path: &str, project: Option<&str>, home: &str) -> Vec<(SegRole, String)>;
// labels
pub fn eco_name(e: Ecosystem) -> &'static str; pub const AI_ECOSYSTEMS: [Ecosystem; 3];
pub fn kind_label(item: &CleanableItem) -> String; pub fn project_label(item: &CleanableItem) -> Option<String>;
pub fn agent_name(id: &str) -> String;
// actions
pub fn risk_rank(r: RiskLevel) -> u8; pub fn is_recoverable(a: &CleanAction) -> bool;
pub fn is_actionable(i: &CleanableItem) -> bool;
pub fn default_action(i: &CleanableItem, prefer_trash: bool) -> Option<&CleanAction>;
pub fn selected_action<'a>(i: &'a CleanableItem, overrides: &HashMap<Uuid, Uuid>, prefer_trash: bool) -> Option<&'a CleanAction>;
pub fn effective_risk(...) -> RiskLevel;
pub struct MethodDesc { pub glyph: &'static str, pub text: String, pub working_dir: Option<String>, pub paths: Vec<String> }
pub fn describe_method(m: &ActionMethod) -> MethodDesc;
pub enum MethodClass { Directories, Files, Commands, Dedup, Trash } pub fn method_class(m: &ActionMethod) -> MethodClass;
pub struct SafetyLine { pub ok: bool, pub text: &'static str } pub fn safety_lines(m: &ActionMethod) -> Vec<SafetyLine>;
pub struct WorktreeInfo { pub branch: Option<String>, pub chips: Vec<Chip> } pub fn worktree_info(i: &CleanableItem) -> WorktreeInfo;
// selection
pub fn contains_path(outer: &Path, inner: &Path) -> bool;
pub fn outermost<'a>(items: &[&'a CleanableItem]) -> Vec<&'a CleanableItem>;
pub fn total_bytes_of(items: &[&CleanableItem]) -> u64;
pub fn innermost_first<T>(entries: Vec<T>, path_of: impl Fn(&T) -> &Path) -> Vec<T>;
// ui_state
pub struct UiState { pub route: Route, pub items: Vec<CleanableItem>, pub selected: HashSet<Uuid>, pub overrides: HashMap<Uuid, Uuid>, pub scanning: bool, pub has_scanned: bool, pub paths_scanned: u64, pub scan_duration_ms: u64, pub last_scan_at: Option<DateTime<Utc>>, pub scanner_status: Vec<(Ecosystem, ScannerStatus)>, pub issues: Vec<(String, usize)>, pub denied_paths: BTreeSet<String>, pub filter: String, pub large_only: bool, pub preset: Option<Preset>, pub sort: SortKey, pub collapsed: HashSet<String>, pub focused: Option<Uuid>, pub drawer: Option<Uuid>, pub config: AppConfig, pub disk: Option<DiskUsage>, pub history: Option<HistoryView>, pub clean: Option<CleanProgress>, pub last_result: Option<CleanResult>, /* agents, settings, toast … */ }
impl UiState {
  pub fn visible_items(&self) -> Vec<&CleanableItem>;
  pub fn groups(&self) -> Vec<Group>;               // groupsFor
  pub fn selection_summary(&self) -> SelectionSummary;
  pub fn build_plan(&self) -> CleanPlan;            // buildPlan
  pub fn apply_scan_event(&mut self, ev: ScanEvent);
  pub fn apply_action_event(&mut self, ev: ActionEvent);
  pub fn begin_scan(&mut self); pub fn begin_clean(&mut self, entries: &[(Uuid, Uuid)]);
  pub fn toggle_select(&mut self, id: Uuid); pub fn toggle_group(&mut self, key: &str); pub fn select_safe(&mut self);
  pub fn apply_preset(&mut self, id: PresetId); pub fn reset_filters(&mut self);
  pub fn move_focus(&mut self, delta: i32); pub fn move_drawer(&mut self, delta: i32);
}
```

- [ ] Port `tests/fixtures.mjs` to `model/fixtures.rs`.
- [ ] For each of format/labels/actions/selection/store/screens `.test.mjs`: write the Rust test module first with the same cases, run `cargo test -p deepclean-app model::` (fails), implement, run (passes).
- [ ] Add Review Focus tests 1–3.
- [ ] Commit: `Port frontend logic to a tested Rust model`.

### Task 3: Backend without Tauri

**Files:** Create `src/backend/*`, `src/platform/reveal.rs`; delete `src/commands.rs`, `src/guard_mode.rs`, `src/integrations.rs` after moving their code and tests.

**Produces:**
```rust
pub enum UiEvent { Scan(ScanEvent), ScanFinished, Action(ActionEvent), GuardStatus(GuardView), GuardCleaned(PathBuf), HistoryChanged, Warning(String), TrayRefresh }
pub struct Backend { rt: tokio::runtime::Runtime, state: Arc<AppState>, tx: UnboundedSender<UiEvent> }
impl Backend {
  pub fn new(state: Arc<AppState>) -> (Self, UnboundedReceiver<UiEvent>);
  pub fn start_scan(&self) -> Result<(), String>;
  pub fn execute_clean(&self, selections: Vec<(Uuid, Uuid)>) -> Result<(), String>;
  pub fn cancel_clean(&self);
  pub fn spawn_guard_loop(&self);
  pub fn run_guard_now(&self) -> impl Future<Output = Result<GuardView, String>>;
  pub fn state(&self) -> &Arc<AppState>;
}
```
Guard notifications and tray refresh become `UiEvent`s / calls into `platform`.

- [ ] Move tests; add `scan_survives_dropped_receiver`.
- [ ] `tests/e2e_backend.rs` (see Verification) — write failing, then wire.
- [ ] Commit: `Run scans, cleans and Guard without Tauri`.

### Task 4: Platform integrations

**Files:** `src/platform/{tray,notify,autostart}.rs`.

- Tray: `Tray::new(visible) -> (Tray, Receiver<TrayCommand>)`; `set_status`, `set_tooltip`, `set_visible`. macOS/Windows: built on the main thread inside `Application::run`; menu events forwarded from `MenuEvent::set_event_handler` into the channel; icon from `icons/trayTemplate@2x.png` with `with_icon_as_template(true)`. Linux: a `std::thread` runs `gtk::init()`, builds the tray, and `gtk::main()`; updates go through a `glib` channel.
- `status_line`/`format_bytes` tests move here unchanged.
- [ ] Commit: `Tray, notifications and login item without Tauri`.

### Task 5: Theme, icons, components

**Files:** `src/ui/{theme,icons,components}.rs`, `assets/icons/*.svg`.

- Theme tokens copied from `tokens.css` (light + dark blocks, typography sizes, spacing, radii, density row heights). `Theme::resolve(ui.theme, window.appearance())`.
- Icons: one SVG per sprite `<symbol>`, rewritten as `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">` (with `data-solid` circles filled).
- [ ] Commit: `GPUI theme, icons and components`.

### Task 6: Results screen, drawer, confirm, issues

**Files:** `src/ui/{app_view,results,drawer,confirm,issues}.rs`, `src/lib.rs`.

- AppView owns `UiState`, the `Backend`, filter/confirm `InputState`s; pumps `UiEvent`s with `cx.spawn` and coalesces notify while scanning (250 ms).
- Keybindings mirror the JS `keydown` handler (`secondary-` = ⌘ on macOS, Ctrl elsewhere).
- [ ] View tests in `tests/ui_views.rs` for R2, R9, R11, R14, R15, R17.
- [ ] Commit: `Results, drawer and clean confirmation in GPUI`.

### Task 7: Agents, History, Settings screens

- [ ] Port screens; settings writes go through `AppState` + debounced `persist_config` (400 ms) like `saveConfig`, then apply tray visibility / login item.
- [ ] View tests: S2 (folder add via injected picker), S8 theme switch, H3, A3.
- [ ] Commit: `Agents, History and Settings screens in GPUI`.

### Task 8: Remove Tauri, packaging, CI

- [ ] Delete Tauri files, frontend, harness, package.json.
- [ ] `[package.metadata.packager]` in `crates/deepclean-app/Cargo.toml`.
- [ ] `ci.yml`: fmt; clippy + test on ubuntu (GPUI deps: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libgtk-3-dev libayatana-appindicator3-dev libdbus-1-dev`), macOS and Windows test jobs for the whole workspace; drop the Node job.
- [ ] `release.yml`: version sync from tag into Cargo.toml files; `cargo packager --release -p deepclean-app`; rename assets to keep `install.sh` patterns (`Void_<v>_aarch64.dmg`, `Void_<v>_x64.dmg`); upload with `softprops/action-gh-release`.
- [ ] Commit: `Drop Tauri; package with cargo-packager; CI on all three OSes`.

### Task 9: Docs, website, changelog

- [ ] README (build/run: `cargo run -p deepclean-app`, `VOID_HOME=… cargo run -p deepclean-app`, Linux packages list, no Tauri CLI, no npm), CONTRIBUTING (layout, tests, adding an ecosystem now edits `model/labels.rs` + `ui/theme.rs` colours), SECURITY if it mentions the webview/CSP, website `docs/index.html` (download button per platform; mention native GPU-rendered app), CHANGELOG `[Unreleased]` → Changed/Removed entries.
- [ ] Commit: `Docs, website and changelog for the GPUI app`.

### Task 10: Validate the matrix and review

- [ ] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- [ ] Run `scripts/e2e-macos.sh` for every matrix row; review screenshots side by side with the baseline; fix and re-run until every row passes. Record results in `design/plans/2026-09-29-gpui-validation.md`.
- [ ] Linux Docker smoke (X2).
- [ ] With the user's OK: push the branch and open a draft PR so CI covers Linux and Windows (X3).
- [ ] Whole-branch review (superpowers:requesting-code-review).
