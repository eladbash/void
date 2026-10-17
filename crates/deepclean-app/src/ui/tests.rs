//! Headless tests of the real window: the same key bindings and intents a
//! user triggers, against real backend state, rendered by GPUI's test
//! platform (no GPU, so they run on every CI runner).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui_kit::{Entity, Modifiers, TestAppContext, VisualTestContext};

use deepclean_core::config::{AppConfig, Density, Grouping, Theme};
use deepclean_core::model::{ArtifactKind, Ecosystem, RiskLevel};

use super::app_view::{AppView, FolderPicker};
use crate::backend::Backend;
use crate::model::fixtures::*;
use crate::model::ui_state::{Menu, PresetId, Route, SettingsSection, SortKey};
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
    let config_dir = tempfile::tempdir().unwrap().keep();
    if let Some(text) = config {
        std::fs::write(config_dir.join(AppConfig::FILE_NAME), text).unwrap();
    }
    open_in(cx, config_dir, answer)
}

/// Open the window on an existing config folder, as a relaunch would.
fn open_in(
    cx: &mut TestAppContext,
    config_dir: PathBuf,
    answer: Option<PathBuf>,
) -> (Entity<AppView>, &mut VisualTestContext, Harness) {
    let home = home().clone();
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
        // Old enough to pass the default Stale only filter.
        v.ui.items = vec![wt, nm, danger]
            .into_iter()
            .map(|mut i| {
                i.days_stale = Some(90);
                i
            })
            .collect();
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
fn the_filters_menu_explains_and_toggles_each_filter(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    let (_, _, danger) = fixture(&view, cx);
    view.update(cx, |v, cx| {
        // A fresh item: hidden by the default Stale only.
        let mut fresh = cleanable("/u/fresh", 5_000_000_000);
        fresh.days_stale = Some(1);
        v.ui.items.push(fresh);
        v.changed(cx);
    });
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.hidden_count()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.ui.filter_summary()), "Stale");

    view.update(cx, |v, cx| {
        v.ui.toggle_menu(Menu::Filters);
        cx.notify();
    });
    draw(cx);
    // Down to Large only, toggle it; the menu stays open for more changes.
    cx.simulate_keystrokes("down space");
    draw(cx);
    let (large, menu) = view.read_with(cx, |v, _| (v.ui.large_only, v.ui.menu));
    assert!(large);
    assert_eq!(menu, Some(Menu::Filters));
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.filter_summary()),
        "Stale · ≥1 GB"
    );
    assert!(
        view.read_with(cx, |v, _| v.ui.item(danger).is_some()
            && !v.ui.visible_items().iter().any(|i| i.id == danger)),
        "the 100 B item is hidden by Large only"
    );

    cx.simulate_keystrokes("escape");
    let (menu, large) = view.read_with(cx, |v, _| (v.ui.menu, v.ui.large_only));
    assert_eq!(menu, None, "Esc closes the menu first");
    assert!(large, "and changes nothing");

    // Show everything: the last row, reached by wrapping upwards.
    view.update(cx, |v, cx| {
        v.ui.toggle_menu(Menu::Filters);
        cx.notify();
    });
    cx.simulate_keystrokes("up enter");
    draw(cx);
    let (stale, large, menu, hidden) = view.read_with(cx, |v, _| {
        (
            v.ui.stale_only(),
            v.ui.large_only,
            v.ui.menu,
            v.ui.hidden_count(),
        )
    });
    assert!(!stale && !large && menu.is_none());
    assert_eq!(hidden, 0);
}

