//! Headless tests of the real window: the same key bindings and intents a
//! user triggers, against real backend state, rendered by GPUI's test
//! platform (no GPU, so they run on every CI runner).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui_kit::{Entity, TestAppContext, VisualTestContext};

use deepclean_core::config::{AppConfig, Density, Theme};
use deepclean_core::model::{ArtifactKind, Ecosystem, RiskLevel};

use super::app_view::{AppView, FolderPicker};
use crate::backend::Backend;
use crate::model::fixtures::*;
use crate::model::ui_state::{PresetId, Route, SettingsSection};
use crate::state::AppState;

/// One seeded fake home for the whole test binary: the home override is
/// process-wide, so every test sees the same one.
fn home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        let dir = dir.canonicalize().unwrap();
        seed(&dir);
        deepclean_core::paths::set_home_override(&dir);
        dir
    })
}

/// The full `void dev seed` home: worktrees in every state, agent data,
/// model stores, stale projects.
#[cfg(not(windows))]
fn seed(dir: &std::path::Path) {
    deepclean_core::testkit::seed_all(&deepclean_core::testkit::FakeHome::at(dir));
}

/// The shared seeds build git worktrees under canonical paths, which on
/// Windows are verbatim (`\\?\C:\…`) and rejected by git. The window tests
/// only need cleanable results with nested items, so Windows gets a git-free
/// home with the same shape: projects with `node_modules`, agent data and a
/// stale Python venv.
#[cfg(windows)]
fn seed(dir: &std::path::Path) {
    let home = deepclean_core::testkit::FakeHome::at(dir);
    for project in ["code/webapp", "code/api", "code/agent-spike-todo-app"] {
        home.file(format!("{project}/package.json"), "{}");
        home.sized_file(format!("{project}/node_modules/left-pad/index.js"), 2048);
    }
    // Set here rather than with `testkit::age`: Windows only changes a
    // file's times through a handle opened for writing.
    let session = home.sized_file(".claude/projects/-code-webapp/session.jsonl", 4096);
    let when = std::time::SystemTime::now() - Duration::from_secs(90 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&session)
        .and_then(|f| f.set_modified(when))
        .expect("age the session file");
    home.file("code/tool/pyproject.toml", "[project]\nname = 'tool'\n");
    home.file("code/tool/.venv/pyvenv.cfg", "home = /usr/bin\n");
    home.sized_file("code/tool/.venv/lib/site.py", 1024);
}

fn picker(answer: Option<PathBuf>) -> FolderPicker {
    std::rc::Rc::new(move |_cx| {
        let (tx, rx) = futures::channel::oneshot::channel();
        let _ = tx.send(answer.clone());
        rx
    })
}

struct Harness {
    config_dir: PathBuf,
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    config: Option<&str>,
    answer: Option<PathBuf>,
) -> (Entity<AppView>, &'a mut VisualTestContext, Harness) {
    let home = home().clone();
    let config_dir = tempfile::tempdir().unwrap().keep();
    if let Some(text) = config {
        std::fs::write(config_dir.join(AppConfig::FILE_NAME), text).unwrap();
    }
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::bind_keys(cx);
    });
    let state = Arc::new(AppState::load_from(config_dir.clone()));
    state.config().scan_roots = vec![home];
    let (backend, events) = Backend::new(state);
    let (view, vcx) = cx.add_window_view(move |window, cx| {
        AppView::new(backend, events, None, picker(answer), window, cx)
    });
    (view, vcx, Harness { config_dir })
}

