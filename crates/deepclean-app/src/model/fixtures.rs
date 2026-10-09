//! Builders shared by the model and view tests. Shapes mirror
//! `deepclean_core::model`, defaults mirror the old `fixtures.mjs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use deepclean_core::model::{
    ActionMethod, ArtifactKind, CleanAction, CleanableItem, Detail, Ecosystem, RiskLevel,
};
use uuid::Uuid;

static N: AtomicU64 = AtomicU64::new(0);

fn next() -> u64 {
    N.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn action(method: ActionMethod, risk: RiskLevel, bytes: u64) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: "Remove".into(),
        description: String::new(),
        method,
        risk,
        estimated_savings_bytes: bytes,
    }
}

pub fn remove_dir(path: impl AsRef<Path>) -> ActionMethod {
    ActionMethod::RemoveDir {
        path: path.as_ref().to_path_buf(),
    }
}

pub fn trash(path: impl AsRef<Path>) -> ActionMethod {
    ActionMethod::MoveToTrash {
        path: path.as_ref().to_path_buf(),
    }
}

pub fn command(program: &str, args: &[&str]) -> ActionMethod {
    ActionMethod::Command {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        working_dir: None,
    }
}

/// A node_modules item with no actions (informational until given some).
pub fn item() -> CleanableItem {
    let n = next();
    CleanableItem {
        id: Uuid::new_v4(),
        path: PathBuf::from(format!("/Users/dev/item-{n}")),
        ecosystem: Ecosystem::Node,
        kind: ArtifactKind::NodeModules,
        risk: RiskLevel::Safe,
        size_bytes: 100,
        size_display: "100 B".into(),
        last_modified: None,
        days_stale: None,
        project_name: None,
        project_root: None,
        available_actions: vec![],
        details: vec![],
        agent: None,
    }
}

/// An item at `path` of `size` bytes with one safe delete action.
pub fn cleanable(path: &str, size: u64) -> CleanableItem {
    let mut i = item();
    i.path = path.into();
    i.size_bytes = size;
    i.available_actions = vec![action(remove_dir(path), RiskLevel::Safe, size)];
    i
}

/// A permanent delete and its Trash twin, as `trash::add_trash_alternatives`
/// emits them.
pub fn delete_and_trash(path: &str, risk: RiskLevel, bytes: u64) -> Vec<CleanAction> {
    let mut delete = action(remove_dir(path), risk, bytes);
    delete.label = "Delete".into();
    let mut t = action(trash(path), risk, bytes);
    t.label = "Move to Trash".into();
    vec![delete, t]
}

pub fn detail(label: &str, value: &str) -> Detail {
    Detail::new(label, value)
}
