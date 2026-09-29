//! The UI's state and every rule that derives something from it.
//!
//! Screens read it; scan and clean events and user input write it. Nothing
//! here knows about GPUI, so the rules are tested without a window.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use deepclean_core::config::{AppConfig, Grouping};
use deepclean_core::disk::DiskUsage;
use deepclean_core::model::{
    ActionEvent, ActionMethod, ArtifactKind, CleanAction, CleanableItem, Ecosystem, RiskLevel,
    ScanEvent,
};
use uuid::Uuid;

use super::actions::{
    effective_risk, is_actionable, is_recoverable, method_class, risk_rank, selected_action,
    MethodClass,
};
use super::labels::{eco_name, is_ai, kind_label};
use super::selection::{innermost_first, outermost, total_bytes_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Results,
    Agents,
    History,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Size,
    Stale,
    Name,
    Risk,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Size => Self::Stale,
            Self::Stale => Self::Name,
            Self::Name => Self::Risk,
            Self::Risk => Self::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "Size",
            Self::Stale => "Last modified",
            Self::Name => "Name",
            Self::Risk => "Risk",
        }
    }
}

pub fn next_grouping(g: Grouping) -> Grouping {
    match g {
        Grouping::Ecosystem => Grouping::Project,
        Grouping::Project => Grouping::Risk,
        Grouping::Risk => Grouping::Ecosystem,
    }
}