/// Let background work (tokio threads) report back, repainting as it goes.
fn settle(
    cx: &mut VisualTestContext,
    view: &Entity<AppView>,
    mut done: impl FnMut(&AppView) -> bool,
) {
    cx.executor().allow_parking();
    for _ in 0..600 {
        cx.run_until_parked();
        if view.read_with(cx, |v, _| done(v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("background work did not finish in time");
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn shortcuts_switch_screens_and_every_screen_renders(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    for (keys, route) in [
        ("secondary-2", Route::Agents),
        ("secondary-3", Route::History),
        ("secondary-4", Route::Settings),
        ("secondary-1", Route::Results),
        ("secondary-,", Route::Settings),
    ] {
        cx.simulate_keystrokes(keys);
        draw(cx);
        assert_eq!(view.read_with(cx, |v, _| v.ui.route), route, "{keys}");
    }
    for section in SettingsSection::ALL {
        view.update(cx, |v, cx| v.settings_section(section, cx));
        draw(cx);
    }
}

#[gpui_kit::test]
fn a_corrupt_settings_file_is_reported_once_and_defaults_load(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, Some("{ broken"), None);
    let toast = view.read_with(cx, |v, _| v.toast.as_ref().map(|t| t.text.clone()));
    assert!(toast.unwrap().starts_with("Settings reset to defaults"));
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.config.staleness_threshold_days),
        30
    );
    assert!(view
        .read_with(cx, |v, _| v.backend.state().take_startup_warnings())
        .is_empty());
}

fn fixture(
    view: &Entity<AppView>,
    cx: &mut VisualTestContext,
) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    view.update(cx, |v, cx| {
        let mut wt = cleanable("/u/repo/.claude/worktrees/fix", 9000);
        wt.ecosystem = Ecosystem::Worktrees;
        wt.kind = ArtifactKind::AgentWorktree;
        wt.details = vec![
            detail("Branch", "worktree-fix"),
            detail("Uncommitted changes", "2 files"),
        ];
        let nm = cleanable("/u/repo/.claude/worktrees/fix/node_modules", 4000);
        let mut danger = cleanable("/u/sim", 100);
        danger.available_actions[0].risk = RiskLevel::Danger;
        let ids = (wt.id, nm.id, danger.id);
        v.ui.has_scanned = true;
        v.ui.items = vec![wt, nm, danger];
        v.changed(cx);
        ids
    })
}

#[gpui_kit::test]
fn keyboard_moves_selects_opens_and_escapes(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (wt, _, _) = fixture(&view, cx);
    draw(cx);
    cx.simulate_keystrokes("down");
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.focused),
        Some(wt),
        "AI groups come first"
    );
    cx.simulate_keystrokes("space");
    assert!(view.read_with(cx, |v, _| v.ui.selected.contains(&wt)));
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.drawer), Some(wt));
    cx.simulate_keystrokes("alt-down");
    assert_ne!(view.read_with(cx, |v, _| v.ui.drawer), Some(wt));
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.ui.drawer), None);
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.ui.selected.is_empty()));

    cx.simulate_keystrokes("secondary-alt-a");
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.selected.len()),
        2,
        "safe items only"
    );
    cx.simulate_keystrokes("secondary-shift-a");
    assert!(view.read_with(cx, |v, _| v.ui.selected.is_empty()));
    cx.simulate_keystrokes("secondary-a");
    assert_eq!(view.read_with(cx, |v, _| v.ui.selected.len()), 3);
    cx.simulate_keystrokes("secondary-enter");
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.ui.show_confirm));
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |v, _| v.ui.show_confirm));
    cx.simulate_keystrokes("secondary-shift-e");
    assert!(view.read_with(cx, |v, _| !v.ui.collapsed.is_empty()));
}

#[gpui_kit::test]
fn typing_in_the_filter_filters_and_never_triggers_list_shortcuts(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    fixture(&view, cx);
    draw(cx);
    cx.simulate_keystrokes("/");
    cx.simulate_input("node modules");
    draw(cx);
    let (filter, selected) = view.read_with(cx, |v, _| (v.ui.filter.clone(), v.ui.selected.len()));
    assert_eq!(filter, "node modules");
    assert_eq!(selected, 0, "a typed space must not toggle a row");
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.ui.filter.is_empty()));
}

#[gpui_kit::test]
fn danger_needs_typed_confirmation(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (_, _, danger) = fixture(&view, cx);
    view.update_in(cx, |v, window, cx| {
        v.ui.selected.insert(danger);
        v.open_review(window, cx);
    });
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.ui.build_plan().has_danger));
    let input = view.read_with(cx, |v, _| v.confirm_input.clone());
    cx.update(|window, cx| input.update(cx, |s, cx| s.focus(window, cx)));
    cx.simulate_input("delete");
    assert_eq!(view.read_with(cx, |v, _| v.confirm_text.clone()), "delete");
}

