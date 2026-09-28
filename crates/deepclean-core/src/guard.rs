//! Guard mode: decide when free space is low, and which items the user's
//! opt-in policies would clean.
//!
//! Pure logic only — no timers, no notifications. The desktop app runs the
//! loop and the CLI exposes `void guard check`; both call into here so the
//! rules are tested once.

use std::path::Path;

use bytesize::ByteSize;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{GuardConfig, Policy, PolicyAction};
use crate::disk::DiskUsage;
use crate::model::{ActionMethod, CleanAction, CleanableItem, RiskLevel};

/// How worried to be about free space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskState {
    Ok,
    Low,
    Critical,
}

/// The guard's verdict on a volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardStatus {
    pub state: DiskState,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub free_percent: f64,
    /// One sentence for a notification or the tray tooltip.
    pub message: String,
}

/// Classify `usage` against the configured thresholds.
///
/// A volume reporting zero capacity (a sandbox with no enumerable mounts) is
/// `Ok`: warning about a disk we cannot measure would be noise.
pub fn evaluate(usage: &DiskUsage, config: &GuardConfig) -> GuardStatus {
    let free = usage.available_bytes;
    let total = usage.total_bytes;
    if total == 0 {
        return GuardStatus {
            state: DiskState::Ok,
            free_bytes: free,
            total_bytes: 0,
            free_percent: 100.0,
            message: "Free space unknown".into(),
        };
    }

    let free_percent = (free.min(total) as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
    let state = if free_percent < f64::from(config.critical_free_percent) {
        DiskState::Critical
    } else if free_percent < f64::from(config.warn_free_percent) {
        DiskState::Low
    } else {
        DiskState::Ok
    };

    let amount = format!("{} free ({})", ByteSize(free), format_percent(free_percent));
    let message = match state {
        DiskState::Critical => {
            format!("Only {amount} — AI agents may start failing. Open Void to reclaim space.")
        }
        DiskState::Low => {
            format!("{amount} — disk space is running low. Open Void to reclaim space.")
        }
        DiskState::Ok => amount,
    };

    GuardStatus {
        state,
        free_bytes: free,
        total_bytes: total,
        free_percent,
        message,
    }
}

/// "3%", but "0.4%" below one percent so a nearly full disk never reads "0%".
fn format_percent(p: f64) -> String {
    if p > 0.0 && p < 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// Whether the background loop should check the disk again.
///
/// Always due when there has been no check yet; never due when the guard is
/// disabled. The interval is floored at one minute so a hand-edited `0`
/// cannot turn the loop into a busy spin.
pub fn next_check_due(
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    config: &GuardConfig,
) -> bool {
    if !config.enabled {
        return false;
    }
    let Some(last) = last else {
        return true;
    };
    let interval = config.check_interval_minutes.max(1);
    let interval = Duration::minutes(i64::try_from(interval).unwrap_or(i64::MAX / 60_000));
    now.signed_duration_since(last) >= interval
}

/// Whether moving from `prev` to `new` deserves a notification.
///
/// Only worsening transitions notify — recovering to `Ok` is good news the
/// user does not need interrupting for. A disk that *stays* critical is
/// re-announced at most once every `renotify_hours` (measured from
/// `last_notified`), so it is not forgotten but does not nag.
pub fn should_notify(
    prev: DiskState,
    new: DiskState,
    last_notified: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    renotify_hours: u64,
) -> bool {
    if new > prev {
        return true;
    }
    if prev == DiskState::Critical && new == DiskState::Critical {
        let Some(last) = last_notified else {
            return true;
        };
        let hours = i64::try_from(renotify_hours.max(1)).unwrap_or(i64::MAX / 3_600_000);
        return now.signed_duration_since(last) >= Duration::hours(hours);
    }
    false
}

/// One item a policy would clean, and the action it would run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySelection {
    pub policy_id: String,
    pub item: CleanableItem,
    pub action: CleanAction,
}

/// The highest risk Guard mode ever runs unattended, whatever the config
/// says. A policy hand-edited to `danger` is capped here, not trusted.
const UNATTENDED_RISK_CAP: RiskLevel = RiskLevel::Safe;

/// Every (item, action) the *enabled* policies select from `items`.
///
/// Never selects an action above `Safe`, never selects the same item twice,
/// and never selects an item nested inside another selected item. The
/// result follows the order of `items`; when several policies match an
/// item, the first (in `policies` order) wins.
pub fn policy_selections(items: &[CleanableItem], policies: &[Policy]) -> Vec<PolicySelection> {
    let enabled: Vec<&Policy> = policies.iter().filter(|p| p.enabled).collect();
    let mut candidates: Vec<PolicySelection> = Vec::new();

    for item in items {
        if candidates.iter().any(|c| c.item.id == item.id) {
            continue;
        }
        let chosen = enabled.iter().find_map(|policy| {
            if !matches_policy(item, policy) {
                return None;
            }
            choose_action(item, policy).map(|action| (policy.id.clone(), action))
        });
        if let Some((policy_id, action)) = chosen {
            candidates.push(PolicySelection {
                policy_id,
                item: item.clone(),
                action: action.clone(),
            });
        }
    }

    // Drop anything inside (or equal to) an earlier-kept selection's path,
    // and anything that contains one: cleaning a parent and its child in the
    // same batch double counts the space and races the second delete.
    let mut kept: Vec<PolicySelection> = Vec::new();
    for sel in candidates {
        let overlaps = kept.iter().any(|k| sel.item.path.starts_with(&k.item.path));
        if overlaps {
            continue;
        }
        // A later item that is an ancestor of an already-kept one replaces
        // nothing; the outer item wins, so drop the kept descendants.
        kept.retain(|k| !is_strictly_inside(&k.item.path, &sel.item.path));
        kept.push(sel);
    }
    // `kept` is still in `items` order: retain preserves order and pushes
    // happen in iteration order.
    kept
}

fn is_strictly_inside(path: &Path, ancestor: &Path) -> bool {
    path != ancestor && path.starts_with(ancestor)
}

fn matches_policy(item: &CleanableItem, policy: &Policy) -> bool {
    if !policy.ecosystems.is_empty() && !policy.ecosystems.contains(&item.ecosystem) {
        return false;
    }
    if !policy.kinds.is_empty() && !policy.kinds.contains(&item.kind) {
        return false;
    }
    if policy.min_days_stale > 0 {
        // Unknown staleness never satisfies an age rule.
        match item.days_stale {
            Some(days) if days >= policy.min_days_stale => {}
            _ => return false,
        }
    }
    true
}

/// Whether `action` may run unattended under `policy`.
fn allowed(action: &CleanAction, policy: &Policy) -> bool {
    action.risk <= policy.max_risk && action.risk <= UNATTENDED_RISK_CAP
}

fn choose_action<'a>(item: &'a CleanableItem, policy: &Policy) -> Option<&'a CleanAction> {
    let eligible = item.available_actions.iter().filter(|a| allowed(a, policy));
    match policy.action {
        PolicyAction::Trim => eligible
            .into_iter()
            .find(|a| matches!(a.method, ActionMethod::RemoveDirs { .. })),
        PolicyAction::Safest => {
            let eligible: Vec<&CleanAction> = eligible.collect();
            let lowest = eligible.iter().map(|a| a.risk).min()?;
            let tied: Vec<&CleanAction> =
                eligible.into_iter().filter(|a| a.risk == lowest).collect();
            let is_trash = |a: &&CleanAction| matches!(a.method, ActionMethod::MoveToTrash { .. });
            // Trash is only worth it for things a person might want back;
            // trashing a cache frees nothing until the Trash is emptied.
            let preferred = if crate::trash::worth_recovering(item.kind) {
                tied.iter().find(|a| is_trash(a))
            } else {
                tied.iter().find(|a| !is_trash(a))
            };
            preferred.or_else(|| tied.first()).copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtifactKind, Ecosystem};
    use chrono::TimeZone;
    use std::path::PathBuf;
    use uuid::Uuid;

    const GB: u64 = 1 << 30;

    fn usage(free: u64, total: u64) -> DiskUsage {
        DiskUsage {
            total_bytes: total,
            available_bytes: free,
            used_bytes: total.saturating_sub(free),
        }
    }

    fn action(method: ActionMethod, risk: RiskLevel) -> CleanAction {
        CleanAction {
            id: Uuid::new_v4(),
            label: "act".into(),
            description: String::new(),
            method,
            risk,
            estimated_savings_bytes: 1,
        }
    }

    fn remove_dir(p: &str, risk: RiskLevel) -> CleanAction {
        action(ActionMethod::RemoveDir { path: p.into() }, risk)
    }

    fn remove_dirs(p: &str, risk: RiskLevel) -> CleanAction {
        action(
            ActionMethod::RemoveDirs {
                paths: vec![PathBuf::from(p).join("node_modules")],
            },
            risk,
        )
    }

    fn trash(p: &str, risk: RiskLevel) -> CleanAction {
        action(ActionMethod::MoveToTrash { path: p.into() }, risk)
    }

    fn item(
        path: &str,
        eco: Ecosystem,
        kind: ArtifactKind,
        days: Option<u64>,
        actions: Vec<CleanAction>,
    ) -> CleanableItem {
        CleanableItem {
            id: Uuid::new_v4(),
            path: path.into(),
            ecosystem: eco,
            kind,
            risk: actions
                .iter()
                .map(|a| a.risk)
                .min()
                .unwrap_or(RiskLevel::Safe),
            size_bytes: 1,
            size_display: "1 B".into(),
            last_modified: None,
            days_stale: days,
            project_name: None,
            project_root: None,
            available_actions: actions,
            details: Vec::new(),
            agent: None,
        }
    }

    fn node(path: &str, days: Option<u64>) -> CleanableItem {
        item(
            path,
            Ecosystem::Node,
            ArtifactKind::NodeModules,
            days,
            vec![remove_dir(path, RiskLevel::Safe)],
        )
    }

    fn policy(action: PolicyAction) -> Policy {
        Policy {
            id: "p".into(),
            name: "p".into(),
            enabled: true,
            action,
            ..Default::default()
        }
    }

    // ---- evaluate ----

    #[test]
    fn thresholds_and_boundaries() {
        let cfg = GuardConfig::default(); // warn 15, critical 5
        assert_eq!(evaluate(&usage(50, 100), &cfg).state, DiskState::Ok);
        assert_eq!(evaluate(&usage(15, 100), &cfg).state, DiskState::Ok);
        assert_eq!(evaluate(&usage(14, 100), &cfg).state, DiskState::Low);
        assert_eq!(evaluate(&usage(5, 100), &cfg).state, DiskState::Low);
        assert_eq!(evaluate(&usage(4, 100), &cfg).state, DiskState::Critical);
        assert_eq!(evaluate(&usage(0, 100), &cfg).state, DiskState::Critical);
    }

    #[test]
    fn zero_total_is_ok_and_unknown() {
        let s = evaluate(&usage(0, 0), &GuardConfig::default());
        assert_eq!(s.state, DiskState::Ok);
        assert!(s.message.contains("unknown"));
    }

    #[test]
    fn available_above_total_is_clamped() {
        let s = evaluate(&usage(200, 100), &GuardConfig::default());
        assert_eq!(s.free_percent, 100.0);
        assert_eq!(s.state, DiskState::Ok);
    }

    #[test]
    fn critical_message_is_factual() {
        let s = evaluate(&usage(4 * GB, 100 * GB), &GuardConfig::default());
        assert_eq!(s.state, DiskState::Critical);
        assert!(s.message.starts_with("Only 4"), "{}", s.message);
        assert!(s.message.contains("(4%)"), "{}", s.message);
        assert!(s.message.contains("AI agents may start failing"));
        let tiny = evaluate(&usage(GB / 2, 100 * GB), &GuardConfig::default());
        assert!(tiny.message.contains("(0.5%)"), "{}", tiny.message);
        let low = evaluate(&usage(10 * GB, 100 * GB), &GuardConfig::default());
        assert!(low.message.contains("running low"));
    }

    // ---- scheduling ----

    #[test]
    fn check_due_respects_interval_and_enabled() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let cfg = GuardConfig::default(); // 15 minutes
        assert!(next_check_due(None, now, &cfg));
        assert!(!next_check_due(
            Some(now - Duration::minutes(14)),
            now,
            &cfg
        ));
        assert!(next_check_due(Some(now - Duration::minutes(15)), now, &cfg));
        let off = GuardConfig {
            enabled: false,
            ..GuardConfig::default()
        };
        assert!(!next_check_due(None, now, &off));
        let zero = GuardConfig {
            check_interval_minutes: 0,
            ..GuardConfig::default()
        };
        assert!(!next_check_due(
            Some(now - Duration::seconds(30)),
            now,
            &zero
        ));
    }

    #[test]
    fn notifies_only_when_worsening_or_long_critical() {
        use DiskState::*;
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert!(should_notify(Ok, Low, None, now, 6));
        assert!(should_notify(Ok, Critical, None, now, 6));
        assert!(should_notify(Low, Critical, Some(now), now, 6));
        assert!(!should_notify(Low, Low, None, now, 6));
        assert!(!should_notify(Critical, Low, None, now, 6));
        assert!(!should_notify(Low, Ok, None, now, 6));
        assert!(!should_notify(Ok, Ok, None, now, 6));
        assert!(!should_notify(
            Critical,
            Critical,
            Some(now - Duration::hours(5)),
            now,
            6
        ));
        assert!(should_notify(
            Critical,
            Critical,
            Some(now - Duration::hours(6)),
            now,
            6
        ));
        assert!(should_notify(Critical, Critical, None, now, 6));
    }

    // ---- policies ----

    #[test]
    fn disabled_policy_selects_nothing() {
        let items = vec![node("/w/a/node_modules", Some(100))];
        let mut p = policy(PolicyAction::Safest);
        p.enabled = false;
        assert!(policy_selections(&items, &[p]).is_empty());
        // The built-in defaults all ship disabled.
        assert!(policy_selections(&items, &Policy::defaults()).is_empty());
    }

    #[test]
    fn ecosystem_kind_and_age_filters() {
        let items = vec![
            node("/w/old/node_modules", Some(40)),
            node("/w/new/node_modules", Some(2)),
            node("/w/unknown/node_modules", None),
            item(
                "/w/rs/target",
                Ecosystem::Rust,
                ArtifactKind::TargetDir,
                Some(40),
                vec![remove_dir("/w/rs/target", RiskLevel::Safe)],
            ),
        ];
        let mut p = policy(PolicyAction::Safest);
        p.min_days_stale = 30;
        p.kinds = vec![ArtifactKind::NodeModules];
        let sel = policy_selections(&items, &[p.clone()]);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].item.path, PathBuf::from("/w/old/node_modules"));

        p.kinds.clear();
        p.ecosystems = vec![Ecosystem::Rust];
        let sel = policy_selections(&items, &[p.clone()]);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].item.kind, ArtifactKind::TargetDir);

        // With no age rule, unknown staleness matches.
        let any = policy(PolicyAction::Safest);
        assert_eq!(policy_selections(&items, &[any]).len(), 4);
    }

    #[test]
    fn trim_picks_remove_dirs_and_nothing_else() {
        let wt = item(
            "/w/wt",
            Ecosystem::Worktrees,
            ArtifactKind::AgentWorktree,
            Some(10),
            vec![
                remove_dirs("/w/wt", RiskLevel::Safe),
                trash("/w/wt", RiskLevel::Caution),
            ],
        );
        let no_trim = node("/w/a/node_modules", Some(10));
        let sel = policy_selections(&[wt, no_trim], &[policy(PolicyAction::Trim)]);
        assert_eq!(sel.len(), 1);
        assert!(matches!(
            sel[0].action.method,
            ActionMethod::RemoveDirs { .. }
        ));
    }

    #[test]
    fn trim_does_not_run_a_risky_remove_dirs() {
        let wt = item(
            "/w/wt",
            Ecosystem::Worktrees,
            ArtifactKind::AgentWorktree,
            Some(10),
            vec![remove_dirs("/w/wt", RiskLevel::Caution)],
        );
        assert!(policy_selections(&[wt], &[policy(PolicyAction::Trim)]).is_empty());
    }

    #[test]
    fn safest_is_capped_at_safe_even_if_policy_says_danger() {
        let risky = item(
            "/w/proj",
            Ecosystem::Projects,
            ArtifactKind::StaleProject,
            Some(300),
            vec![
                trash("/w/proj", RiskLevel::Caution),
                remove_dir("/w/proj", RiskLevel::Danger),
            ],
        );
        let cmd = item(
            "/w/cache",
            Ecosystem::Node,
            ArtifactKind::NpmCache,
            Some(300),
            vec![action(
                ActionMethod::Command {
                    program: "rm".into(),
                    args: vec!["-rf".into(), "/".into()],
                    working_dir: None,
                },
                RiskLevel::Danger,
            )],
        );
        let mut p = policy(PolicyAction::Safest);
        p.max_risk = RiskLevel::Danger;
        assert!(policy_selections(&[risky, cmd], &[p]).is_empty());
    }

    #[test]
    fn safe_command_is_allowed() {
        let cmd = item(
            "/w/cache",
            Ecosystem::Go,
            ArtifactKind::GoBuildCache,
            Some(300),
            vec![action(
                ActionMethod::Command {
                    program: "go".into(),
                    args: vec!["clean".into(), "-cache".into()],
                    working_dir: None,
                },
                RiskLevel::Safe,
            )],
        );
        assert_eq!(
            policy_selections(&[cmd], &[policy(PolicyAction::Safest)]).len(),
            1
        );
    }

    #[test]
    fn safest_prefers_permanent_removal_for_caches_and_trash_for_recoverables() {
        let cache = item(
            "/w/a/node_modules",
            Ecosystem::Node,
            ArtifactKind::NodeModules,
            Some(40),
            vec![
                trash("/w/a/node_modules", RiskLevel::Safe),
                remove_dir("/w/a/node_modules", RiskLevel::Safe),
            ],
        );
        let transcripts = item(
            "/h/.claude/projects/x",
            Ecosystem::AgentData,
            ArtifactKind::AgentTranscripts,
            Some(40),
            vec![
                remove_dir("/h/.claude/projects/x", RiskLevel::Safe),
                trash("/h/.claude/projects/x", RiskLevel::Safe),
            ],
        );
        let sel = policy_selections(&[cache, transcripts], &[policy(PolicyAction::Safest)]);
        assert_eq!(sel.len(), 2);
        assert!(matches!(
            sel[0].action.method,
            ActionMethod::RemoveDir { .. }
        ));
        assert!(matches!(
            sel[1].action.method,
            ActionMethod::MoveToTrash { .. }
        ));
    }

    #[test]
    fn safest_picks_the_lowest_risk() {
        let it = item(
            "/w/x",
            Ecosystem::Node,
            ArtifactKind::NodeModules,
            Some(40),
            vec![
                remove_dir("/w/x", RiskLevel::Caution),
                remove_dirs("/w/x", RiskLevel::Safe),
            ],
        );
        let sel = policy_selections(&[it], &[policy(PolicyAction::Safest)]);
        assert!(matches!(
            sel[0].action.method,
            ActionMethod::RemoveDirs { .. }
        ));
    }

    #[test]
    fn first_matching_policy_wins_and_items_are_not_duplicated() {
        let items = vec![node("/w/a/node_modules", Some(40))];
        let mut first = policy(PolicyAction::Safest);
        first.id = "first".into();
        let mut second = policy(PolicyAction::Safest);
        second.id = "second".into();
        let sel = policy_selections(&items, &[first.clone(), second.clone()]);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].policy_id, "first");

        // A policy that matches but has no usable action falls through.
        let mut trim = policy(PolicyAction::Trim);
        trim.id = "trim".into();
        let sel = policy_selections(&items, &[trim, second]);
        assert_eq!(sel[0].policy_id, "second");

        // The same item passed twice is selected once.
        let twice = vec![items[0].clone(), items[0].clone()];
        assert_eq!(policy_selections(&twice, &[first]).len(), 1);
    }

    #[test]
    fn nested_items_are_excluded_either_order() {
        let outer = node("/w/app/node_modules", Some(40));
        let inner = node("/w/app/node_modules/pkg/node_modules", Some(40));
        let sibling = node("/w/app2/node_modules", Some(40));
        let p = [policy(PolicyAction::Safest)];

        let sel = policy_selections(&[outer.clone(), inner.clone(), sibling.clone()], &p);
        let paths: Vec<_> = sel.iter().map(|s| s.item.path.clone()).collect();
        assert_eq!(paths, vec![outer.path.clone(), sibling.path.clone()]);

        let sel = policy_selections(&[inner, sibling.clone(), outer.clone()], &p);
        let paths: Vec<_> = sel.iter().map(|s| s.item.path.clone()).collect();
        assert_eq!(paths, vec![sibling.path, outer.path]);
    }

    #[test]
    fn prefix_that_is_not_a_path_ancestor_is_not_nested() {
        let a = node("/w/app", Some(40));
        let b = node("/w/app-old", Some(40));
        assert_eq!(
            policy_selections(&[a, b], &[policy(PolicyAction::Safest)]).len(),
            2
        );
    }
}
