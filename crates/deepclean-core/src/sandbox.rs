//! Sandboxed runs: a fake home that nothing can escape.
//!
//! `void --home <dir>` and the desktop app's `VOID_HOME=<dir>` both point
//! Void at a seeded fake home (`void dev seed`). Scanners are rooted there
//! (see `paths` and `scanner::registry`), config, history, plans and the
//! Trash live inside it, and [`confine`] strips every result or action that
//! could still reach the real machine.

use std::path::Path;

use crate::model::{ActionMethod, CleanableItem};

/// Directory names used inside a sandbox home.
pub const SANDBOX_TRASH: &str = ".void-sandbox-trash";
pub const SANDBOX_PLANS: &str = ".void-sandbox-plans";
pub const SANDBOX_CONFIG: &str = ".void-sandbox-config";

/// Keep a sandbox scan inside the sandbox.
///
/// Some scanners report locations that are not under home at all — Docker's
/// images, Homebrew's cache, system logs — or run commands that act on the
/// real machine. A sandbox run must never plan those: drop items outside
/// home, and drop actions that could reach beyond it.
pub fn confine(items: Vec<CleanableItem>, home: &Path) -> Vec<CleanableItem> {
    let inside = |p: &Path| {
        let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        p.starts_with(home)
    };
    items
        .into_iter()
        .filter(|item| inside(&item.path))
        .filter_map(|mut item| {
            item.available_actions.retain(|a| match &a.method {
                ActionMethod::DockerPrune { .. } => false,
                ActionMethod::Command {
                    working_dir, args, ..
                } => {
                    working_dir.as_deref().is_some_and(inside)
                        && args
                            .iter()
                            .filter(|arg| arg.starts_with('/') || arg.starts_with('~'))
                            .all(|arg| !arg.starts_with('~') && inside(Path::new(arg)))
                }
                ActionMethod::RemoveDir { path }
                | ActionMethod::RemoveFile { path }
                | ActionMethod::MoveToTrash { path } => inside(path),
                ActionMethod::RemoveDirs { paths } | ActionMethod::RemoveFiles { paths } => {
                    paths.iter().all(|p| inside(p))
                }
                ActionMethod::GitWorktreeRemove { repo, worktree, .. } => {
                    inside(repo) && inside(worktree)
                }
                ActionMethod::GitWorktreePrune { repo }
                | ActionMethod::GitDeleteBranches { repo, .. } => inside(repo),
                ActionMethod::RemoveOllamaOrphans { store, blobs } => {
                    inside(store) && blobs.iter().all(|b| inside(b))
                }
                ActionMethod::DedupFiles { groups } => groups
                    .iter()
                    .all(|g| inside(&g.keep) && g.duplicates.iter().all(|d| inside(d))),
            });
            (!item.available_actions.is_empty()).then_some(item)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtifactKind, CleanAction, Ecosystem, RiskLevel};
    use uuid::Uuid;

    fn item(path: &Path, methods: Vec<ActionMethod>) -> CleanableItem {
        CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Models,
            kind: ArtifactKind::OllamaModel,
            risk: RiskLevel::Caution,
            size_bytes: 1,
            size_display: String::new(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: methods
                .into_iter()
                .map(|method| CleanAction {
                    id: Uuid::new_v4(),
                    label: String::new(),
                    description: String::new(),
                    method,
                    risk: RiskLevel::Caution,
                    estimated_savings_bytes: 0,
                })
                .collect(),
            details: Vec::new(),
            agent: None,
        }
    }

    #[test]
    fn commands_that_would_act_on_the_real_machine_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let store = home.join(".ollama/models");
        std::fs::create_dir_all(&store).unwrap();
        // `ollama rm` talks to the real Ollama server, whatever the sandbox.
        let model = item(
            &store,
            vec![
                ActionMethod::Command {
                    program: "ollama".into(),
                    args: vec!["rm".into(), "llama3.2:1b".into()],
                    working_dir: None,
                },
                ActionMethod::RemoveDir {
                    path: store.clone(),
                },
            ],
        );
        let outside = item(
            Path::new("/var/run/docker.sock"),
            vec![ActionMethod::DockerPrune {
                prune_type: "system".into(),
            }],
        );
        let kept = confine(vec![model, outside], &home);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].available_actions.len(), 1);
        assert!(matches!(
            kept[0].available_actions[0].method,
            ActionMethod::RemoveDir { .. }
        ));
    }
}