pub fn grouping_label(g: Grouping) -> &'static str {
    match g {
        Grouping::Ecosystem => "Ecosystem",
        Grouping::Project => "Project",
        Grouping::Risk => "Risk",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScannerStatus {
    Pending,
    Running,
    Done(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresetId {
    IdleWorktrees,
    OldTranscripts,
    DuplicateModels,
}

impl PresetId {
    pub const ALL: [PresetId; 3] = [
        Self::IdleWorktrees,
        Self::OldTranscripts,
        Self::DuplicateModels,
    ];
}

/// A quick filter from the Agents screen or the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub id: PresetId,
    pub label: &'static str,
    pub kinds: Vec<ArtifactKind>,
    pub min_days: Option<u64>,
}

/// Thresholds come from the AI settings, so "idle" means what the user said.
pub fn preset_for(id: PresetId, config: &AppConfig) -> Preset {
    match id {
        PresetId::IdleWorktrees => Preset {
            id,
            label: "Idle worktrees",
            kinds: vec![ArtifactKind::AgentWorktree],
            min_days: Some(config.ai.worktree_idle_days),
        },
        PresetId::OldTranscripts => Preset {
            id,
            label: "Old transcripts",
            kinds: vec![ArtifactKind::AgentTranscripts],
            min_days: Some(config.ai.agent_data_retention_days),
        },
        PresetId::DuplicateModels => Preset {
            id,
            label: "Duplicate models",
            kinds: vec![ArtifactKind::DuplicateModelFiles],
            min_days: None,
        },
    }
}

pub fn matches_preset(item: &CleanableItem, preset: Option<&Preset>) -> bool {
    let Some(p) = preset else { return true };
    if !p.kinds.is_empty() && !p.kinds.contains(&item.kind) {
        return false;
    }
    match p.min_days {
        Some(min) => item.days_stale.is_some_and(|d| d >= min),
        None => true,
    }
}

/// Per-row progress while a batch runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    Queued,
    Active,
    Done,
    Failed { error: String, can_fallback: bool },
}

#[derive(Debug, Clone)]
pub struct CleanProgress {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub bytes: u64,
    pub current: Option<String>,
    pub states: HashMap<Uuid, RowState>,
    pub started_at: Instant,
}

impl CleanProgress {
    pub fn done(&self) -> usize {
        self.completed + self.failed
    }

    pub fn percent(&self) -> u32 {
        if self.total == 0 {
            0
        } else {
            ((self.done() as f64 / self.total as f64) * 100.0).round() as u32
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub bytes_freed: u64,
    pub estimated: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

/// One results group, items already sorted.
pub struct Group<'a> {
    pub key: String,
    pub label: String,
    pub eco: Ecosystem,
    pub items: Vec<&'a CleanableItem>,
    pub bytes: u64,
    /// This group's bytes relative to the largest group, 0..=1.
    pub share: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionSummary {
    pub count: usize,
    pub bytes: u64,
    pub nested: usize,
    pub hidden: usize,
}

/// Everything the confirmation modal needs, derived from the selection.
pub struct CleanPlan<'a> {
    /// Innermost first.
    pub entries: Vec<(&'a CleanableItem, &'a CleanAction)>,
    pub effects: Vec<(MethodClass, usize, u64)>,
    pub trashed: usize,
    pub trashed_bytes: u64,
    pub estimated: u64,
    pub has_danger: bool,
    /// Non-safe entries, danger first. Safe items are counted, never listed:
    /// listing fifteen safe node_modules deletions trains people to scroll
    /// past the dialog.
    pub attention: Vec<(&'a CleanableItem, &'a CleanAction)>,
}

impl CleanPlan<'_> {
    pub fn selections(&self) -> Vec<(Uuid, Uuid)> {
        self.entries.iter().map(|(i, a)| (i.id, a.id)).collect()
    }
}

/// What an exclusion removed, so the toast's Undo can put it back.
#[derive(Debug, Clone)]
pub struct Excluded {
    pub item: CleanableItem,
    pub blocked_before: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct UiState {
    pub route: Route,
    pub items: Vec<CleanableItem>,
    /// item id → action id, when the user overrides the default.
    pub overrides: HashMap<Uuid, Uuid>,
    /// Independent of render state and of filtering.
    pub selected: HashSet<Uuid>,

    pub scanning: bool,
    pub has_scanned: bool,
    pub paths_scanned: u64,
    pub scan_duration_ms: u64,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub scanner_status: Vec<(Ecosystem, ScannerStatus)>,
    /// Deduplicated scan problems, in first-seen order: message → count.
    pub issues: Vec<(String, usize)>,
    pub denied_paths: BTreeSet<String>,

    pub filter: String,
    pub large_only: bool,
    pub preset: Option<Preset>,
    pub sort: SortKey,
    pub collapsed: HashSet<String>,

    pub focused: Option<Uuid>,
    pub drawer: Option<Uuid>,
    pub show_confirm: bool,
    pub show_issues: bool,
    pub open_run: Option<Uuid>,
    pub settings_section: SettingsSection,
    pub settings_dirty: bool,

    pub config: AppConfig,
    pub disk: Option<DiskUsage>,

    pub clean: Option<CleanProgress>,
    pub last_result: Option<CleanResult>,
    /// Rows that failed in the last batch. Kept after it ends so the error
    /// and the “Remove directory instead” offer stay on screen.
    pub failures: HashMap<Uuid, RowState>,
    /// The confirmation dialog is for this one item (the drawer's “Clean
    /// this item”) rather than the selection.
    pub confirm_only: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    #[default]
    Scanning,
    Ecosystems,
    Ai,
    Guard,
    Safety,
    Performance,
    General,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 8] = [
        Self::Scanning,
        Self::Ecosystems,
        Self::Ai,
        Self::Guard,
        Self::Safety,
        Self::Performance,
        Self::General,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Scanning => "Scanning",
            Self::Ecosystems => "Ecosystems",
            Self::Ai => "AI",
            Self::Guard => "Guard",
            Self::Safety => "Safety",
            Self::Performance => "Performance",
            Self::General => "General",
            Self::About => "About",
        }
    }
}

/// Informational threshold for the "large items" filter.
pub const LARGE_BYTES: u64 = 1_000_000_000;

impl UiState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            route: Route::Results,
            items: vec![],
            overrides: HashMap::new(),
            selected: HashSet::new(),
            scanning: false,
            has_scanned: false,
            paths_scanned: 0,
            scan_duration_ms: 0,
            last_scan_at: None,
            scanner_status: vec![],
            issues: vec![],
            denied_paths: BTreeSet::new(),
            filter: String::new(),
            large_only: false,
            preset: None,
            sort: SortKey::Size,
            collapsed: HashSet::new(),
            focused: None,
            drawer: None,
            show_confirm: false,
            show_issues: false,
            open_run: None,
            settings_section: SettingsSection::Scanning,
            settings_dirty: false,
            config,
            disk: None,
            clean: None,
            last_result: None,
            failures: HashMap::new(),
            confirm_only: None,
        }
    }

    pub fn prefer_trash(&self) -> bool {
        self.config.ui.prefer_trash
    }

    pub fn item(&self, id: Uuid) -> Option<&CleanableItem> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn action_for<'a>(&self, item: &'a CleanableItem) -> Option<&'a CleanAction> {
        selected_action(item, &self.overrides, self.prefer_trash())
    }

    pub fn risk_of(&self, item: &CleanableItem) -> RiskLevel {
        effective_risk(item, &self.overrides, self.prefer_trash())
    }

    pub fn is_stale(&self, item: &CleanableItem) -> bool {
        item.days_stale
            .is_some_and(|d| d > self.config.staleness_threshold_days)
    }

    // ── Filtering ───────────────────────────────────────────────────────

    /// Items surviving the current filters, in scan order.
    pub fn visible_items(&self) -> Vec<&CleanableItem> {
        let q = self.filter.trim().to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                if self.large_only && item.size_bytes < LARGE_BYTES {
                    return false;
                }
                if !matches_preset(item, self.preset.as_ref()) {
                    return false;
                }
                if !q.is_empty() {
                    let hay = format!(
                        "{} {} {}",
                        item.path.display(),
                        item.project_name.as_deref().unwrap_or(""),
                        super::labels::kind_id(item.kind)
                    )
                    .to_lowercase();
                    if !hay.contains(&q) {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    pub fn filters_active(&self) -> bool {
        self.large_only || self.preset.is_some()
    }

    pub fn active_filter_count(&self) -> usize {
        usize::from(self.large_only) + usize::from(self.preset.is_some())
    }

    pub fn reset_filters(&mut self) {
        self.large_only = false;
        self.preset = None;
        self.filter.clear();
    }

    pub fn apply_preset(&mut self, id: PresetId) {
        self.reset_filters();
        self.preset = Some(preset_for(id, &self.config));
        self.route = Route::Results;
        self.drawer = None;
    }

    pub fn ecosystems_present(&self) -> usize {
        self.items
            .iter()
            .map(|i| i.ecosystem)
            .collect::<HashSet<_>>()
            .len()
    }

    // ── Grouping ────────────────────────────────────────────────────────

    pub fn groups(&self) -> Vec<Group<'_>> {
        self.groups_of(self.visible_items())
    }

    fn groups_of<'a>(&'a self, items: Vec<&'a CleanableItem>) -> Vec<Group<'a>> {
        let mode = self.config.ui.grouping;
        let mut order: Vec<String> = vec![];
        let mut buckets: HashMap<String, Group<'a>> = HashMap::new();

        for item in items {
            let (key, label) = match mode {
                Grouping::Risk => {
                    let r = super::actions::risk_id(self.risk_of(item));
                    let mut label = r.to_string();
                    label[..1].make_ascii_uppercase();
                    (r.to_string(), label)
                }
                Grouping::Project => match &item.project_root {
                    Some(root) => (
                        root.display().to_string(),
                        root.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| root.display().to_string()),
                    ),
                    None => ("__global__".into(), "Global caches".into()),
                },
                Grouping::Ecosystem => (
                    super::labels::eco_id(item.ecosystem).to_string(),
                    eco_name(item.ecosystem).to_string(),
                ),
            };
            let g = buckets.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Group {
                    key,
                    label,
                    eco: item.ecosystem,
                    items: vec![],
                    bytes: 0,
                    share: 0.0,
                }
            });
            g.items.push(item);
            g.bytes += item.size_bytes;
        }

        let mut list: Vec<Group<'a>> = order
            .into_iter()
            .filter_map(|k| buckets.remove(&k))
            .collect();
        match mode {
            // Fixed severity order; Safe first because it is the bulk-select target.
            Grouping::Risk => list.sort_by_key(|g| match g.key.as_str() {
                "safe" => 0,
                "caution" => 1,
                "danger" => 2,
                _ => 3,
            }),
            // AI ecosystems lead; size orders the rest, and each half.
            Grouping::Ecosystem => list.sort_by(|a, b| {
                (!is_ai(a.eco))
                    .cmp(&!is_ai(b.eco))
                    .then(b.bytes.cmp(&a.bytes))
            }),
            Grouping::Project => list.sort_by_key(|g| std::cmp::Reverse(g.bytes)),
        }
        let max = list.iter().map(|g| g.bytes).max().unwrap_or(0).max(1);
        for g in &mut list {
            g.share = g.bytes as f32 / max as f32;
            self.sort_items(&mut g.items);
        }
        list
    }

    fn sort_items(&self, items: &mut [&CleanableItem]) {
        match self.sort {
            SortKey::Stale => items
                .sort_by_key(|i| std::cmp::Reverse(i.days_stale.map(|d| d as i64).unwrap_or(-1))),
            SortKey::Name => items.sort_by_key(|i| kind_label(i).to_lowercase()),
            SortKey::Risk => items.sort_by_key(|i| std::cmp::Reverse(risk_rank(self.risk_of(i)))),
            SortKey::Size => items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes)),
        }
    }

    /// Rows in display order, skipping collapsed groups: what arrow keys walk.
    pub fn items_in_order(&self) -> Vec<Uuid> {
        self.groups()
            .into_iter()
            .filter(|g| !self.collapsed.contains(&g.key))
            .flat_map(|g| g.items.into_iter().map(|i| i.id))
            .collect()
    }

    // ── Selection ───────────────────────────────────────────────────────

    pub fn selected_items(&self) -> Vec<&CleanableItem> {
        self.items
            .iter()
            .filter(|i| self.selected.contains(&i.id))
            .collect()
    }

    /// Bytes count nested selections once: a stale project and the
    /// `node_modules` inside it are one amount of space, not two.
    pub fn selection_summary(&self) -> SelectionSummary {
        let chosen = self.selected_items();
        let visible: HashSet<Uuid> = self.visible_items().iter().map(|i| i.id).collect();
        SelectionSummary {
            count: chosen.len(),
            bytes: total_bytes_of(&chosen),
            nested: chosen.len() - outermost(&chosen).len(),
            hidden: chosen.iter().filter(|i| !visible.contains(&i.id)).count(),
        }
    }

    /// Reclaimable total across every result, nested items counted once.
    /// Informational items are excluded — nothing Void can run frees them,
    /// and a VM disk *contains* the Docker data already listed.
    pub fn total_bytes(&self) -> u64 {
        let actionable: Vec<&CleanableItem> =
            self.items.iter().filter(|i| is_actionable(i)).collect();
        total_bytes_of(&actionable)
    }

    pub fn safe_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| is_actionable(i) && self.risk_of(i) == RiskLevel::Safe)
            .count()
    }

    pub fn toggle_select(&mut self, id: Uuid) {
        match self.item(id) {
            Some(item) if is_actionable(item) => {}
            _ => return,
        }
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    /// Tri-state of a group's checkbox: `Some(true)` all, `None` mixed,
    /// `Some(false)` none. Informational rows do not count.
    pub fn group_check_state(&self, group: &Group<'_>) -> Option<bool> {
        let selectable: Vec<_> = group.items.iter().filter(|i| is_actionable(i)).collect();
        let n = selectable
            .iter()
            .filter(|i| self.selected.contains(&i.id))
            .count();
        if !selectable.is_empty() && n == selectable.len() {
            Some(true)
        } else if n > 0 {
            None
        } else {
            Some(false)
        }
    }

    pub fn toggle_group(&mut self, key: &str) {
        let ids: Vec<Uuid> = match self.groups().into_iter().find(|g| g.key == key) {
            Some(g) => g
                .items
                .iter()
                .filter(|i| is_actionable(i))
                .map(|i| i.id)
                .collect(),
            None => return,
        };
        let all = ids.iter().all(|id| self.selected.contains(id));
        for id in ids {
            if all {
                self.selected.remove(&id);
            } else {
                self.selected.insert(id);
            }
        }
    }

    pub fn toggle_collapsed(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
    }

    /// ⌘⇧E: expand everything if anything is collapsed, else collapse all.
    pub fn toggle_expand_all(&mut self) {
        if self.collapsed.is_empty() {
            let keys: Vec<String> = self.groups().into_iter().map(|g| g.key).collect();
            self.collapsed.extend(keys);
        } else {
            self.collapsed.clear();
        }
    }

    pub fn select_safe(&mut self) {
        let ids: Vec<Uuid> = self
            .visible_items()
            .into_iter()
            .filter(|i| is_actionable(i) && self.risk_of(i) == RiskLevel::Safe)
            .map(|i| i.id)
            .collect();
        self.selected.extend(ids);
    }

    pub fn select_all_visible(&mut self) {
        let ids: Vec<Uuid> = self
            .visible_items()
            .into_iter()
            .filter(|i| is_actionable(i))
            .map(|i| i.id)
            .collect();
        self.selected.extend(ids);
    }

    pub fn choose_action(&mut self, item: Uuid, action: Uuid) {
        self.overrides.insert(item, action);
    }

    // ── Keyboard focus and drawer ───────────────────────────────────────

    pub fn move_focus(&mut self, delta: i32) {
        let order = self.items_in_order();
        if order.is_empty() {
            return;
        }
        let next = match self
            .focused
            .and_then(|f| order.iter().position(|id| *id == f))
        {
            Some(i) => (i as i64 + delta as i64).clamp(0, order.len() as i64 - 1) as usize,
            None => 0,
        };
        self.focused = Some(order[next]);
    }

    pub fn move_drawer(&mut self, delta: i32) {
        let order = self.items_in_order();
        let Some(i) = self
            .drawer
            .and_then(|d| order.iter().position(|id| *id == d))
        else {
            return;
        };
        let j = i as i64 + delta as i64;
        if j >= 0 && (j as usize) < order.len() {
            self.drawer = Some(order[j as usize]);
        }
    }

    /// Esc closes the innermost thing that is open. Returns what it did so
    /// the view can clear a focused text field when nothing else applied.
    pub fn escape(&mut self) -> bool {
        if self.show_confirm {
            self.show_confirm = false;
            self.confirm_only = None;
        } else if self.show_issues {
            self.show_issues = false;
        } else if self.open_run.is_some() {
            self.open_run = None;
        } else if self.drawer.is_some() {
            self.drawer = None;
        } else if !self.selected.is_empty() {
            self.selected.clear();
        } else {
            return false;
        }
        true
    }

    // ── Excluding ───────────────────────────────────────────────────────

    /// Add the item's path to the blocklist and drop it from the results.
    pub fn exclude(&mut self, id: Uuid) -> Option<Excluded> {
        let pos = self.items.iter().position(|i| i.id == id)?;
        let before = self.config.blocked_paths.clone();
        let item = self.items.remove(pos);
        self.config.blocked_paths.push(item.path.clone());
        self.selected.remove(&id);
        self.drawer = None;
        Some(Excluded {
            item,
            blocked_before: before,
        })
    }

    pub fn undo_exclude(&mut self, excluded: Excluded) {
        self.config.blocked_paths = excluded.blocked_before;
        self.items.push(excluded.item);
    }

    // ── Clean plan ──────────────────────────────────────────────────────

    pub fn build_plan(&self) -> CleanPlan<'_> {
        let mut entries: Vec<(&CleanableItem, &CleanAction)> = self
            .items
            .iter()
            .filter(|i| match self.confirm_only {
                Some(only) => i.id == only,
                None => self.selected.contains(&i.id),
            })
            .filter_map(|i| self.action_for(i).map(|a| (i, a)))
            .collect();
        entries = innermost_first(entries, |(i, _)| i.path.as_path());
        self.plan_for(entries)
    }

    pub fn plan_for<'a>(
        &'a self,
        entries: Vec<(&'a CleanableItem, &'a CleanAction)>,
    ) -> CleanPlan<'a> {
        // Bytes count only the outermost selected item — an inner item's
        // space is already inside its container's size.
        let items: Vec<&CleanableItem> = entries.iter().map(|(i, _)| *i).collect();
        let counted: HashSet<Uuid> = outermost(&items).iter().map(|i| i.id).collect();
        let size_of = |i: &CleanableItem| {
            if counted.contains(&i.id) {
                i.size_bytes
            } else {
                0
            }
        };

        let classes = [
            MethodClass::Directories,
            MethodClass::Files,
            MethodClass::Commands,
            MethodClass::Dedup,
        ];
        let mut effects: Vec<(MethodClass, usize, u64)> =
            classes.iter().map(|c| (*c, 0, 0)).collect();
        let (mut trashed, mut trashed_bytes, mut estimated, mut has_danger) = (0, 0, 0, false);
        for (item, action) in &entries {
            if is_recoverable(action) {
                trashed += 1;
                trashed_bytes += size_of(item);
            } else {
                let k = method_class(&action.method);
                let k = if k == MethodClass::Trash {
                    MethodClass::Commands
                } else {
                    k
                };
                if let Some(e) = effects.iter_mut().find(|e| e.0 == k) {
                    e.1 += 1;
                    e.2 += size_of(item);
                }
            }
            estimated += size_of(item);
            has_danger |= action.risk == RiskLevel::Danger;
        }
        effects.retain(|e| e.1 > 0);

        let mut attention: Vec<(&CleanableItem, &CleanAction)> = entries
            .iter()
            .filter(|(_, a)| a.risk != RiskLevel::Safe)
            .copied()
            .collect();
        attention.sort_by_key(|(_, a)| std::cmp::Reverse(risk_rank(a.risk)));

        CleanPlan {
            entries,
            effects,
            trashed,
            trashed_bytes,
            estimated,
            has_danger,
            attention,
        }
    }

    // ── Scan events ─────────────────────────────────────────────────────

    pub fn begin_scan(&mut self) {
        self.scanning = true;
        self.has_scanned = true;
        self.items.clear();
        self.selected.clear();
        self.overrides.clear();
        self.issues.clear();
        self.denied_paths.clear();
        self.paths_scanned = 0;
        self.last_result = None;
        self.clean = None;
        self.failures.clear();
        self.confirm_only = None;
        self.scanner_status = self
            .config
            .enabled_ecosystems
            .iter()
            .map(|e| (*e, ScannerStatus::Pending))
            .collect();
        self.settings_dirty = false;
    }

    pub fn record_issue(&mut self, message: String) {
        match self.issues.iter_mut().find(|(m, _)| *m == message) {
            Some((_, n)) => *n += 1,
            None => self.issues.push((message, 1)),
        }
    }

    pub fn issue_count(&self) -> usize {
        self.issues.iter().map(|(_, n)| n).sum()
    }

    fn set_status(&mut self, eco: Ecosystem, status: ScannerStatus) {
        match self.scanner_status.iter_mut().find(|(e, _)| *e == eco) {
            Some(entry) => entry.1 = status,
            None => self.scanner_status.push((eco, status)),
        }
    }

    pub fn apply_scan_event(&mut self, ev: ScanEvent) {
        match ev {
            ScanEvent::ScannerStarted { ecosystem } => {
                self.set_status(ecosystem, ScannerStatus::Running)
            }
            ScanEvent::ScannerCompleted {
                ecosystem,
                items_found,
            } => self.set_status(ecosystem, ScannerStatus::Done(items_found)),
            ScanEvent::Progress { paths_scanned, .. } => self.paths_scanned = paths_scanned,
            ScanEvent::ItemFound { item } => self.items.push(item),
            ScanEvent::PermissionDenied { path } => {
                self.denied_paths.insert(path.display().to_string());
            }
            ScanEvent::Error { message } => self.record_issue(message),
            ScanEvent::ScanComplete { summary } => {
                self.finish_scan();
                self.scan_duration_ms = summary.scan_duration_ms;
            }
        }
    }

    /// The scan ended (normally or because its channel closed).
    pub fn finish_scan(&mut self) {
        if !self.scanning {
            return;
        }
        self.scanning = false;
        self.last_scan_at = Some(Utc::now());
        let counts: Vec<(Ecosystem, usize)> = self
            .scanner_status
            .iter()
            .filter(|(_, s)| *s == ScannerStatus::Running)
            .map(|(e, _)| (*e, self.items.iter().filter(|i| i.ecosystem == *e).count()))
            .collect();
        for (e, n) in counts {
            self.set_status(e, ScannerStatus::Done(n));
        }
    }

    // ── Clean events ────────────────────────────────────────────────────

    pub fn begin_clean(&mut self, selections: &[(Uuid, Uuid)]) {
        self.show_confirm = false;
        self.confirm_only = None;
        self.failures.clear();
        self.clean = Some(CleanProgress {
            total: selections.len(),
            completed: 0,
            failed: 0,
            bytes: 0,
            current: None,
            states: selections
                .iter()
                .map(|(i, _)| (*i, RowState::Queued))
                .collect(),
            started_at: Instant::now(),
        });
    }

    pub fn apply_action_event(&mut self, ev: ActionEvent) {
        let Some(c) = self.clean.as_mut() else { return };
        match ev {
            ActionEvent::Started { item_id, label, .. } => {
                c.current = Some(label);
                if let Some(s) = c.states.get_mut(&item_id) {
                    *s = RowState::Active;
                }
            }
            ActionEvent::Completed {
                item_id,
                bytes_freed,
                estimated_bytes,
                ..
            } => {
                c.completed += 1;
                c.bytes += if bytes_freed > 0 {
                    bytes_freed
                } else {
                    estimated_bytes
                };
                c.states.insert(item_id, RowState::Done);
                // Drop cleaned items so the list reflects reality without a rescan.
                self.items.retain(|i| i.id != item_id);
                self.selected.remove(&item_id);
            }
            ActionEvent::Failed { item_id, error, .. } => {
                c.failed += 1;
                // The single most likely real failure: a GUI-launched app has
                // a bare PATH, so `cargo`/`npm`/`go` may not resolve. Offer
                // the direct filesystem action instead.
                let can_fallback = error.contains("was not found in PATH")
                    && self
                        .items
                        .iter()
                        .find(|i| i.id == item_id)
                        .is_some_and(|i| fallback_action(i).is_some());
                c.states.insert(
                    item_id,
                    RowState::Failed {
                        error,
                        can_fallback,
                    },
                );
            }
            ActionEvent::BatchComplete {
                total,
                succeeded,
                failed,
                bytes_freed,
                estimated,
                cancelled,
            } => {
                self.last_result = Some(CleanResult {
                    total,
                    succeeded,
                    failed,
                    bytes_freed,
                    estimated,
                    cancelled,
                    duration_ms: c.started_at.elapsed().as_millis() as u64,
                });
                self.failures = c
                    .states
                    .drain()
                    .filter(|(_, st)| matches!(st, RowState::Failed { .. }))
                    .collect();
                self.clean = None;
            }
        }
    }

    pub fn row_state(&self, id: Uuid) -> Option<&RowState> {
        match &self.clean {
            Some(c) => c.states.get(&id),
            None => self.failures.get(&id),
        }
    }

    /// The action "Clean this item" would run, when it may run without the
    /// confirmation dialog: Safe only. Anything riskier goes through review,
    /// where Danger needs `delete` typed.
    pub fn needs_review(&self, id: Uuid) -> bool {
        self.item(id)
            .and_then(|i| self.action_for(i))
            .is_some_and(|a| a.risk != RiskLevel::Safe)
    }

    /// Guard's automatic cleanup removed this path; drop it so Results match disk.
    pub fn remove_path(&mut self, path: &Path) {
        let gone: Vec<Uuid> = self
            .items
            .iter()
            .filter(|i| i.path == path)
            .map(|i| i.id)
            .collect();
        self.items.retain(|i| i.path != path);
        for id in gone {
            self.selected.remove(&id);
        }
    }

    /// Worktrees idle for at least the configured days, once a scan has run.
    pub fn idle_worktrees(&self) -> Option<usize> {
        self.has_scanned.then(|| {
            self.items
                .iter()
                .filter(|i| i.kind == ArtifactKind::AgentWorktree)
                .filter(|i| {
                    i.days_stale
                        .is_some_and(|d| d >= self.config.ai.worktree_idle_days)
                })
                .count()
        })
    }
}