#[gpui_kit::test]
fn group_and_sort_pick_from_a_list_and_are_saved(cx: &mut TestAppContext) {
    let (view, cx, h) = open(cx, None, None);
    fixture(&view, cx);
    let open_menu = |view: &Entity<AppView>, cx: &mut VisualTestContext, menu| {
        view.update(cx, |v, cx| {
            v.ui.toggle_menu(menu);
            cx.notify();
        });
        draw(cx);
    };

    open_menu(&view, cx, Menu::Sort);
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.menu_cursor),
        0,
        "cursor on Size"
    );
    // Two down from Size is Name; Enter picks it once and closes the menu.
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    let (sort, menu) = view.read_with(cx, |v, _| (v.ui.config.ui.sort, v.ui.menu));
    assert_eq!(sort, SortKey::Name);
    assert_eq!(menu, None);

    open_menu(&view, cx, Menu::Group);
    cx.simulate_keystrokes("down escape");
    let (grouping, menu) = view.read_with(cx, |v, _| (v.ui.config.ui.grouping, v.ui.menu));
    assert_eq!(grouping, Grouping::Ecosystem, "Esc changes nothing");
    assert_eq!(menu, None);

    open_menu(&view, cx, Menu::Group);
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.config.ui.grouping),
        Grouping::Risk
    );

    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let saved = AppConfig::load(&h.config_dir.join(AppConfig::FILE_NAME)).0;
    assert_eq!(saved.ui.sort, SortKey::Name);
    assert_eq!(saved.ui.grouping, Grouping::Risk);
}

#[gpui_kit::test]
fn deselect_all_is_one_click_after_select_all_safe(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    fixture(&view, cx);
    view.update(cx, |v, cx| {
        // Safe but young: hidden by Stale only, so not counted or selected.
        let mut fresh = cleanable("/u/fresh", 10);
        fresh.days_stale = Some(1);
        v.ui.items.push(fresh);
        v.changed(cx);
    });
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.safe_count()), 2);
    cx.simulate_keystrokes("secondary-alt-a");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.ui.selected.len()),
        2,
        "the button's count is what it selects"
    );

    let at = cx.debug_bounds("deselect-all").expect("Deselect all shown");
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.ui.selected.is_empty()));
    assert!(
        cx.debug_bounds("deselect-all").is_none(),
        "gone with the selection"
    );
}

#[gpui_kit::test]
fn the_filters_chip_opens_its_menu_and_an_outside_click_closes_it(cx: &mut TestAppContext) {
    let (view, cx, _h) = open(cx, None, None);
    fixture(&view, cx);
    draw(cx);
    let chip = cx.debug_bounds("filters-chip").expect("Filters chip");
    cx.simulate_click(chip.center(), Modifiers::none());
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.menu), Some(Menu::Filters));
    assert!(
        view.read_with(cx, |v, _| v.ui.stale_only()),
        "opening toggles nothing"
    );

    // Clicking another chip while one menu is open only closes the first.
    let sort = cx.debug_bounds("sort-chip").expect("Sort chip");
    cx.simulate_click(sort.center(), Modifiers::none());
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.menu), None);
    cx.simulate_click(sort.center(), Modifiers::none());
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.ui.menu), Some(Menu::Sort));
    let name = cx.debug_bounds("sort-row-2").expect("Name row");
    cx.simulate_click(name.center(), Modifiers::none());
    draw(cx);
    let (sort_key, menu) = view.read_with(cx, |v, _| (v.ui.config.ui.sort, v.ui.menu));
    assert_eq!(sort_key, SortKey::Name, "a clicked row applies");
    assert_eq!(menu, None);

    cx.simulate_click(chip.center(), Modifiers::none());
    draw(cx);
    // Far from the menu: closes it and reaches nothing underneath.
    let below = chip.center() + gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(400.));
    cx.simulate_click(below, Modifiers::none());
    draw(cx);
    let (menu, drawer) = view.read_with(cx, |v, _| (v.ui.menu, v.ui.drawer));
    assert_eq!(menu, None);
    assert_eq!(drawer, None, "the click did not open a row");
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

#[gpui_kit::test]
fn update_checks_stay_quiet_at_launch_and_report_a_manual_failure(cx: &mut TestAppContext) {
    use crate::model::update::UpdateStatus;
    // The test home is a sandbox with no local feed, so nothing may reach the
    // network: the launch check doesn't even start.
    let (view, cx, _h) = open(cx, None, None);
    draw(cx);
    view.read_with(cx, |v, _| {
        assert_eq!(v.update, UpdateStatus::Idle);
        assert!(v.ui.config.updates.last_checked_at.is_none());
    });

    // "Check for updates" says why it couldn't.
    view.update(cx, |v, cx| {
        v.settings_section(SettingsSection::About, cx);
        v.check_for_updates(true, cx);
    });
    settle(cx, &view, |v| matches!(v.update, UpdateStatus::Failed(_)));
    draw(cx);
    let summary = view.read_with(cx, |v, _| v.update.summary("1.2.1"));
    assert!(summary.contains("sandbox"), "{summary}");

    // Turning the automatic check off is saved like any other setting.
    view.update(cx, |v, cx| {
        v.ui.config.updates.check_automatically = false;
        v.commit_config(cx);
    });
    assert!(!view.read_with(cx, |v, _| v
        .backend
        .state()
        .config()
        .updates
        .check_automatically));
}

