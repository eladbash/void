//! Action selection and risk resolution.
//!
//! `CleanableItem.risk` and `CleanAction.risk` are different fields and they
//! disagree: `SimulatorDevices` is a Caution item holding a Danger action;
//! `~/Downloads` is a Safe item holding `rm -rf`. Everything here works from
//! the action that will actually run.

use std::collections::HashMap;

use deepclean_core::model::{ActionMethod, ArtifactKind, CleanAction, CleanableItem, RiskLevel};
use uuid::Uuid;

pub fn risk_rank(risk: RiskLevel) -> u8 {
    match risk {
        RiskLevel::Safe => 0,
        RiskLevel::Caution => 1,
        RiskLevel::Danger => 2,
    }
}

pub fn risk_id(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "safe",
        RiskLevel::Caution => "caution",
        RiskLevel::Danger => "danger",
    }
}

/// Whether an action puts things in the Trash rather than destroying them.
///
/// `MoveToTrash` is the executor's own recoverable delete. The two legacy
/// shell one-liners (Finder / Shell.Application) are matched on the command.
pub fn is_recoverable(action: &CleanAction) -> bool {
    match &action.method {
        ActionMethod::MoveToTrash { .. } => true,
        ActionMethod::Command { program, args, .. } => {
            let line = format!("{program} {}", args.join(" ")).to_lowercase();
            (line.contains("osascript") && line.contains("finder") && line.contains("delete"))
                || (line.contains("shell.application") && line.contains("namespace(10)"))
        }
        _ => false,
    }
}

/// Kinds worth a recoverable delete, mirroring `trash::worth_recovering`.
///
/// Everything else is a rebuildable cache: moving `node_modules` to the Trash
/// frees nothing until the Trash is emptied, which defeats a disk cleaner.
pub const RECOVERY_WORTHY_KINDS: [ArtifactKind; 7] = [
    ArtifactKind::DownloadsDir,
    ArtifactKind::OrphanWorktree,
    ArtifactKind::StaleProject,
    ArtifactKind::AgentTranscripts,
    ArtifactKind::AgentFileHistory,
    ArtifactKind::EditorWorkspaceStorage,
    ArtifactKind::Archives,
];

pub fn worth_recovering(kind: ArtifactKind) -> bool {
    RECOVERY_WORTHY_KINDS.contains(&kind)
}

/// Items with nothing to run are informational: shown, never selected.
pub fn is_actionable(item: &CleanableItem) -> bool {
    !item.available_actions.is_empty()
}

/// Pick the action that runs by default. Deterministic and stated:
///   1. lowest risk
///   2. on a risk tie, the recoverable action wins for recovery-worthy kinds
///      when Trash is preferred; for rebuildable caches the permanent delete
///      wins, because Trash frees nothing until it is emptied
///   3. largest estimate (`Remove target/` over `cargo clean`)
///   4. first listed
pub fn default_action(item: &CleanableItem, prefer_trash: bool) -> Option<&CleanAction> {
    let mut actions = item.available_actions.iter();
    let first = actions.next()?;
    Some(actions.fold(first, |best, candidate| {
        let (a, b) = (risk_rank(candidate.risk), risk_rank(best.risk));
        if a != b {
            return if a < b { candidate } else { best };
        }
        let (ta, tb) = (is_recoverable(candidate), is_recoverable(best));
        if ta != tb {
            let worthy = worth_recovering(item.kind);
            if prefer_trash && worthy {
                return if ta { candidate } else { best };
            }
            if !worthy {
                return if ta { best } else { candidate };
            }
        }
        let (ea, eb) = (
            candidate.estimated_savings_bytes,
            best.estimated_savings_bytes,
        );
        if ea != eb {
            return if ea > eb { candidate } else { best };
        }
        best
    }))
}

/// The action currently chosen for an item, honouring any manual override.
pub fn selected_action<'a>(
    item: &'a CleanableItem,
    overrides: &HashMap<Uuid, Uuid>,
    prefer_trash: bool,
) -> Option<&'a CleanAction> {
    if let Some(id) = overrides.get(&item.id) {
        if let Some(found) = item.available_actions.iter().find(|a| a.id == *id) {
            return Some(found);
        }
    }
    default_action(item, prefer_trash)
}