/// The plain directory delete offered when a tool is missing from PATH.
pub fn fallback_action(item: &CleanableItem) -> Option<&CleanAction> {
    item.available_actions
        .iter()
        .find(|a| matches!(a.method, ActionMethod::RemoveDir { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::*;
    use deepclean_core::model::ScanSummary;

    fn state() -> UiState {
        let mut config = AppConfig {
            staleness_threshold_days: 30,
            ..Default::default()
        };
        config.ai.worktree_idle_days = 3;
        config.ai.agent_data_retention_days = 30;
        let mut s = UiState::new(config);
        s.has_scanned = true;
        s
    }

    fn with_kind(mut i: CleanableItem, eco: Ecosystem, kind: ArtifactKind) -> CleanableItem {
        i.ecosystem = eco;
        i.kind = kind;
        i
    }

    #[test]
    fn selection_totals_count_a_project_and_its_node_modules_once() {
        let mut s = state();
        let project = with_kind(
            cleanable("/u/proj", 5000),
            Ecosystem::Projects,
            ArtifactKind::StaleProject,
        );
        let nm = cleanable("/u/proj/node_modules", 3000);
        let lone = cleanable("/u/other/node_modules", 700);
        s.items = vec![project.clone(), nm.clone(), lone.clone()];

        s.selected.insert(nm.id);
        assert_eq!(s.selection_summary().bytes, 3000);

        s.selected.insert(project.id);
        s.selected.insert(lone.id);
        let sum = s.selection_summary();
        assert_eq!((sum.count, sum.bytes, sum.nested), (3, 5700, 1));
        assert_eq!(s.total_bytes(), 5700);
    }

    #[test]
    fn reclaimable_total_skips_informational_items_such_as_a_docker_vm_disk() {
        let mut s = state();
        let docker = with_kind(
            cleanable("/var/run/docker.sock", 220),
            Ecosystem::Docker,
            ArtifactKind::DockerData,
        );
        let mut vm = with_kind(
            cleanable(
                "/u/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
                220,
            ),
            Ecosystem::Docker,
            ArtifactKind::ContainerVmDisk,
        );
        vm.available_actions.clear();
        s.items = vec![docker, vm, cleanable("/u/app/node_modules", 5)];
        assert_eq!(s.total_bytes(), 225);
    }

    #[test]
    fn selection_ignores_ids_no_longer_in_the_results_and_counts_hidden_ones() {
        let mut s = state();
        let a = cleanable("/u/rust/target", 10);
        let b = cleanable("/u/web/node_modules", 20);
        s.items = vec![a.clone(), b.clone()];
        s.selected.extend([a.id, b.id, Uuid::new_v4()]);
        s.filter = "web".into();
        let sum = s.selection_summary();
        assert_eq!((sum.count, sum.bytes, sum.hidden), (2, 30, 1));
    }

    #[test]
    fn idle_worktree_preset_uses_the_configured_idle_days() {
        let mut s = state();
        let preset = preset_for(PresetId::IdleWorktrees, &s.config);
        assert_eq!(preset.min_days, Some(3));
        let wt = |days: Option<u64>| {
            let mut i = with_kind(
                cleanable("/u/wt", 1),
                Ecosystem::Worktrees,
                ArtifactKind::AgentWorktree,
            );
            i.days_stale = days;
            i
        };
        let (idle, fresh, unknown) = (wt(Some(5)), wt(Some(1)), wt(None));
        let mut other = cleanable("/u/nm", 1);
        other.days_stale = Some(50);
        assert!(matches_preset(&idle, Some(&preset)));
        assert!(!matches_preset(&fresh, Some(&preset)));
        assert!(!matches_preset(&unknown, Some(&preset)));
        assert!(!matches_preset(&other, Some(&preset)));

        s.items = vec![idle.clone(), fresh, other];
        s.apply_preset(PresetId::IdleWorktrees);
        assert!(s.filters_active());
        assert_eq!(
            s.visible_items().iter().map(|i| i.id).collect::<Vec<_>>(),
            [idle.id]
        );
        s.reset_filters();
        assert!(s.preset.is_none());
        assert_eq!(s.visible_items().len(), 3);
        assert_eq!(s.idle_worktrees(), Some(1));
    }

    #[test]
    fn other_presets() {
        let c = AppConfig::default();
        assert_eq!(
            preset_for(PresetId::OldTranscripts, &c).kinds,
            [ArtifactKind::AgentTranscripts]
        );
        assert_eq!(preset_for(PresetId::DuplicateModels, &c).min_days, None);
    }

    /// The fixture from the old screens test: a worktree, the node_modules
    /// inside it, and a locked (informational) worktree.
    fn worktree_fixture() -> (UiState, CleanableItem, CleanableItem, CleanableItem) {
        let mut s = state();
        let mut wt = with_kind(
            cleanable("/u/repo/.claude/worktrees/fix", 9000),
            Ecosystem::Worktrees,
            ArtifactKind::AgentWorktree,
        );
        wt.agent = Some("claude".into());
        wt.days_stale = Some(6);
        wt.details = vec![
            detail("Branch", "worktree-fix"),
            detail("Uncommitted changes", "2 files"),
        ];
        wt.available_actions = vec![action(
            ActionMethod::RemoveDirs {
                paths: vec!["/u/repo/.claude/worktrees/fix/node_modules".into()],
            },
            RiskLevel::Safe,
            4000,
        )];
        let mut nm = cleanable("/u/repo/.claude/worktrees/fix/node_modules", 4000);
        nm.available_actions = delete_and_trash(
            "/u/repo/.claude/worktrees/fix/node_modules",
            RiskLevel::Safe,
            1000,
        );
        let mut locked = with_kind(
            cleanable("/u/repo/.claude/worktrees/locked", 100),
            Ecosystem::Worktrees,
            ArtifactKind::AgentWorktree,
        );
        locked.details = vec![detail("Locked", "yes")];
        locked.available_actions.clear();
        s.items = vec![nm.clone(), wt.clone(), locked.clone()];
        (s, wt, nm, locked)
    }

    #[test]
    fn ai_ecosystems_lead_and_informational_rows_cannot_be_selected() {
        let (mut s, _, _, locked) = worktree_fixture();
        let groups = s.groups();
        assert_eq!(groups[0].label, "Agent worktrees");
        assert_eq!(groups[1].label, "Node.js");
        s.toggle_select(locked.id);
        assert!(s.selected.is_empty());
    }

    #[test]
    fn build_plan_runs_nested_items_innermost_first_and_counts_bytes_once() {
        let (mut s, wt, nm, _) = worktree_fixture();
        s.selected.extend([wt.id, nm.id]);
        let plan = s.build_plan();
        assert_eq!(
            plan.entries.iter().map(|(i, _)| i.id).collect::<Vec<_>>(),
            [nm.id, wt.id]
        );
        assert_eq!(plan.estimated, 9000);
        assert!(!plan.has_danger);
        assert!(plan.attention.is_empty());
    }

    #[test]
    fn plan_lists_non_safe_actions_danger_first_and_groups_effects() {
        let mut s = state();
        let mut caution = cleanable("/u/a", 10);
        caution.available_actions[0].risk = RiskLevel::Caution;
        let mut danger = cleanable("/u/b", 20);
        danger.available_actions[0].risk = RiskLevel::Danger;
        let mut cmd = cleanable("/u/c", 30);
        cmd.available_actions = vec![action(
            command("npm", &["cache", "clean"]),
            RiskLevel::Safe,
            30,
        )];
        let mut t = with_kind(
            cleanable("/u/d", 40),
            Ecosystem::Projects,
            ArtifactKind::StaleProject,
        );
        t.available_actions = vec![action(trash("/u/d"), RiskLevel::Safe, 40)];
        s.items = vec![caution.clone(), danger.clone(), cmd, t];
        s.selected
            .extend(s.items.iter().map(|i| i.id).collect::<Vec<_>>());
        let plan = s.build_plan();
        assert!(plan.has_danger);
        assert_eq!(plan.attention[0].0.id, danger.id);
        assert_eq!(plan.attention[1].0.id, caution.id);
        assert_eq!(plan.trashed, 1);
        assert_eq!(plan.trashed_bytes, 40);
        assert_eq!(
            plan.effects,
            vec![
                (MethodClass::Directories, 2, 30),
                (MethodClass::Commands, 1, 30)
            ]
        );
        assert_eq!(plan.estimated, 100);
    }

    #[test]
    fn grouping_by_risk_and_project() {
        let (mut s, _, _, _) = worktree_fixture();
        s.config.ui.grouping = Grouping::Risk;
        assert_eq!(
            s.groups()
                .iter()
                .map(|g| g.label.as_str())
                .collect::<Vec<_>>(),
            ["Safe"]
        );
        s.config.ui.grouping = Grouping::Project;
        assert_eq!(s.groups()[0].label, "Global caches");
    }

    #[test]
    fn sorting_and_collapsing_drive_keyboard_order() {
        let mut s = state();
        let small = cleanable("/u/small", 1);
        let big = cleanable("/u/big", 100);
        s.items = vec![small.clone(), big.clone()];
        assert_eq!(s.items_in_order(), [big.id, small.id]);
        s.sort = SortKey::Name;
        s.move_focus(1);
        assert_eq!(s.focused, Some(s.items_in_order()[0]));
        s.move_focus(1);
        assert_eq!(s.focused, Some(s.items_in_order()[1]));
        s.move_focus(5);
        assert_eq!(
            s.focused,
            Some(s.items_in_order()[1]),
            "focus clamps at the end"
        );
        s.toggle_expand_all();
        assert!(s.items_in_order().is_empty());
        s.toggle_expand_all();
        assert_eq!(s.items_in_order().len(), 2);
    }

    #[test]
    fn group_checkbox_is_tri_state_and_toggles_selectable_rows() {
        let (mut s, wt, nm, _) = worktree_fixture();
        let key = s.groups()[0].key.clone();
        assert_eq!(s.group_check_state(&s.groups()[0]), Some(false));
        s.toggle_group(&key);
        assert!(s.selected.contains(&wt.id));
        assert_eq!(s.group_check_state(&s.groups()[0]), Some(true));
        s.selected.insert(nm.id);
        s.toggle_group(&key);
        assert!(!s.selected.contains(&wt.id));
        s.selected.clear();
        s.select_safe();
        assert_eq!(s.selected.len(), 2);
    }

    #[test]
    fn scan_events_stream_into_state() {
        let mut s = state();
        s.has_scanned = false;
        s.config.enabled_ecosystems = vec![Ecosystem::Node, Ecosystem::Rust];
        s.begin_scan();
        assert!(s.scanning && s.has_scanned);
        s.apply_scan_event(ScanEvent::ScannerStarted {
            ecosystem: Ecosystem::Node,
        });
        s.apply_scan_event(ScanEvent::ItemFound {
            item: cleanable("/u/a", 1),
        });
        s.apply_scan_event(ScanEvent::Progress {
            message: String::new(),
            paths_scanned: 42,
        });
        s.apply_scan_event(ScanEvent::PermissionDenied {
            path: "/u/locked".into(),
        });
        s.apply_scan_event(ScanEvent::Error {
            message: "boom".into(),
        });
        s.apply_scan_event(ScanEvent::Error {
            message: "boom".into(),
        });
        assert_eq!(
            s.scanner_status[0],
            (Ecosystem::Node, ScannerStatus::Running)
        );
        s.apply_scan_event(ScanEvent::ScanComplete {
            summary: ScanSummary {
                scan_duration_ms: 1234,
                ..Default::default()
            },
        });
        assert!(!s.scanning);
        assert_eq!(s.scan_duration_ms, 1234);
        assert_eq!(s.paths_scanned, 42);
        assert_eq!(s.issue_count(), 2);
        assert_eq!(s.issues.len(), 1);
        assert_eq!(s.denied_paths.len(), 1);
        assert_eq!(
            s.scanner_status[0],
            (Ecosystem::Node, ScannerStatus::Done(1))
        );
        assert_eq!(
            s.scanner_status[1],
            (Ecosystem::Rust, ScannerStatus::Pending)
        );
    }

    #[test]
    fn action_events_track_progress_and_drop_cleaned_items() {
        let mut s = state();
        let a = cleanable("/u/a", 100);
        let b = cleanable("/u/b", 200);
        s.items = vec![a.clone(), b.clone()];
        s.selected.extend([a.id, b.id]);
        s.selected.insert(a.id);
        let sels = s.build_plan().selections();
        s.begin_clean(&sels);
        s.apply_action_event(ActionEvent::Started {
            item_id: a.id,
            action_id: a.available_actions[0].id,
            label: "Remove".into(),
        });
        assert_eq!(s.row_state(a.id), Some(&RowState::Active));
        s.apply_action_event(ActionEvent::Completed {
            item_id: a.id,
            action_id: a.available_actions[0].id,
            bytes_freed: 0,
            estimated_bytes: 100,
        });
        assert!(s.item(a.id).is_none());
        assert!(!s.selected.contains(&a.id));
        assert_eq!(s.clean.as_ref().unwrap().bytes, 100);
        assert_eq!(s.clean.as_ref().unwrap().percent(), 50);
        s.apply_action_event(ActionEvent::BatchComplete {
            total: 2,
            succeeded: 1,
            failed: 1,
            bytes_freed: 100,
            estimated: true,
            cancelled: false,
        });
        assert!(s.clean.is_none());
        assert_eq!(s.last_result.as_ref().unwrap().succeeded, 1);
    }

    #[test]
    fn action_event_failure_offers_fallback_when_a_tool_is_missing() {
        let mut s = state();
        let mut i = cleanable("/u/proj/target", 100);
        i.available_actions
            .insert(0, action(command("cargo", &["clean"]), RiskLevel::Safe, 0));
        s.items = vec![i.clone()];
        s.begin_clean(&[(i.id, i.available_actions[0].id)]);
        s.apply_action_event(ActionEvent::Failed {
            item_id: i.id,
            action_id: i.available_actions[0].id,
            error: "cargo was not found in PATH".into(),
        });
        assert_eq!(
            s.row_state(i.id),
            Some(&RowState::Failed {
                error: "cargo was not found in PATH".into(),
                can_fallback: true
            })
        );
        assert!(fallback_action(&i).is_some());

        // The offer outlives the batch: a one-item clean's failure and its
        // BatchComplete usually arrive together.
        s.apply_action_event(ActionEvent::BatchComplete {
            total: 1,
            succeeded: 0,
            failed: 1,
            bytes_freed: 0,
            estimated: false,
            cancelled: false,
        });
        assert!(s.clean.is_none());
        assert!(matches!(
            s.row_state(i.id),
            Some(RowState::Failed {
                can_fallback: true,
                ..
            })
        ));
        s.begin_scan();
        assert!(s.row_state(i.id).is_none());
    }

    #[test]
    fn a_single_item_plan_ignores_the_selection_and_flags_danger() {
        let mut s = state();
        let a = cleanable("/u/a", 10);
        let mut d = cleanable("/u/d", 20);
        d.available_actions[0].risk = RiskLevel::Danger;
        s.items = vec![a.clone(), d.clone()];
        s.selected.insert(a.id);
        assert!(!s.needs_review(a.id));
        assert!(s.needs_review(d.id));
        s.confirm_only = Some(d.id);
        let plan = s.build_plan();
        assert_eq!(plan.entries.len(), 1);
        assert!(plan.has_danger);
    }

    #[test]
    fn exclude_and_undo() {
        let mut s = state();
        let a = cleanable("/u/a", 1);
        s.items = vec![a.clone()];
        s.selected.insert(a.id);
        let ex = s.exclude(a.id).unwrap();
        assert!(s.items.is_empty() && s.selected.is_empty());
        assert_eq!(s.config.blocked_paths, [PathBuf::from("/u/a")]);
        s.undo_exclude(ex);
        assert_eq!(s.items.len(), 1);
        assert!(s.config.blocked_paths.is_empty());
    }

    #[test]
    fn escape_closes_the_innermost_thing() {
        let mut s = state();
        let a = cleanable("/u/a", 1);
        s.items = vec![a.clone()];
        s.selected.insert(a.id);
        s.drawer = Some(a.id);
        s.show_confirm = true;
        assert!(s.escape() && !s.show_confirm && s.drawer.is_some());
        assert!(s.escape() && s.drawer.is_none());
        assert!(s.escape() && s.selected.is_empty());
        assert!(!s.escape());
    }

    #[test]
    fn many_items_group_and_filter_under_budget() {
        let mut s = state();
        s.items = (0..10_000)
            .map(|i| cleanable(&format!("/u/p{i}/node_modules"), i))
            .collect();
        s.filter = "p9".into();
        let t = std::time::Instant::now();
        let groups = s.groups();
        let _ = s.selection_summary();
        assert!(!groups.is_empty());
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
    }
}