#[gpui_kit::test]
fn the_update_banner_can_be_dismissed_or_skipped_and_drives_the_tray_item(cx: &mut TestAppContext) {
    // An earlier check found 9.0.0; the notice survives a relaunch.
    let config = r#"{"updates": {"check_automatically": false,
        "latest_version": "9.0.0", "latest_notes_url": "https://example.com/notes"}}"#;
    let (view, cx, h) = open(cx, Some(config), None);
    draw(cx);
    view.read_with(cx, |v, _| {
        let n = v.update_notice().expect("banner");
        assert_eq!(n.version.to_string(), "9.0.0");
        assert_eq!(n.notes_url.as_deref(), Some("https://example.com/notes"));
        assert_eq!(v.tray_update_label().as_deref(), Some("Update to v9.0.0…"));
    });

    // "Update…" shows the update in Settings → About.
    view.update(cx, |v, cx| v.show_update(cx));
    draw(cx);
    view.read_with(cx, |v, _| {
        assert_eq!(v.ui.route, Route::Settings);
        assert_eq!(v.ui.settings_section, SettingsSection::About);
    });

    // Closing the banner hides it for this session; the tray still offers it.
    view.update(cx, |v, cx| v.dismiss_update_notice(cx));
    draw(cx);
    view.read_with(cx, |v, _| {
        assert!(v.update_notice().is_none());
        assert!(v.tray_update_label().is_some());
    });

    // Skipping hides banner and tray item, and is saved.
    view.update(cx, |v, cx| v.skip_update(cx));
    draw(cx);
    view.read_with(cx, |v, _| {
        assert!(v.pending_update().is_none());
        assert!(v.tray_update_label().is_none());
    });
    let saved = std::fs::read_to_string(h.config_dir.join(AppConfig::FILE_NAME)).unwrap();
    assert!(saved.contains(r#""skipped_version": "9.0.0""#), "{saved}");
}

#[gpui_kit::test]
fn a_skipped_version_stays_hidden_until_a_newer_one(cx: &mut TestAppContext) {
    let config = r#"{"updates": {"check_automatically": false,
        "latest_version": "9.0.0", "skipped_version": "9.0.0"}}"#;
    let (view, cx, _h) = open(cx, Some(config), None);
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.update_notice().is_none()));
    view.update(cx, |v, cx| {
        v.ui.config.updates.latest_version = Some("9.1.0".into());
        cx.notify();
    });
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.update_notice().map(|n| n.version.to_string())),
        Some("9.1.0".into())
    );
}