/// Risk to display for a row: the selected action's, never the item's.
pub fn effective_risk(
    item: &CleanableItem,
    overrides: &HashMap<Uuid, Uuid>,
    prefer_trash: bool,
) -> RiskLevel {
    selected_action(item, overrides, prefer_trash)
        .map(|a| a.risk)
        .unwrap_or(item.risk)
}

/// What an action physically does.
///
/// For commands this is the literal command line. Void does not interpret
/// what a command removes, so the command string is the only honest
/// disclosure the user gets — which is why it is shown, not summarised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDesc {
    pub glyph: &'static str,
    pub text: String,
    pub working_dir: Option<String>,
    pub paths: Vec<String>,
}

fn desc(glyph: &'static str, text: impl Into<String>, working_dir: Option<String>) -> MethodDesc {
    MethodDesc {
        glyph,
        text: text.into(),
        working_dir,
        paths: vec![],
    }
}

fn s(p: &std::path::Path) -> String {
    p.display().to_string()
}

pub fn describe_method(method: &ActionMethod) -> MethodDesc {
    match method {
        ActionMethod::Command {
            program,
            args,
            working_dir,
        } => {
            let line = std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            desc("▶", line, working_dir.as_deref().map(s))
        }
        ActionMethod::RemoveDir { .. } => desc("⌫", "Delete directory recursively", None),
        ActionMethod::RemoveFile { .. } => desc("⌫", "Delete file", None),
        ActionMethod::DockerPrune { prune_type } => {
            desc("▶", format!("docker {prune_type} prune -f"), None)
        }
        ActionMethod::MoveToTrash { .. } => desc("↩", "Move to Trash", None),
        ActionMethod::RemoveDirs { paths } => {
            let n = paths.len();
            let mut d = desc(
                "⌫",
                format!("Delete {n} director{}", if n == 1 { "y" } else { "ies" }),
                None,
            );
            d.paths = paths.iter().map(|p| s(p)).collect();
            d
        }
        ActionMethod::RemoveFiles { paths } => {
            let n = paths.len();
            let mut d = desc(
                "⌫",
                format!("Delete {n} file{}", if n == 1 { "" } else { "s" }),
                None,
            );
            d.paths = paths.iter().map(|p| s(p)).collect();
            d
        }
        ActionMethod::GitWorktreeRemove {
            repo,
            worktree,
            delete_branch,
        } => {
            let base = format!("git worktree remove {}", worktree.display());
            let text = match delete_branch {
                Some(branch) => format!("{base} && git branch -d {branch}"),
                None => base,
            };
            desc("▶", text, Some(s(repo)))
        }
        ActionMethod::GitWorktreePrune { repo } => desc("▶", "git worktree prune", Some(s(repo))),
        ActionMethod::RemoveOllamaOrphans { store, blobs } => {
            let n = blobs.len();
            desc(
                "⌫",
                format!(
                    "Delete {n} unreferenced Ollama blob{} — manifests re-checked first",
                    if n == 1 { "" } else { "s" }
                ),
                Some(s(store)),
            )
        }
        ActionMethod::GitDeleteBranches { repo, branches } => desc(
            "▶",
            format!("git branch -d {}", branches.join(" ")),
            Some(s(repo)),
        ),
        ActionMethod::DedupFiles { groups } => {
            let n: usize = groups.iter().map(|g| g.duplicates.len()).sum();
            desc(
                "⧉",
                format!(
                    "Replace {n} duplicate file{} with copy-on-write clones",
                    if n == 1 { "" } else { "s" }
                ),
                None,
            )
        }
    }
}

/// Whether the confirmation dialog shows the literal command line.
pub fn shows_command_line(method: &ActionMethod) -> bool {
    matches!(
        method,
        ActionMethod::Command { .. }
            | ActionMethod::DockerPrune { .. }
            | ActionMethod::GitWorktreeRemove { .. }
            | ActionMethod::GitWorktreePrune { .. }
            | ActionMethod::GitDeleteBranches { .. }
    )
}

/// Coarse grouping used by the confirmation dialog's "what will happen".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodClass {
    Directories,
    Files,
    Commands,
    Dedup,
    Trash,
}

