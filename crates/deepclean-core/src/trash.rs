//! Recoverable deletion.
//!
//! Every `RemoveDir`/`RemoveFile` used to be a permanent `remove_dir_all`.
//! That is fine for a `node_modules` that `npm install` rebuilds, and wrong
//! for anything a person might want back: an agent worktree, an abandoned
//! project, a transcript. This module gives the executor a Trash it can move
//! things into, and decides which items are worth offering it for.
//!
//! The backend is injectable so tests never touch the real Trash: CI runs
//! against [`TrashBackend::Directory`], which moves into a temp folder.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::model::{ActionMethod, ArtifactKind, CleanAction, CleanableItem};

/// Where "Move to Trash" puts things.
#[derive(Debug, Clone, Default)]
pub enum TrashBackend {
    /// The operating system's Trash / Recycle Bin.
    #[default]
    System,
    /// A plain directory. Used by tests and the development sandbox so a
    /// trash operation is observable and never reaches the real Trash.
    Directory(PathBuf),
}

impl TrashBackend {
    /// Move `path` into the Trash.
    pub fn trash(&self, path: &Path) -> Result<(), String> {
        if !path.exists() && !path.is_symlink() {
            return Err(format!("{} no longer exists", path.display()));
        }
        match self {
            Self::System => trash::delete(path).map_err(|err| err.to_string()),
            Self::Directory(dir) => {
                std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "item".into());
                // Unique per call, so trashing two `node_modules` never collides.
                let dest = dir.join(format!("{}-{name}", Uuid::new_v4().simple()));
                std::fs::rename(path, &dest).map_err(|err| err.to_string())
            }
        }
    }
}

/// Kinds whose contents cannot simply be regenerated, so a recoverable delete
/// is worth offering — and worth defaulting to when the user prefers Trash.
///
/// Deliberately excludes rebuildable caches (`node_modules`, `target`, pip,
/// Docker): moving those to the Trash frees nothing until it is emptied,
/// which defeats the point of a disk cleaner.
pub fn worth_recovering(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::DownloadsDir
            | ArtifactKind::OrphanWorktree
            | ArtifactKind::StaleProject
            | ArtifactKind::AgentTranscripts
            | ArtifactKind::AgentFileHistory
            | ArtifactKind::EditorWorkspaceStorage
            | ArtifactKind::Archives
    )
}

/// Offer "Move to Trash" beside each permanent delete of a recovery-worthy
/// item. Idempotent: an item that already has a Trash action for a path does
/// not get a second one.
pub fn add_trash_alternatives(mut item: CleanableItem) -> CleanableItem {
    if !worth_recovering(item.kind) {
        return item;
    }

    let already_trashed: Vec<PathBuf> = item
        .available_actions
        .iter()
        .filter_map(|a| match &a.method {
            ActionMethod::MoveToTrash { path } => Some(path.clone()),
            _ => None,
        })
        .collect();

    let additions: Vec<CleanAction> = item
        .available_actions
        .iter()
        .filter_map(|a| {
            let path = match &a.method {
                ActionMethod::RemoveDir { path } | ActionMethod::RemoveFile { path } => path,
                _ => return None,
            };
            if already_trashed.contains(path) {
                return None;
            }
            Some(CleanAction {
                id: Uuid::new_v4(),
                label: "Move to Trash".into(),
                description: format!(
                    "Move {} to the Trash. Recoverable; the space is freed when the Trash is emptied.",
                    path.display()
                ),
                method: ActionMethod::MoveToTrash { path: path.clone() },
                risk: a.risk,
                estimated_savings_bytes: a.estimated_savings_bytes,
            })
        })
        .collect();

    item.available_actions.extend(additions);
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ecosystem, RiskLevel};

    fn item(kind: ArtifactKind, path: &Path) -> CleanableItem {
        CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Projects,
            kind,
            risk: RiskLevel::Caution,
            size_bytes: 10,
            size_display: "10 B".into(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Delete".into(),
                description: String::new(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: 10,
            }],
        }
    }

    #[test]
    fn directory_backend_moves_and_keeps_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("project");
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("notes.md"), "keep me").unwrap();
        let bin = tmp.path().join("bin");

        TrashBackend::Directory(bin.clone()).trash(&victim).unwrap();

        assert!(!victim.exists());
        let moved: Vec<_> = std::fs::read_dir(&bin).unwrap().flatten().collect();
        assert_eq!(moved.len(), 1);
        assert_eq!(
            std::fs::read_to_string(moved[0].path().join("notes.md")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn trashing_a_missing_path_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = TrashBackend::Directory(tmp.path().join("bin"));
        assert!(backend.trash(&tmp.path().join("gone")).is_err());
    }

    #[test]
    fn recovery_worthy_items_gain_a_trash_action_once() {
        let tmp = tempfile::tempdir().unwrap();
        let it = add_trash_alternatives(item(ArtifactKind::StaleProject, tmp.path()));
        let trash_count = |i: &CleanableItem| {
            i.available_actions
                .iter()
                .filter(|a| matches!(a.method, ActionMethod::MoveToTrash { .. }))
                .count()
        };
        assert_eq!(trash_count(&it), 1);
        // Idempotent.
        let again = add_trash_alternatives(it);
        assert_eq!(trash_count(&again), 1);
    }

    #[test]
    fn rebuildable_caches_do_not_gain_a_trash_action() {
        let tmp = tempfile::tempdir().unwrap();
        let it = add_trash_alternatives(item(ArtifactKind::NodeModules, tmp.path()));
        assert!(it
            .available_actions
            .iter()
            .all(|a| !matches!(a.method, ActionMethod::MoveToTrash { .. })));
    }
}