#[gpui_kit::test]
fn settings_are_saved_and_look_changes_apply_live(cx: &mut TestAppContext) {
    let (view, cx, h) = open(cx, None, Some(PathBuf::from("/tmp/extra-root")));
    view.update_in(cx, |v, window, cx| {
        v.ui.config.ui.theme = Theme::Dark;
        v.apply_look(window, cx);
        v.toggle_density(window, cx);
    });
    assert!(view.read_with(cx, |v, _| v.p.dark));
    view.update(cx, |v, cx| {
        v.pick_folder(cx, |this, dir, cx| {
            this.ui.config.scan_roots.push(dir);
            this.save_config(true, cx);
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let saved = AppConfig::load(&h.config_dir.join(AppConfig::FILE_NAME)).0;
    assert_eq!(saved.ui.density, Density::Compact);
    assert_eq!(saved.ui.theme, Theme::Dark);
    assert!(saved.scan_roots.contains(&PathBuf::from("/tmp/extra-root")));
    assert!(view.read_with(cx, |v, _| v.ui.settings_dirty));
    assert!(view.read_with(cx, |v, _| v.saved_flash));
}

#[gpui_kit::test]
fn exclude_then_undo(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (_, nm, _) = fixture(&view, cx);
    view.update(cx, |v, cx| v.exclude(nm, cx));
    assert!(view.read_with(cx, |v, _| v.ui.item(nm).is_none()));
    assert!(view.read_with(cx, |v, _| v
        .toast
        .as_ref()
        .is_some_and(|t| t.undo.is_some())));
    view.update(cx, |v, cx| v.undo_toast(cx));
    assert!(view.read_with(cx, |v, _| v.ui.item(nm).is_some()));
    assert!(view.read_with(cx, |v, _| v.ui.config.blocked_paths.is_empty()));
}

#[gpui_kit::test]
fn renders_every_overlay_in_both_themes(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (wt, _, _) = fixture(&view, cx);
    for theme in [Theme::Light, Theme::Dark] {
        view.update_in(cx, |v, window, cx| {
            v.ui.config.ui.theme = theme;
            v.apply_look(window, cx);
            v.ui.drawer = Some(wt);
        });
        draw(cx);
        view.update(cx, |v, _| {
            v.ui.drawer = None;
            v.ui.selected.insert(wt);
            v.ui.show_confirm = true;
        });
        draw(cx);
        view.update(cx, |v, _| {
            v.ui.show_confirm = false;
            v.ui.denied_paths.insert("/u/locked".into());
            v.ui.record_issue("boom".into());
            v.ui.show_issues = true;
        });
        draw(cx);
        view.update_in(cx, |v, window, cx| {
            v.ui.show_issues = false;
            v.apply_preset(PresetId::IdleWorktrees, window, cx);
        });
        draw(cx);
    }
}

/// The whole loop a user runs: scan a seeded home, select the safe items,
/// clean them, and find the run in History with the files gone from disk.
#[gpui_kit::test]
fn scan_select_clean_and_see_it_in_history(cx: &mut TestAppContext) {
    // The scan and clean run on real tokio threads.
    cx.executor().allow_parking();
    let (view, cx, _h) = open(cx, None, None);
    cx.simulate_keystrokes("secondary-r");
    assert!(view.read_with(cx, |v, _| v.ui.scanning));
    settle(cx, &view, |v| !v.ui.scanning);
    draw(cx);
    let (count, first_group) = view.read_with(cx, |v, _| {
        (v.ui.items.len(), v.ui.groups().first().map(|g| g.eco))
    });
    let expected = if cfg!(windows) { 3 } else { 10 };
    assert!(
        count >= expected,
        "seeded home should yield results, got {count}"
    );
    if cfg!(not(windows)) {
        assert!(matches!(
            first_group,
            Some(Ecosystem::Worktrees | Ecosystem::AgentData | Ecosystem::Models)
        ));
    }

    // Pick safe node_modules directories: plain deletes whose effect is easy to check.
    let targets: Vec<(uuid::Uuid, PathBuf)> = view.read_with(cx, |v, _| {
        v.ui.items
            .iter()
            .filter(|i| i.kind == ArtifactKind::NodeModules && v.ui.risk_of(i) == RiskLevel::Safe)
            .map(|i| (i.id, i.path.clone()))
            .collect()
    });
    assert!(
        !targets.is_empty(),
        "the seeded home has node_modules folders"
    );
    view.update(cx, |v, cx| {
        v.ui.selected.extend(targets.iter().map(|t| t.0));
        v.changed(cx);
    });
    cx.simulate_keystrokes("secondary-enter");
    assert!(view.read_with(cx, |v, _| v.ui.show_confirm));
    view.update(cx, |v, cx| v.clean_selection(cx));
    settle(cx, &view, |v| {
        v.ui.clean.is_none() && v.ui.last_result.is_some()
    });
    draw(cx);

    let result = view.read_with(cx, |v, _| v.ui.last_result.clone().unwrap());
    assert_eq!(result.failed, 0, "{result:?}");
    assert_eq!(result.succeeded, targets.len());
    for (_, path) in &targets {
        assert!(!path.exists(), "{} should be gone", path.display());
    }
    cx.simulate_keystrokes("secondary-3");
    draw(cx);
    let runs = view.read_with(cx, |v, _| v.history.as_ref().map(|h| h.run_count));
    assert_eq!(runs, Some(1));
    view.update(cx, |v, cx| {
        let id = v.history.as_ref().unwrap().runs[0].id;
        v.ui.open_run = Some(id);
        cx.notify();
    });
    draw(cx);
}

/// Hooks and the MCP snippet need the `void` CLI; with a stand-in on PATH
/// they install into (and uninstall from) the sandbox home's settings.
#[gpui_kit::test]
fn hooks_install_and_uninstall_in_the_sandbox_home(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let bin = tempfile::tempdir().unwrap().keep();
    let exe = bin.join(if cfg!(windows) { "void.exe" } else { "void" });
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    // SAFETY: set before the check reads it; no other test depends on PATH.
    unsafe { std::env::set_var("PATH", path) };

    cx.simulate_keystrokes("secondary-2");
    draw(cx);
    let mcp = view
        .read_with(cx, |v, _| v.mcp.clone())
        .expect("MCP snippet with the CLI on PATH");
    // The path appears JSON-escaped (Windows backslashes are doubled).
    let escaped = serde_json::to_string(&exe.display().to_string()).unwrap();
    assert!(mcp.json.contains(escaped.trim_matches('"')), "{}", mcp.json);
    assert_eq!(mcp.command, "claude mcp add void -- void mcp");

    view.update(cx, |v, cx| v.hooks(true, cx));
    draw(cx);
    let installed = view.read_with(cx, |v, _| v.hooks.clone()).unwrap();
    assert!(installed.status.installed, "{:?}", installed.status);
    assert!(installed.status.settings_path.starts_with(home()));
    view.update(cx, |v, cx| v.hooks(false, cx));
    assert!(
        !view
            .read_with(cx, |v, _| v.hooks.clone())
            .unwrap()
            .status
            .installed
    );
}

/// "Clean this item" on a Danger action opens the review with `delete`
/// required; it never runs on one click.
#[gpui_kit::test]
fn cleaning_a_danger_item_from_the_drawer_goes_through_review(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (wt, _, danger) = fixture(&view, cx);
    view.update_in(cx, |v, window, cx| {
        v.ui.selected.insert(wt);
        v.ui.drawer = Some(danger);
        v.clean_one(danger, window, cx);
    });
    draw(cx);
    let (confirm, only, cleaning, has_danger) = view.read_with(cx, |v, _| {
        (
            v.ui.show_confirm,
            v.ui.confirm_only,
            v.ui.clean.is_some(),
            v.ui.build_plan().has_danger,
        )
    });
    assert!(confirm && !cleaning, "must ask before running");
    assert_eq!(
        only,
        Some(danger),
        "the dialog is about that one item, not the selection"
    );
    assert!(has_danger);
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.ui.confirm_only.is_none()));
}