pub fn method_class(method: &ActionMethod) -> MethodClass {
    match method {
        ActionMethod::RemoveDir { .. } | ActionMethod::RemoveDirs { .. } => {
            MethodClass::Directories
        }
        ActionMethod::RemoveFile { .. }
        | ActionMethod::RemoveFiles { .. }
        | ActionMethod::RemoveOllamaOrphans { .. } => MethodClass::Files,
        ActionMethod::MoveToTrash { .. } => MethodClass::Trash,
        ActionMethod::DedupFiles { .. } => MethodClass::Dedup,
        _ => MethodClass::Commands,
    }
}

/// How much an action frees, for the drawer's outcome line.
///
/// `Frees unknown` rather than `0 B`: many commands genuinely report zero,
/// and printing "0 B" would claim they free nothing.
pub struct Outcome {
    pub frees: Option<u64>,
    pub recoverable: bool,
    pub note: &'static str,
}

pub fn outcome_text(action: &CleanAction) -> Outcome {
    let recoverable = is_recoverable(action);
    Outcome {
        frees: (action.estimated_savings_bytes > 0).then_some(action.estimated_savings_bytes),
        recoverable,
        note: if recoverable {
            "Recoverable — moves to the Trash"
        } else {
            "Not recoverable"
        },
    }
}

/// A safety claim that is true for a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyLine {
    pub ok: bool,
    pub text: &'static str,
}

const fn ok(text: &'static str) -> SafetyLine {
    SafetyLine { ok: true, text }
}

/// `SafetyChecker` gates paths; for commands it also scans arguments, but it
/// cannot know what an arbitrary command does internally. Saying so costs
/// one line.
pub fn safety_lines(method: &ActionMethod) -> Vec<SafetyLine> {
    let path_checks = [
        ok("Path is outside all protected locations"),
        ok("No credential files detected alongside it"),
        ok("Checked against your blocklist before deleting"),
    ];
    match method {
        ActionMethod::RemoveDir { .. }
        | ActionMethod::RemoveFile { .. }
        | ActionMethod::RemoveDirs { .. }
        | ActionMethod::RemoveFiles { .. } => path_checks.to_vec(),
        ActionMethod::MoveToTrash { .. } => {
            let mut v = path_checks.to_vec();
            v.push(ok("Recoverable from the Trash until you empty it"));
            v
        }
        ActionMethod::GitWorktreeRemove { delete_branch, .. } => {
            let mut v = vec![ok(
                "Re-checked just before running: refused if the worktree has uncommitted or unpushed work, or is locked",
            )];
            if delete_branch.is_some() {
                v.push(ok(
                    "git branch -d refuses to delete a branch that is not merged",
                ));
            }
            v.push(ok(
                "Repository and worktree checked against protected paths",
            ));
            v
        }
        ActionMethod::RemoveOllamaOrphans { .. } => vec![
            ok("Every manifest is re-read just before deleting; a blob any model now uses is kept"),
            ok("Refused outright if any manifest cannot be parsed"),
            ok("Store checked against protected paths and your blocklist"),
        ],
        ActionMethod::GitWorktreePrune { .. } => {
            vec![ok(
                "Only removes git’s records of worktrees that no longer exist on disk",
            )]
        }
        ActionMethod::GitDeleteBranches { .. } => {
            vec![ok(
                "git branch -d refuses to delete a branch that is not merged",
            )]
        }
        ActionMethod::DedupFiles { .. } => vec![
            ok("Every path keeps its file — duplicates become clones of one copy"),
            ok("Contents are re-hashed before replacing; any mismatch aborts that group"),
        ],
        ActionMethod::Command { working_dir, .. } => {
            let mut v = vec![];
            if working_dir.is_some() {
                v.push(ok("Working directory checked against protected paths"));
            }
            v.push(ok("Command arguments checked against protected paths"));
            v.push(SafetyLine {
                ok: false,
                text: "Void runs this command as-is; it does not inspect what the command deletes",
            });
            v
        }
        ActionMethod::DockerPrune { .. } => vec![
            ok("Command arguments checked against protected paths"),
            SafetyLine {
                ok: false,
                text: "Void runs this command as-is; it does not inspect what the command deletes",
            },
        ],
    }
}