/// The whole in-app update against fake signed releases: the only test that
/// sets the update environment, since it is process-wide.
#[cfg(unix)]
#[gpui_kit::test]
fn update_now_refuses_a_tampered_download_and_installs_a_verified_one(cx: &mut TestAppContext) {
    use crate::model::update::{InstallState, UpdateStatus};
    use deepclean_core::testkit::release::fake_release;
    use deepclean_core::update::{Check, Feed, Package};

    let (view, cx, h) = open(
        cx,
        Some(r#"{"updates": {"check_automatically": false}}"#),
        None,
    );
    let base = tempfile::tempdir().unwrap();
    let start = |view: &Entity<AppView>, cx: &mut VisualTestContext, tamper: bool| {
        let dir = base.path().join(if tamper { "bad" } else { "good" });
        let r = fake_release(&dir, "9.0.0", "1.2.1", tamper).unwrap();
        std::env::set_var("VOID_UPDATE_KEY", &r.key);
        std::env::set_var("VOID_UPDATE_TARGET", &r.installed);
        let Check::Available(update) = Feed::File(r.feed.clone())
            .check("1.2.1", Package::desktop())
            .unwrap()
        else {
            panic!("the fake release should be newer");
        };
        view.update(cx, |v, cx| {
            v.update = UpdateStatus::Available(Box::new(update));
            v.install = InstallState::Idle;
            v.start_update(cx);
        });
        settle(cx, view, |v| !v.install.busy());
        draw(cx);
        r
    };

    // A download that doesn't match its signed checksum is refused, deleted,
    // and the installed app is left alone.
    let bad = start(&view, cx, true);
    let state = view.read_with(cx, |v, _| v.install.clone());
    let InstallState::Failed(err) = state else {
        panic!("expected a failure, got {state:?}");
    };
    assert!(err.contains("checksum"), "{err}");
    let updates = h.config_dir.join("updates");
    let leftovers: Vec<_> = std::fs::read_dir(&updates)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(
        deepclean_core::update::install::bundle_version(&bad.installed).as_deref(),
        Some("1.2.1")
    );

    // A verified one is installed: the bundle swapped in on macOS, the
    // package manager's command on Linux.
    let good = start(&view, cx, false);
    let state = view.read_with(cx, |v, _| v.install.clone());
    if cfg!(target_os = "macos") {
        assert!(matches!(state, InstallState::Installed { .. }), "{state:?}");
        assert_eq!(
            deepclean_core::update::install::bundle_version(&good.installed).as_deref(),
            Some("9.0.0")
        );
    } else {
        let InstallState::Manual { command, .. } = state else {
            panic!("expected the apt command, got {state:?}");
        };
        assert!(command.starts_with("sudo apt install"), "{command}");
    }
    std::env::remove_var("VOID_UPDATE_KEY");
    std::env::remove_var("VOID_UPDATE_TARGET");
}

#[gpui_kit::test]
fn a_relaunch_opens_to_the_last_scan_and_refreshes_it_keeping_the_selection(
    cx: &mut TestAppContext,
) {
    // Scans and the cache write run on real tokio threads.
    cx.executor().allow_parking();
    let (view, vcx, h) = open(cx, None, None);
    vcx.simulate_keystrokes("secondary-r");
    settle(vcx, &view, |v| v.ui.has_scanned && !v.ui.scanning);
    let found = view.read_with(vcx, |v, _| v.ui.items.len());
    assert!(found > 0);
    // The cache is written off the UI thread.
    let cache = h.config_dir.join(crate::backend::cache::FILE_NAME);
    for _ in 0..200 {
        if cache.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cache.exists(), "scan cache was not written");

    // "Relaunch": a new window on the same settings folder.
    let (view, vcx, _h) = open_in(cx, h.config_dir.clone(), None);
    // The fake home is shared, and another test cleans its node_modules
    // folders; select something nothing else deletes.
    let (items, refreshing, first) = view.read_with(vcx, |v, _| {
        (
            v.ui.items.len(),
            v.ui.refreshing,
            v.ui.items
                .iter()
                .find(|i| i.kind != ArtifactKind::NodeModules)
                .map(|i| (i.id, i.path.clone())),
        )
    });
    assert_eq!(items, found, "the last scan shows at once");
    assert!(refreshing, "and is refreshed in the background");
    draw(vcx);

    // Selecting while it refreshes survives the merge, by path.
    let (id, first) = first.unwrap();
    view.update(vcx, |v, cx| {
        v.ui.selected.insert(id);
        v.ui.filter = "a".into();
        cx.notify();
    });
    settle(vcx, &view, |v| !v.ui.scanning);
    view.read_with(vcx, |v, _| {
        assert!(!v.ui.refreshing);
        assert_eq!(v.ui.filter, "a");
        assert_eq!(v.ui.selected.len(), 1);
        let id = *v.ui.selected.iter().next().unwrap();
        assert_eq!(v.ui.item(id).map(|i| i.path.clone()), Some(first.clone()));
    });
}

#[gpui_kit::test]
fn a_corrupt_scan_cache_is_ignored(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap().keep();
    std::fs::write(dir.join(crate::backend::cache::FILE_NAME), "{ not json").unwrap();
    let (view, vcx, _h) = open_in(cx, dir, None);
    draw(vcx);
    view.read_with(vcx, |v, _| {
        assert!(!v.ui.has_scanned);
        assert!(!v.ui.scanning);
    });
}
