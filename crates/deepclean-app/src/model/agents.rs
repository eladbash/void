//! Agents screen helpers.

/// Series slot for a usage category.
///
/// Colour follows the category, never its rank: "Worktrees" is slot 1 in
/// every agent's bar, whether it is that agent's biggest slice or its
/// smallest. Categories nothing here anticipates share the neutral "other"
/// swatch rather than borrowing a hue that means something else.
pub fn category_slot(label: &str) -> Option<u8> {
    let l = label.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| l.contains(w));
    if has(&["worktree"]) {
        Some(1)
    } else if has(&[
        "transcript",
        "session",
        "conversation",
        "history",
        "snapshot",
    ]) {
        Some(2)
    } else if has(&["model", "weights", "blob"]) {
        Some(3)
    } else if has(&["browser"]) {
        Some(4)
    } else if has(&["cache"]) {
        Some(5)
    } else if has(&["log"]) {
        Some(6)
    } else if has(&["branch"]) {
        Some(7)
    } else if has(&["state", "storage", "database", "workspace"]) {
        Some(8)
    } else {
        None
    }
}

/// The Agents screen's "Jump to" hint under each quick filter.
pub fn preset_hint(id: super::ui_state::PresetId, min_days: Option<u64>) -> String {
    use super::ui_state::PresetId::*;
    match id {
        IdleWorktrees => format!("untouched {}+ days", min_days.unwrap_or(0)),
        OldTranscripts => format!("older than {} days", min_days.unwrap_or(0)),
        DuplicateModels => "byte-identical copies".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_colours_follow_the_category_not_its_rank() {
        assert_eq!(category_slot("Worktrees"), Some(1));
        assert_eq!(category_slot("Transcripts"), Some(2));
        assert_eq!(category_slot("Models"), Some(3));
        assert_eq!(category_slot("Something new"), None);
    }
}