/// Colour of a worktree state chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Safe,
    Caution,
    Danger,
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub key: &'static str,
    pub label: &'static str,
    pub tone: Tone,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub branch: Option<String>,
    pub chips: Vec<Chip>,
}

/// A detail value that means "no": `no`, `none`, `0`, `clean`, `false`, `—`.
fn is_negative(value: &str) -> bool {
    let v = value.trim().to_lowercase();
    if v.is_empty() {
        return true;
    }
    ["no", "none", "0", "clean", "false", "—", "-"]
        .iter()
        .any(|w| {
            v.strip_prefix(w).is_some_and(|rest| {
                rest.is_empty() || !rest.chars().next().unwrap().is_alphanumeric()
            })
        })
}

fn starts_with_word(value: &str, words: &[&str]) -> bool {
    let v = value.trim().to_lowercase();
    words.iter().any(|w| {
        v.strip_prefix(w)
            .is_some_and(|rest| rest.is_empty() || !rest.chars().next().unwrap().is_alphanumeric())
    })
}

/// Branch and state chips for a worktree row, read from `item.details`.
///
/// Labels are matched loosely so a reworded detail degrades to "no chip",
/// never a wrong one.
pub fn worktree_info(item: &CleanableItem) -> WorktreeInfo {
    let mut info = WorktreeInfo::default();
    let add = |chips: &mut Vec<Chip>, key, label, tone, title: String| {
        if !chips.iter().any(|c: &Chip| c.key == key) {
            chips.push(Chip {
                key,
                label,
                tone,
                title,
            });
        }
    };
    for d in &item.details {
        let label = d.label.to_lowercase();
        let value = d.value.as_str();
        let title = format!("{}: {}", d.label, d.value);
        if info.branch.is_none() && starts_with_word(&label, &["branch"]) {
            info.branch = Some(value.to_string());
            continue;
        }
        let has = |words: &[&str]| words.iter().any(|w| label.contains(w));
        if has(&["uncommitted", "dirty", "modified", "changes"]) {
            if !is_negative(value) {
                add(&mut info.chips, "dirty", "dirty", Tone::Caution, title);
            }
        } else if has(&["unpushed", "ahead", "not pushed"]) {
            if !is_negative(value) {
                add(
                    &mut info.chips,
                    "unpushed",
                    "unpushed",
                    Tone::Caution,
                    title,
                );
            }
        } else if has(&["merged"]) {
            if starts_with_word(value, &["yes", "true", "merged"]) {
                add(&mut info.chips, "merged", "merged", Tone::Safe, title);
            }
        } else if has(&["locked"]) && !is_negative(value) {
            add(&mut info.chips, "locked", "locked", Tone::Neutral, title);
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::*;
    use deepclean_core::model::{DedupGroup, Ecosystem};

    #[test]
    fn trash_and_the_legacy_finder_one_liner_are_recoverable_rm_is_not() {
        assert!(is_recoverable(&action(trash("/a"), RiskLevel::Safe, 0)));
        assert!(is_recoverable(&action(
            command(
                "osascript",
                &[
                    "-e",
                    "tell application \"Finder\" to delete POSIX file \"/a\""
                ]
            ),
            RiskLevel::Safe,
            0
        )));
        assert!(!is_recoverable(&action(
            command("rm", &["-rf", "/a"]),
            RiskLevel::Safe,
            0
        )));
        assert!(!is_recoverable(&action(
            remove_dir("/a"),
            RiskLevel::Safe,
            0
        )));
        assert!(outcome_text(&action(trash("/a"), RiskLevel::Safe, 0))
            .note
            .contains("Recoverable"));
    }

    #[test]
    fn recovery_worthy_kinds_mirror_trash_worth_recovering() {
        for kind in RECOVERY_WORTHY_KINDS {
            assert!(deepclean_core::trash::worth_recovering(kind), "{kind:?}");
        }
        assert!(!worth_recovering(ArtifactKind::NodeModules));
    }

    #[test]
    fn prefer_trash_picks_the_trash_action_for_recovery_worthy_kinds() {
        let mut it = item();
        it.kind = ArtifactKind::StaleProject;
        it.available_actions = delete_and_trash("/p", RiskLevel::Caution, 1000);
        assert!(matches!(
            default_action(&it, true).unwrap().method,
            ActionMethod::MoveToTrash { .. }
        ));
    }

    #[test]
    fn with_prefer_trash_off_a_recovery_worthy_item_falls_back_to_listed_order() {
        let mut it = item();
        it.kind = ArtifactKind::AgentTranscripts;
        it.available_actions = delete_and_trash("/t", RiskLevel::Safe, 1000);
        assert!(matches!(
            default_action(&it, false).unwrap().method,
            ActionMethod::RemoveDir { .. }
        ));
    }

    #[test]
    fn regenerable_caches_keep_the_permanent_delete_even_when_trash_is_preferred() {
        let mut it = item();
        let mut actions = delete_and_trash("/nm", RiskLevel::Safe, 1000);
        actions.reverse();
        it.available_actions = actions;
        assert!(matches!(
            default_action(&it, true).unwrap().method,
            ActionMethod::RemoveDir { .. }
        ));
        assert!(matches!(
            default_action(&it, false).unwrap().method,
            ActionMethod::RemoveDir { .. }
        ));
    }

    #[test]
    fn lower_risk_always_wins_over_trash_preference() {
        let mut it = item();
        it.kind = ArtifactKind::DownloadsDir;
        it.available_actions = vec![
            action(trash("/d"), RiskLevel::Caution, 10),
            action(
                ActionMethod::RemoveFile { path: "/d".into() },
                RiskLevel::Safe,
                10,
            ),
        ];
        assert_eq!(default_action(&it, true).unwrap().risk, RiskLevel::Safe);
    }

    #[test]
    fn largest_estimate_breaks_a_risk_tie() {
        let mut it = item();
        it.kind = ArtifactKind::TargetDir;
        it.available_actions = vec![
            action(command("cargo", &["clean"]), RiskLevel::Safe, 0),
            action(remove_dir("/t"), RiskLevel::Safe, 3800),
        ];
        assert!(matches!(
            default_action(&it, true).unwrap().method,
            ActionMethod::RemoveDir { .. }
        ));
    }

    #[test]
    fn effective_risk_follows_the_action_that_will_run_and_overrides() {
        let safe = action(remove_dir("/x"), RiskLevel::Safe, 0);
        let danger = action(remove_dir("/sim"), RiskLevel::Danger, 0);
        let mut it = item();
        it.risk = RiskLevel::Caution;
        it.available_actions = vec![danger.clone(), safe];
        let mut overrides = HashMap::new();
        assert_eq!(effective_risk(&it, &overrides, true), RiskLevel::Safe);
        overrides.insert(it.id, danger.id);
        assert_eq!(
            selected_action(&it, &overrides, true).unwrap().id,
            danger.id
        );
        assert_eq!(effective_risk(&it, &overrides, true), RiskLevel::Danger);

        let mut info = item();
        info.risk = RiskLevel::Caution;
        assert!(default_action(&info, true).is_none());
        assert_eq!(
            effective_risk(&info, &HashMap::new(), true),
            RiskLevel::Caution
        );
        assert!(!is_actionable(&info));
        assert!(is_actionable(&it));
    }

    #[test]
    fn describe_method_states_what_every_method_runs() {
        let cases: Vec<(ActionMethod, &str)> = vec![
            (trash("/a"), "Move to Trash"),
            (
                ActionMethod::RemoveDirs {
                    paths: vec!["/a".into(), "/b".into()],
                },
                "Delete 2 directories",
            ),
            (
                ActionMethod::RemoveDirs {
                    paths: vec!["/a".into()],
                },
                "Delete 1 directory",
            ),
            (
                ActionMethod::RemoveFiles {
                    paths: vec!["/a".into(), "/b".into(), "/c".into()],
                },
                "Delete 3 files",
            ),
            (
                ActionMethod::GitWorktreeRemove {
                    repo: "/r".into(),
                    worktree: "/r/.claude/worktrees/x".into(),
                    delete_branch: Some("fix".into()),
                },
                "git worktree remove /r/.claude/worktrees/x && git branch -d fix",
            ),
            (
                ActionMethod::GitWorktreeRemove {
                    repo: "/r".into(),
                    worktree: "/w".into(),
                    delete_branch: None,
                },
                "git worktree remove /w",
            ),
            (
                ActionMethod::GitWorktreePrune { repo: "/r".into() },
                "git worktree prune",
            ),
            (
                ActionMethod::GitDeleteBranches {
                    repo: "/r".into(),
                    branches: vec!["a".into(), "b".into()],
                },
                "git branch -d a b",
            ),
            (
                ActionMethod::DedupFiles {
                    groups: vec![DedupGroup {
                        keep: "/k".into(),
                        duplicates: vec!["/d1".into(), "/d2".into()],
                        size_bytes: 5,
                        hash: "h".into(),
                    }],
                },
                "Replace 2 duplicate files with copy-on-write clones",
            ),
            (
                ActionMethod::RemoveOllamaOrphans {
                    store: "/o".into(),
                    blobs: vec!["/o/b1".into(), "/o/b2".into()],
                },
                "Delete 2 unreferenced Ollama blobs — manifests re-checked first",
            ),
            (
                ActionMethod::DockerPrune {
                    prune_type: "image".into(),
                },
                "docker image prune -f",
            ),
        ];
        for (method, text) in cases {
            assert_eq!(describe_method(&method).text, text);
            assert!(
                !safety_lines(&method).is_empty(),
                "{method:?} has safety lines"
            );
        }
        assert_eq!(
            describe_method(&ActionMethod::GitWorktreePrune { repo: "/r".into() })
                .working_dir
                .as_deref(),
            Some("/r")
        );
        assert_eq!(
            describe_method(&ActionMethod::RemoveDirs {
                paths: vec!["/a".into()]
            })
            .paths,
            vec!["/a"]
        );
    }

    #[test]
    fn method_class_groups_methods_for_the_confirmation_dialog() {
        assert_eq!(
            method_class(&ActionMethod::RemoveDirs { paths: vec![] }),
            MethodClass::Directories
        );
        assert_eq!(
            method_class(&ActionMethod::RemoveFiles { paths: vec![] }),
            MethodClass::Files
        );
        assert_eq!(
            method_class(&ActionMethod::RemoveOllamaOrphans {
                store: "/".into(),
                blobs: vec![]
            }),
            MethodClass::Files
        );
        assert_eq!(method_class(&trash("/a")), MethodClass::Trash);
        assert_eq!(
            method_class(&ActionMethod::DedupFiles { groups: vec![] }),
            MethodClass::Dedup
        );
        assert_eq!(
            method_class(&ActionMethod::GitWorktreeRemove {
                repo: "/r".into(),
                worktree: "/w".into(),
                delete_branch: None
            }),
            MethodClass::Commands
        );
    }

    #[test]
    fn safety_copy_never_claims_commands_are_inspected() {
        assert!(safety_lines(&command("npm", &[])).iter().any(|l| !l.ok));
    }

    #[test]
    fn worktree_info_reads_branch_and_state_chips_from_details() {
        let mut it = item();
        it.ecosystem = Ecosystem::Worktrees;
        it.kind = ArtifactKind::AgentWorktree;
        it.details = vec![
            detail("Branch", "worktree-fix-login"),
            detail("Uncommitted changes", "3 files"),
            detail("Unpushed commits", "0"),
            detail("Merged", "yes — PR #12 closed"),
            detail("Locked", "no"),
        ];
        let info = worktree_info(&it);
        assert_eq!(info.branch.as_deref(), Some("worktree-fix-login"));
        assert_eq!(
            info.chips.iter().map(|c| c.key).collect::<Vec<_>>(),
            ["dirty", "merged"]
        );

        let mut clean = item();
        clean.details = vec![
            detail("Uncommitted changes", "none"),
            detail("Locked", "yes"),
        ];
        assert_eq!(
            worktree_info(&clean)
                .chips
                .iter()
                .map(|c| c.key)
                .collect::<Vec<_>>(),
            ["locked"]
        );
        assert!(worktree_info(&item()).chips.is_empty());
    }
}
