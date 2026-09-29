//! The root view: owns the UI state and the backend handle, drains their
//! events, and draws the rail, the current screen and any overlay.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::{
    div, prelude::*, px, App, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    ListAlignment, ListState, PathPromptOptions, Subscription, Task, Window,
};
use uuid::Uuid;

use deepclean_core::attribution::AgentUsage;
use deepclean_core::config::{AppConfig, Density};
use deepclean_core::model::{ActionEvent, ScanEvent};

use super::components::*;
use super::icons::icon;
use super::theme::{Palette, RAIL_W};
use super::*;
use crate::backend::integrations::{self, HookView, McpSnippet};
use crate::backend::{Backend, GuardView, HistoryView, UiEvent};
use crate::model::format::format_bytes;
use crate::model::labels::kind_label;
use crate::model::ui_state::{Excluded, PresetId, Route, SettingsSection, UiState};
use crate::platform::tray::{Tray, TrayCommand};

/// How often the list repaints while a scan streams items in. Items arrive
/// faster than anyone can read them; repainting on each is wasted work.
const SCAN_REPAINT: Duration = Duration::from_millis(250);
/// Settings are written this long after the last change.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// One entry of the results list: a group header or a row.
#[derive(Clone)]
pub(crate) enum Row {
    Group {
        key: String,
        label: String,
        eco: deepclean_core::model::Ecosystem,
        count: usize,
        bytes: u64,
        share: f32,
        check: Option<bool>,
        selectable: bool,
    },
    Item {
        id: Uuid,
        group_max: u64,
    },
}

pub(crate) struct Toast {
    pub text: String,
    pub undo: Option<Excluded>,
}

/// Opens a folder picker; swapped for a canned answer in tests.
pub type FolderPicker =
    Rc<dyn Fn(&mut App) -> futures::channel::oneshot::Receiver<Option<PathBuf>>>;

pub fn native_folder_picker() -> FolderPicker {
    Rc::new(|cx: &mut App| {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let (tx, out) = futures::channel::oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                let picked = rx
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .flatten()
                    .and_then(|mut v| v.pop());
                let _ = tx.send(picked);
            })
            .detach();
        out
    })
}

pub struct AppView {
    pub(crate) ui: UiState,
    pub(crate) backend: Backend,
    pub(crate) p: Palette,
    pub(crate) focus: FocusHandle,
    pub(crate) filter_input: Entity<InputState>,
    pub(crate) confirm_input: Entity<InputState>,
    pub(crate) number_inputs: HashMap<&'static str, Entity<InputState>>,
    pub(crate) sliders: HashMap<&'static str, Entity<SliderState>>,
    pub(crate) list: ListState,
    pub(crate) rows: Vec<Row>,
    pub(crate) row_index: HashMap<Uuid, usize>,
    rows_dirty: bool,
    pub(crate) toast: Option<Toast>,
    toast_task: Option<Task<()>>,
    pub(crate) saved_flash: bool,
    save_task: Option<Task<()>>,
    flash_task: Option<Task<()>>,
    pub(crate) guard: Option<GuardView>,
    pub(crate) guard_checking: bool,
    pub(crate) agent_usage: Option<Vec<AgentUsage>>,
    pub(crate) hooks: Option<HookView>,
    pub(crate) hooks_error: Option<String>,
    pub(crate) mcp: Option<McpSnippet>,
    pub(crate) mcp_error: Option<String>,
    pub(crate) history: Option<HistoryView>,
    pub(crate) config_path: String,
    pub(crate) home: String,
    pub(crate) confirm_text: String,
    pub(crate) cores: usize,
    tray: Option<Rc<Tray>>,
    sandboxed: bool,
    picker: FolderPicker,
    last_paint: Instant,
    repaint_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AppView {
    pub fn new(
        backend: Backend,
        events: UnboundedReceiver<UiEvent>,
        tray: Option<(Rc<Tray>, UnboundedReceiver<TrayCommand>)>,
        picker: FolderPicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = backend.state().config().clone();
        let home = config
            .scan_roots
            .first()
            .map(|r| r.display().to_string().trim_end_matches('/').to_string())
            .unwrap_or_default();
        set_app_appearance(config.ui.theme, cx);
        let p = Palette::resolve(config.ui.theme, window.appearance());
        super::theme::sync_component_theme(&p, cx);
        let filter_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Filter by path, project, or type"));
        let confirm_input = cx.new(|cx| InputState::new(window, cx));

        let mut subscriptions = vec![];
        subscriptions.push(cx.subscribe_in(
            &filter_input,
            window,
            |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.ui.filter = input.read(cx).value().to_string();
                    this.list.scroll_to(Default::default());
                    this.changed(cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &confirm_input,
            window,
            |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    this.confirm_text = input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            this.p = Palette::resolve(this.ui.config.ui.theme, window.appearance());
            super::theme::sync_component_theme(&this.p, cx);
            cx.notify();
        }));

        let mut tasks = vec![Self::pump_backend(events, cx)];
        let tray = tray.map(|(tray, commands)| {
            tasks.push(Self::pump_tray(commands, window, cx));
            tray
        });

        let mut ui = UiState::new(config);
        ui.disk = backend.disk_usage();
        let mut this = Self {
            ui,
            config_path: backend.state().config_path().display().to_string(),
            history: Some(backend.history_view()),
            guard: Some(backend.guard_view()),
            backend,
            p,
            focus: cx.focus_handle(),
            filter_input,
            confirm_input,
            number_inputs: HashMap::new(),
            sliders: HashMap::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: vec![],
            row_index: HashMap::new(),
            rows_dirty: true,
            toast: None,
            toast_task: None,
            saved_flash: false,
            save_task: None,
            flash_task: None,
            guard_checking: false,
            agent_usage: None,
            hooks: None,
            hooks_error: None,
            mcp: None,
            mcp_error: None,
            home,
            confirm_text: String::new(),
            cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8),
            tray,
            sandboxed: deepclean_core::paths::home_override().is_some(),
            picker,
            last_paint: Instant::now(),
            repaint_task: None,
            _subscriptions: subscriptions,
            _tasks: tasks,
        };
        this.build_settings_controls(window, cx);
        let warnings = this.backend.state().take_startup_warnings();
        if let Some(first) = warnings.into_iter().next() {
            this.show_toast(first, None, cx);
        }
        this.refresh_tray();
        window.focus(&this.focus, cx);
        this
    }

    // ── Event pumps ─────────────────────────────────────────────────────

    fn pump_backend(mut events: UnboundedReceiver<UiEvent>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(first) = events.next().await {
                // Drain whatever else is already queued so a burst of scan
                // events costs one update, not hundreds.
                let mut batch = vec![first];
                while let Ok(ev) = events.try_recv() {
                    batch.push(ev);
                    if batch.len() >= 512 {
                        break;
                    }
                }
                if this
                    .update(cx, |this, cx| this.on_backend(batch, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    fn pump_tray(
        mut commands: UnboundedReceiver<TrayCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(cmd) = commands.next().await {
                let ok = this.update_in(cx, |this, window, cx| this.on_tray(cmd, window, cx));
                if ok.is_err() {
                    break;
                }
            }
        })
    }

    fn on_backend(&mut self, batch: Vec<UiEvent>, cx: &mut Context<Self>) {
        let mut scan_progress = false;
        for ev in batch {
            match ev {
                UiEvent::Scan(ev) => {
                    let complete = matches!(ev, ScanEvent::ScanComplete { .. });
                    self.ui.apply_scan_event(ev);
                    scan_progress = true;
                    if complete {
                        self.after_scan();
                    }
                }
                UiEvent::ScanFinished => {
                    if self.ui.scanning {
                        self.ui.finish_scan();
                        self.after_scan();
                    }
                }
                UiEvent::Action(ev) => {
                    let done = matches!(ev, ActionEvent::BatchComplete { .. });
                    self.ui.apply_action_event(ev);
                    if done {
                        self.history = Some(self.backend.history_view());
                        self.ui.disk = self.backend.disk_usage();
                    }
                }
                UiEvent::GuardStatus(view) => self.guard = Some(view),
                UiEvent::GuardCleaned(path) => self.ui.remove_path(&path),
                UiEvent::HistoryChanged => {
                    self.history = Some(self.backend.history_view());
                    self.ui.disk = self.backend.disk_usage();
                    self.guard = Some(self.backend.guard_view());
                }
                UiEvent::Warning(msg) => self.show_toast(msg, None, cx),
                UiEvent::Notify { title, body } => {
                    // Notification delivery can block on D-Bus or the
                    // notification centre; keep it off the UI thread.
                    std::thread::spawn(move || crate::platform::notify::notify(&title, &body));
                }
                UiEvent::TrayRefresh => {}
            }
        }
        self.refresh_tray();
        self.rows_dirty = true;
        if scan_progress && self.ui.scanning {
            self.throttled_notify(cx);
        } else {
            cx.notify();
        }
    }

    fn after_scan(&mut self) {
        self.ui.disk = self.backend.disk_usage();
        self.agent_usage = Some(self.backend.agent_usage());
    }

    fn throttled_notify(&mut self, cx: &mut Context<Self>) {
        let since = self.last_paint.elapsed();
        if since >= SCAN_REPAINT {
            self.last_paint = Instant::now();
            cx.notify();
            return;
        }
        if self.repaint_task.is_some() {
            return;
        }
        let wait = SCAN_REPAINT - since;
        self.repaint_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.repaint_task = None;
                this.last_paint = Instant::now();
                cx.notify();
            });
        }));
    }

    fn on_tray(&mut self, cmd: TrayCommand, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            TrayCommand::ScanNow => {
                window.activate_window();
                self.start_scan(cx);
            }
            TrayCommand::OpenDashboard => window.activate_window(),
            TrayCommand::TrimIdleWorktrees => {
                window.activate_window();
                self.apply_preset(PresetId::IdleWorktrees, cx);
                if !self.ui.has_scanned && !self.ui.scanning {
                    self.start_scan(cx);
                }
            }
            TrayCommand::Quit => cx.quit(),
        }
    }

    pub(crate) fn refresh_tray(&self) {
        if let Some(tray) = &self.tray {
            let free = self
                .guard
                .as_ref()
                .and_then(|g| g.status.as_ref())
                .map(|s| s.free_bytes);
            tray.refresh(free, self.ui.idle_worktrees());
        }
    }

    // ── State helpers ───────────────────────────────────────────────────

    /// Mark the list for rebuilding and repaint.
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        self.rows_dirty = true;
        cx.notify();
    }

    pub(crate) fn show_toast(
        &mut self,
        text: String,
        undo: Option<Excluded>,
        cx: &mut Context<Self>,
    ) {
        self.toast = Some(Toast { text, undo });
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn rebuild_rows(&mut self) {
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        let mut rows = vec![];
        for g in self.ui.groups() {
            let collapsed = self.ui.collapsed.contains(&g.key);
            let group_max = g
                .items
                .iter()
                .map(|i| i.size_bytes)
                .max()
                .unwrap_or(1)
                .max(1);
            rows.push(Row::Group {
                key: g.key.clone(),
                label: g.label.clone(),
                eco: g.eco,
                count: g.items.len(),
                bytes: g.bytes,
                share: g.share,
                check: self.ui.group_check_state(&g),
                selectable: g
                    .items
                    .iter()
                    .any(|i| crate::model::actions::is_actionable(i)),
            });
            if !collapsed {
                rows.extend(g.items.iter().map(|i| Row::Item {
                    id: i.id,
                    group_max,
                }));
            }
        }
        let same_shape = rows.len() == self.rows.len()
            && rows.iter().zip(&self.rows).all(|(a, b)| match (a, b) {
                (Row::Item { id: x, .. }, Row::Item { id: y, .. }) => x == y,
                (Row::Group { key: x, .. }, Row::Group { key: y, .. }) => x == y,
                _ => false,
            });
        self.row_index = self
            .ui
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| (item.id, i))
            .collect();
        if !same_shape {
            let top = self.list.logical_scroll_top();
            self.list.reset(rows.len());
            // Keep the reader's place while items stream in or a row is
            // cleaned; changes that replace the list scroll it to the top
            // themselves.
            self.list.scroll_to(top);
        }
        self.rows = rows;
    }

    // ── Intents ─────────────────────────────────────────────────────────

    pub(crate) fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        self.ui.route = route;
        match route {
            Route::History => self.history = Some(self.backend.history_view()),
            Route::Agents => self.refresh_agents(),
            _ => {}
        }
        cx.notify();
    }

    fn refresh_agents(&mut self) {
        if self.ui.has_scanned {
            self.agent_usage = Some(self.backend.agent_usage());
        }
        self.guard = Some(self.backend.guard_view());
        match integrations::hook_status() {
            Ok(h) => {
                self.hooks = Some(h);
                self.hooks_error = None;
            }
            Err(e) => self.hooks_error = Some(e),
        }
        match integrations::mcp_snippet() {
            Ok(m) => {
                self.mcp = Some(m);
                self.mcp_error = None;
            }
            Err(e) => {
                self.mcp = None;
                self.mcp_error = Some(e);
            }
        }
    }

    pub(crate) fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.ui.scanning {
            return;
        }
        self.ui.begin_scan();
        self.agent_usage = None;
        if let Err(err) = self.backend.start_scan() {
            self.ui.scanning = false;
            self.ui.record_issue(err);
        }
        self.last_paint = Instant::now();
        self.list.scroll_to(Default::default());
        self.changed(cx);
    }

    pub(crate) fn apply_preset(&mut self, id: PresetId, cx: &mut Context<Self>) {
        self.ui.apply_preset(id);
        self.list.scroll_to(Default::default());
        self.changed(cx);
    }

    pub(crate) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui.selected.is_empty() {
            return;
        }
        self.ui.show_confirm = true;
        self.confirm_text.clear();
        self.confirm_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub(crate) fn run_clean(&mut self, selections: Vec<(Uuid, Uuid)>, cx: &mut Context<Self>) {
        if selections.is_empty() {
            return;
        }
        self.ui.begin_clean(&selections);
        if let Err(err) = self.backend.execute_clean(&selections) {
            self.ui.clean = None;
            self.show_toast(err, None, cx);
        }
        self.changed(cx);
    }

    pub(crate) fn clean_selection(&mut self, cx: &mut Context<Self>) {
        let selections = self.ui.build_plan().selections();
        self.run_clean(selections, cx);
    }

    pub(crate) fn clean_one(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(item) = self.ui.item(id) else { return };
        let Some(action) = self.ui.action_for(item) else {
            return;
        };
        let sel = vec![(item.id, action.id)];
        self.ui.drawer = None;
        self.run_clean(sel, cx);
    }

    pub(crate) fn fallback(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(item) = self.ui.item(id) else { return };
        let Some(alt) = crate::model::ui_state::fallback_action(item) else {
            return;
        };
        let alt_id = alt.id;
        self.ui.choose_action(id, alt_id);
        self.run_clean(vec![(id, alt_id)], cx);
    }

    pub(crate) fn copy(&mut self, text: String, toast: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_toast(toast.into(), None, cx);
    }

    pub(crate) fn reveal(&mut self, id: Uuid, cx: &mut Context<Self>) {
        match crate::backend::clean::reveal_target(self.backend.state(), id) {
            Ok(path) => cx.reveal_path(&path),
            Err(err) => self.show_toast(err, None, cx),
        }
    }

    pub(crate) fn exclude(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(excluded) = self.ui.exclude(id) else {
            return;
        };
        let label = format!("{} excluded", kind_label(&excluded.item));
        self.save_config(false, cx);
        self.show_toast(label, Some(excluded), cx);
        self.changed(cx);
    }

    pub(crate) fn undo_toast(&mut self, cx: &mut Context<Self>) {
        if let Some(Toast { undo: Some(ex), .. }) = self.toast.take() {
            self.ui.undo_exclude(ex);
            self.save_config(false, cx);
        }
        self.changed(cx);
    }

    pub(crate) fn guard_check(&mut self, cx: &mut Context<Self>) {
        self.guard_checking = true;
        let backend = self.backend.clone();
        let fut = self
            .backend
            .run(async move { backend.guard_check(true).await });
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.guard_checking = false;
                match result {
                    Some(Ok(view)) => this.guard = Some(view),
                    Some(Err(err)) => this.show_toast(err, None, cx),
                    None => {}
                }
                this.refresh_tray();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn hooks(&mut self, install: bool, cx: &mut Context<Self>) {
        let result = if install {
            integrations::install_hooks()
        } else {
            integrations::uninstall_hooks()
        };
        match result {
            Ok(h) => {
                self.hooks = Some(h);
                self.hooks_error = None;
                let msg = if install {
                    "Claude Code hooks installed"
                } else {
                    "Claude Code hooks removed"
                };
                self.show_toast(msg.into(), None, cx);
            }
            Err(err) => self.hooks_error = Some(err),
        }
        cx.notify();
    }

    pub(crate) fn clear_history(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.backend.clear_history() {
            self.show_toast(err, None, cx);
            return;
        }
        self.history = Some(self.backend.history_view());
        self.show_toast("History cleared".into(), None, cx);
    }

    /// Ask for a folder, then hand it to `apply`.
    pub(crate) fn pick_folder(
        &mut self,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Self, PathBuf, &mut Context<Self>) + 'static,
    ) {
        let rx = (self.picker)(cx);
        cx.spawn(async move |this, cx| {
            if let Ok(Some(dir)) = rx.await {
                let _ = this.update(cx, |this, cx| apply(this, dir, cx));
            }
        })
        .detach();
    }

    // ── Settings persistence ───────────────────────────────────────────

    /// Save the edited settings shortly, the way the old UI debounced typing.
    /// `dirty` marks a change that only takes effect on the next scan.
    pub(crate) fn save_config(&mut self, dirty: bool, cx: &mut Context<Self>) {
        if dirty {
            self.ui.settings_dirty = true;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.commit_config(cx));
        }));
        cx.notify();
    }

    /// Write the settings now and apply the ones that act on the OS.
    pub(crate) fn commit_config(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        // The file is hand-editable, so the UI's bounds are not a guarantee.
        let config = self.ui.config.clone().sanitized();
        let state = self.backend.state().clone();
        let previous = std::mem::replace(&mut *state.config(), config.clone());
        if previous.ui.show_menu_bar_icon != config.ui.show_menu_bar_icon {
            if let Some(tray) = &self.tray {
                tray.set_visible(config.ui.show_menu_bar_icon);
            }
        }
        if previous.ui.launch_at_login != config.ui.launch_at_login && !self.sandboxed {
            if let Err(err) =
                crate::platform::autostart::apply_launch_at_login(config.ui.launch_at_login)
            {
                // Keep the file honest: it must not claim a login item the
                // OS refused to register.
                state.config().ui.launch_at_login = previous.ui.launch_at_login;
                self.ui.config.ui.launch_at_login = previous.ui.launch_at_login;
                self.show_toast(err, None, cx);
            }
        }
        match state.persist_config() {
            Ok(()) => {
                self.saved_flash = true;
                self.flash_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.saved_flash = false;
                        cx.notify();
                    });
                }));
            }
            Err(err) => self.show_toast(err, None, cx),
        }
        self.refresh_tray();
        self.changed(cx);
    }

    /// Live preferences that change how the window looks.
    pub(crate) fn apply_look(&mut self, window: &Window, cx: &mut Context<Self>) {
        set_app_appearance(self.ui.config.ui.theme, cx);
        self.p = Palette::resolve(self.ui.config.ui.theme, window.appearance());
        super::theme::sync_component_theme(&self.p, cx);
        self.list.remeasure();
        self.changed(cx);
    }

    // ── Keyboard ────────────────────────────────────────────────────────

    fn on_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typing = self.filter_input.focus_handle(cx).is_focused(window);
        if typing && !self.ui.show_confirm {
            self.filter_input
                .update(cx, |s, cx| s.set_value("", window, cx));
            self.ui.filter.clear();
            window.focus(&self.focus, cx);
            self.changed(cx);
            return;
        }
        if self.ui.escape() {
            window.focus(&self.focus, cx);
            self.changed(cx);
        }
    }

    fn register_actions(&self, root: gpui_kit::Div, cx: &mut Context<Self>) -> gpui_kit::Div {
        root.on_action(cx.listener(|this, _: &GoResults, _, cx| this.navigate(Route::Results, cx)))
            .on_action(cx.listener(|this, _: &GoAgents, _, cx| this.navigate(Route::Agents, cx)))
            .on_action(cx.listener(|this, _: &GoHistory, _, cx| this.navigate(Route::History, cx)))
            .on_action(
                cx.listener(|this, _: &GoSettings, _, cx| this.navigate(Route::Settings, cx)),
            )
            .on_action(cx.listener(|this, _: &Scan, _, cx| this.start_scan(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.on_dismiss(window, cx)))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &Review, window, cx| {
                if this.ui.route == Route::Results && this.ui.clean.is_none() {
                    this.open_review(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
                if this.ui.route == Route::Results && this.ui.has_scanned {
                    this.filter_input.update(cx, |s, cx| s.focus(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                if this.ui.route == Route::Results {
                    this.ui.select_all_visible();
                    this.changed(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectNone, _, cx| {
                this.ui.selected.clear();
                this.changed(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectSafe, _, cx| {
                if this.ui.route == Route::Results {
                    this.ui.select_safe();
                    this.changed(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleDensity, window, cx| {
                if this.ui.route == Route::Results {
                    this.toggle_density(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ExpandAll, _, cx| {
                this.ui.toggle_expand_all();
                this.changed(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNext, _, cx| this.move_focus(1, cx)))
            .on_action(cx.listener(|this, _: &FocusPrev, _, cx| this.move_focus(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleFocused, _, cx| {
                if let (Route::Results, Some(id)) = (this.ui.route, this.ui.focused) {
                    this.ui.toggle_select(id);
                    this.changed(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenFocused, _, cx| {
                if let (Route::Results, Some(id)) = (this.ui.route, this.ui.focused) {
                    this.ui.drawer = Some(id);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &DrawerNext, _, cx| {
                this.ui.move_drawer(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DrawerPrev, _, cx| {
                this.ui.move_drawer(-1);
                cx.notify();
            }))
    }

    fn move_focus(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.ui.route != Route::Results || self.ui.show_confirm {
            return;
        }
        self.ui.move_focus(delta);
        self.rebuild_rows();
        if let Some(ix) = self.ui.focused.and_then(|f| {
            self.rows
                .iter()
                .position(|r| matches!(r, Row::Item { id, .. } if *id == f))
        }) {
            self.list.scroll_to_reveal_item(ix);
        }
        cx.notify();
    }

    pub(crate) fn toggle_density(&mut self, window: &Window, cx: &mut Context<Self>) {
        let ui = &mut self.ui.config.ui;
        ui.density = match ui.density {
            Density::Compact => Density::Comfortable,
            Density::Comfortable => Density::Compact,
        };
        self.apply_look(window, cx);
        self.save_config(false, cx);
    }

    // ── Rendering ───────────────────────────────────────────────────────

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let key = mod_key();
        let button = |route: Route,
                      ico: &'static str,
                      label: &'static str,
                      n: &str,
                      cx: &mut Context<Self>| {
            let current = self.ui.route == route;
            let dot = route == Route::Results && !self.ui.items.is_empty() && !current;
            let hover = p.state_hover;
            div()
                .id(SharedStringId::rail(label))
                .relative()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .map(|d| {
                    if current {
                        d.bg(p.accent_subtle).border_l_2().border_color(p.accent_bg)
                    } else {
                        d.hover(move |s| s.bg(hover))
                    }
                })
                .tooltip_text(format!("{label} ({key}{n})"))
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(route, cx)))
                .child(icon(
                    ico,
                    px(20.),
                    if current {
                        p.accent_text
                    } else {
                        p.text_tertiary
                    },
                ))
                .when(dot, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(7.))
                            .right(px(7.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(p.accent_bg),
                    )
                })
        };

        let disk = self.ui.disk.as_ref().map(|disk| {
            let reclaim = self.ui.items.iter().map(|i| i.size_bytes).sum::<u64>();
            let total = disk.total_bytes.max(1) as f32;
            let recl = (reclaim as f32 / total).min(1.);
            let used = (disk.used_bytes as f32 / total).min(1.);
            div()
                .id("rail-disk")
                .flex()
                .flex_col()
                .items_center()
                .gap(px(5.))
                .tooltip_text(format!(
                    "{} free of {}",
                    format_bytes(disk.available_bytes),
                    format_bytes(disk.total_bytes)
                ))
                .child(
                    div()
                        .w(px(4.))
                        .h(px(36.))
                        .rounded(px(2.))
                        .overflow_hidden()
                        .bg(p.chart_track)
                        .flex()
                        .flex_col()
                        .justify_end()
                        .child(
                            div()
                                .w_full()
                                .h(px(36. * (used - recl).max(0.)))
                                .bg(p.text_disabled),
                        )
                        .child(div().w_full().h(px(36. * recl)).bg(p.accent_bg)),
                )
                .child(
                    div()
                        .t_overline()
                        .text_color(p.text_tertiary)
                        .child(format!("{}%", (used * 100.).round())),
                )
        });

        let mark = if self.ui.scanning {
            spinner("rail-mark", 24., p.accent_bg).into_any_element()
        } else {
            icon("mark", px(24.), p.accent_bg).into_any_element()
        };

        div()
            .w(px(RAIL_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .pt(px(12.))
            .pb(px(16.))
            .bg(p.surface_sunken)
            .border_r_1()
            .border_color(p.border_subtle)
            .child(div().mb(px(20.)).child(mark))
            .child(button(Route::Results, "sidebar-toggle", "Results", "1", cx))
            .child(button(Route::Agents, "sparkle", "Agents", "2", cx))
            .child(button(Route::History, "history", "History", "3", cx))
            .child(button(Route::Settings, "settings", "Settings", "4", cx))
            .child(spacer())
            .children(disk)
    }

    fn render_toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let toast = self.toast.as_ref()?;
        let p = self.p;
        Some(
            div()
                .absolute()
                .left(px(RAIL_W + 16.))
                .bottom(px(40.))
                .min_w(px(280.))
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(7.))
                .bg(p.surface_overlay)
                .border_1()
                .border_color(p.border_default)
                .shadow_md()
                .child(
                    div()
                        .flex_1()
                        .t_body_sm()
                        .text_color(p.text_primary)
                        .child(toast.text.clone()),
                )
                .when(toast.undo.is_some(), |d| {
                    d.child(
                        btn("toast-undo", BtnKind::Ghost, BtnSize::Sm, false, &p)
                            .child("Undo")
                            .on_click(cx.listener(|this, _, _, cx| this.undo_toast(cx))),
                    )
                })
                .child(
                    iconbtn("toast-close", "close", 22., &p).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        },
                    )),
                ),
        )
    }
}

/// Make the native title bar follow Void's theme, not only the OS's.
/// Only macOS honours this; elsewhere it is a no-op.
fn set_app_appearance(pref: deepclean_core::config::Theme, cx: &mut App) {
    use deepclean_core::config::Theme;
    cx.set_window_appearance(match pref {
        Theme::Light => Some(gpui_kit::WindowAppearance::Light),
        Theme::Dark => Some(gpui_kit::WindowAppearance::Dark),
        Theme::System => None,
    });
}

/// Stable element ids for things tests and the accessibility tree address.
pub(crate) struct SharedStringId;

impl SharedStringId {
    pub fn rail(label: &str) -> gpui_kit::ElementId {
        gpui_kit::ElementId::Name(format!("rail-{}", label.to_lowercase()).into())
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rebuild_rows();
        let p = self.p;
        let base = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full();
        self.register_actions(base, cx).child(
            div()
                .id("void-root")
                .size_full()
                .relative()
                .flex()
                .flex_row()
                .overflow_hidden()
                .bg(p.surface_base)
                .text_color(p.text_primary)
                .font_family(".SystemUIFont")
                .text_size(px(13.))
                .child(self.render_rail(cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .relative()
                        .child({
                            match self.ui.route {
                                Route::Results => {
                                    self.render_results(window, cx).into_any_element()
                                }
                                Route::Agents => self.render_agents(window, cx).into_any_element(),
                                Route::History => {
                                    self.render_history(window, cx).into_any_element()
                                }
                                Route::Settings => {
                                    self.render_settings(window, cx).into_any_element()
                                }
                            }
                        }),
                )
                .children(
                    (self.ui.route == Route::Results && self.ui.drawer.is_some())
                        .then(|| self.render_drawer(window, cx))
                        .flatten(),
                )
                .children(
                    self.ui
                        .show_confirm
                        .then(|| self.render_confirm(window, cx)),
                )
                .children(self.ui.show_issues.then(|| self.render_issues(cx)))
                .children(
                    (self.ui.route == Route::History)
                        .then(|| self.render_run_detail(cx))
                        .flatten(),
                )
                .children(self.render_toast(cx)),
        )
    }
}

/// Keep this list in one place: the settings controls that are GPUI
/// entities (text fields and sliders), keyed by config path.
pub(crate) const NUMBER_FIELDS: &[&str] = &[
    "staleness_threshold_days",
    "ai.worktree_idle_days",
    "ai.agent_data_retention_days",
    "ai.large_session_file_mb",
    "ai.stale_project_days",
    "ai.min_duplicate_model_mb",
    "guard.check_interval_minutes",
];

pub(crate) const SLIDERS: &[&str] = &[
    "guard.warn_free_percent",
    "guard.critical_free_percent",
    "walker_threads",
    "max_concurrent_analyses",
    "cpu_threshold_percent",
];

pub(crate) fn config_number(c: &AppConfig, path: &str) -> f64 {
    match path {
        "staleness_threshold_days" => c.staleness_threshold_days as f64,
        "ai.worktree_idle_days" => c.ai.worktree_idle_days as f64,
        "ai.agent_data_retention_days" => c.ai.agent_data_retention_days as f64,
        "ai.large_session_file_mb" => c.ai.large_session_file_mb as f64,
        "ai.stale_project_days" => c.ai.stale_project_days as f64,
        "ai.min_duplicate_model_mb" => c.ai.min_duplicate_model_mb as f64,
        "guard.check_interval_minutes" => c.guard.check_interval_minutes as f64,
        "guard.warn_free_percent" => c.guard.warn_free_percent as f64,
        "guard.critical_free_percent" => c.guard.critical_free_percent as f64,
        "walker_threads" => c.walker_threads as f64,
        "max_concurrent_analyses" => c.max_concurrent_analyses as f64,
        "cpu_threshold_percent" => c.cpu_threshold_percent as f64,
        _ => 0.,
    }
}

pub(crate) fn set_config_number(c: &mut AppConfig, path: &str, v: f64) {
    let u = v.max(0.).round() as u64;
    match path {
        "staleness_threshold_days" => c.staleness_threshold_days = u,
        "ai.worktree_idle_days" => c.ai.worktree_idle_days = u,
        "ai.agent_data_retention_days" => c.ai.agent_data_retention_days = u,
        "ai.large_session_file_mb" => c.ai.large_session_file_mb = u,
        "ai.stale_project_days" => c.ai.stale_project_days = u,
        "ai.min_duplicate_model_mb" => c.ai.min_duplicate_model_mb = u,
        "guard.check_interval_minutes" => c.guard.check_interval_minutes = u,
        "guard.warn_free_percent" => c.guard.warn_free_percent = u.min(100) as u8,
        "guard.critical_free_percent" => c.guard.critical_free_percent = u.min(100) as u8,
        "walker_threads" => c.walker_threads = u as usize,
        "max_concurrent_analyses" => c.max_concurrent_analyses = u as usize,
        "cpu_threshold_percent" => c.cpu_threshold_percent = u.min(100) as u8,
        _ => {}
    }
}

/// Settings under `ai.`, the roots and the ecosystems change what a scan
/// finds; the rest do not.
pub(crate) fn affects_scan(path: &str) -> bool {
    path.starts_with("ai.") || path == "scan_roots" || path == "enabled_ecosystems"
}

impl AppView {
    fn build_settings_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for &path in NUMBER_FIELDS {
            let value = config_number(&self.ui.config, path);
            let input = cx.new(|cx| InputState::new(window, cx).default_value(format!("{value}")));
            let sub = cx.subscribe_in(
                &input,
                window,
                move |this, input, ev: &InputEvent, _, cx| {
                    if let InputEvent::Change = ev {
                        let text = input.read(cx).value().to_string();
                        if let Ok(n) = text.trim().parse::<f64>() {
                            if n.is_finite() {
                                set_config_number(&mut this.ui.config, path, n);
                                let dirty = if path == "staleness_threshold_days" {
                                    false
                                } else {
                                    affects_scan(path)
                                };
                                this.save_config(dirty, cx);
                            }
                        }
                    }
                },
            );
            self._subscriptions.push(sub);
            self.number_inputs.insert(path, input);
        }
        for &path in SLIDERS {
            let (min, max) = match path {
                "guard.warn_free_percent" => (2., 50.),
                "guard.critical_free_percent" => (1., 50.),
                "walker_threads" => (1., self.cores as f32),
                "max_concurrent_analyses" => (1., 32.),
                _ => (10., 100.),
            };
            let value = config_number(&self.ui.config, path) as f32;
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(min)
                    .max(max)
                    .step(1.)
                    .default_value(value.clamp(min, max))
            });
            let sub = cx.subscribe_in(&slider, window, move |this, _, ev: &SliderEvent, _, cx| {
                let (v, release) = match ev {
                    SliderEvent::Change(v) => (v.start(), false),
                    SliderEvent::Release(v) => (v.start(), true),
                };
                set_config_number(&mut this.ui.config, path, v as f64);
                if path == "guard.warn_free_percent" {
                    // Critical is never above the warning level.
                    let warn = this.ui.config.guard.warn_free_percent;
                    if this.ui.config.guard.critical_free_percent > warn {
                        this.ui.config.guard.critical_free_percent = warn;
                    }
                }
                if release {
                    this.save_config(false, cx);
                } else {
                    cx.notify();
                }
            });
            self._subscriptions.push(sub);
            self.sliders.insert(path, slider);
        }
    }

    /// Push config values back into the controls (after Restore defaults).
    pub(crate) fn sync_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (&path, input) in &self.number_inputs {
            let v = config_number(&self.ui.config, path);
            input.update(cx, |s, cx| s.set_value(format!("{v}"), window, cx));
        }
        for (&path, slider) in &self.sliders {
            let v = config_number(&self.ui.config, path) as f32;
            slider.update(cx, |s, cx| s.set_value(v, window, cx));
        }
    }

    pub(crate) fn settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.ui.settings_section = section;
        cx.notify();
    }
}
