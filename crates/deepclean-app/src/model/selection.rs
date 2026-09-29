//! Selection math that has to survive nesting.
//!
//! AI-era items nest: a stale project contains its `node_modules`, an agent
//! worktree contains its `target/`. Selecting both and adding their sizes
//! counts the inner bytes twice — the headline would promise space that does
//! not exist.

use std::path::Path;

use deepclean_core::model::CleanableItem;

/// Normalise a path for prefix comparison: forward slashes, no trailing slash.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.len() > 1 {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

/// Whether `outer` contains `inner` (or is the same path). Component-wise:
/// `/a/foo` does not contain `/a/foobar`.
pub fn contains_path(outer: &Path, inner: &Path) -> bool {
    let (o, i) = (norm(outer), norm(inner));
    if o.is_empty() || i.is_empty() {
        return false;
    }
    o == i || i.starts_with(&format!("{}/", o.trim_end_matches('/')))
}

/// The items not contained in any other item of the list, in input order.
///
/// An item only swallows another when its own size covers it: a repo's stale
/// worktree records are a few kilobytes "at" the repo root, and letting
/// those hide a 4 GB `node_modules` inside the same repo would undercount.
/// Identical paths keep the first.
pub fn outermost<'a>(items: &[&'a CleanableItem]) -> Vec<&'a CleanableItem> {
    // Sorting on a key where the separator sorts below every other character
    // puts each directory immediately before everything inside it, so one
    // pass with a stack of open ancestors finds every containment in
    // O(n log n).
    let mut order: Vec<(usize, String)> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| (idx, norm(&item.path).replace('/', "\u{0}")))
        .collect();
    order.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    let mut nested = vec![false; items.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (idx, _) in order {
        let item = items[idx];
        while stack
            .last()
            .is_some_and(|&top| !contains_path(&items[top].path, &item.path))
        {
            stack.pop();
        }
        if stack
            .iter()
            .any(|&a| items[a].size_bytes >= item.size_bytes)
        {
            nested[idx] = true;
        }
        stack.push(idx);
    }
    items
        .iter()
        .enumerate()
        .filter(|(i, _)| !nested[*i])
        .map(|(_, item)| *item)
        .collect()
}

/// Bytes across `items`, counting nested ones once.
pub fn total_bytes_of(items: &[&CleanableItem]) -> u64 {
    outermost(items).iter().map(|i| i.size_bytes).sum()
}

/// Path depth, for running nested cleans innermost-first.
pub fn depth(p: &Path) -> usize {
    norm(p).split('/').filter(|s| !s.is_empty()).count()
}

/// Order a batch so nested items run before the item that contains them.
///
/// Removing the inner `node_modules` and then trashing the project works;
/// trashing the project first makes the inner action fail on a missing path.
/// Deeper paths run first; equal depths keep their order (stable sort).
pub fn innermost_first<T>(mut entries: Vec<T>, path_of: impl Fn(&T) -> &Path) -> Vec<T> {
    entries.sort_by_key(|e| std::cmp::Reverse(depth(path_of(e))));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::item;
    use std::path::PathBuf;

    fn it(path: &str, size: u64) -> CleanableItem {
        let mut i = item();
        i.path = path.into();
        i.size_bytes = size;
        i
    }

    fn paths(items: &[&CleanableItem]) -> Vec<String> {
        items.iter().map(|i| i.path.display().to_string()).collect()
    }

    #[test]
    fn contains_path_is_component_wise() {
        let c = |a: &str, b: &str| contains_path(Path::new(a), Path::new(b));
        assert!(c("/a/foo", "/a/foo/bar"));
        assert!(c("/a/foo/", "/a/foo"));
        assert!(!c("/a/foo", "/a/foobar"));
        assert!(!c("/a/foo/bar", "/a/foo"));
        assert!(c("C:\\p\\x", "C:/p/x/y"));
        assert!(!c("", "/a"));
    }

    #[test]
    fn outermost_drops_items_inside_another_item() {
        let project = it("/u/proj", 5000);
        let nm = it("/u/proj/node_modules", 3000);
        let target = it("/u/proj/target", 1000);
        let other = it("/u/other/node_modules", 700);
        let all = [&nm, &project, &other, &target];
        assert_eq!(
            paths(&outermost(&all)),
            ["/u/proj", "/u/other/node_modules"]
        );
        assert_eq!(total_bytes_of(&all), 5700);
    }

    #[test]
    fn siblings_sharing_a_prefix_are_not_nested() {
        let (a, b, c, d) = (
            it("/u/proj", 100),
            it("/u/proj-old", 50),
            it("/u/proj-old/node_modules", 40),
            it("/u/proj/x", 10),
        );
        assert_eq!(
            paths(&outermost(&[&a, &b, &c, &d])),
            ["/u/proj", "/u/proj-old"]
        );
    }

    #[test]
    fn a_container_too_small_to_include_the_inner_item_does_not_swallow_it() {
        let refs = it("/u/repo", 4);
        let nm = it("/u/repo/node_modules", 4000);
        assert_eq!(outermost(&[&refs, &nm]).len(), 2);
        assert_eq!(total_bytes_of(&[&refs, &nm]), 4004);
    }

    #[test]
    fn identical_paths_count_once_keeping_the_first() {
        let a = it("/u/x", 10);
        let b = it("/u/x/", 10);
        let out = outermost(&[&a, &b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, a.id);
    }

    #[test]
    fn deep_nesting_and_empty_input() {
        let chain = [
            it("/a/b/c/d", 1),
            it("/a", 10),
            it("/a/b", 5),
            it("/a/b/c", 2),
        ];
        let refs: Vec<&CleanableItem> = chain.iter().collect();
        assert_eq!(paths(&outermost(&refs)), ["/a"]);
        assert!(outermost(&[]).is_empty());
        assert_eq!(total_bytes_of(&[]), 0);
    }

    #[test]
    fn innermost_first_runs_nested_items_before_their_container() {
        let entries: Vec<PathBuf> = vec![
            "/u/proj".into(),
            "/u/other".into(),
            "/u/proj/node_modules".into(),
        ];
        let out = innermost_first(entries, |p| p.as_path());
        assert_eq!(
            out,
            [
                PathBuf::from("/u/proj/node_modules"),
                "/u/proj".into(),
                "/u/other".into()
            ]
        );
    }

    #[test]
    fn outermost_scales_to_thousands_of_items() {
        let mut many: Vec<CleanableItem> = (0..5000)
            .map(|i| it(&format!("/u/p{i}/node_modules"), 10))
            .collect();
        many.push(it("/u", 1_000_000_000));
        let refs: Vec<&CleanableItem> = many.iter().collect();
        let t = std::time::Instant::now();
        assert_eq!(outermost(&refs).len(), 1);
        assert!(
            t.elapsed().as_millis() < 500,
            "outermost must not be quadratic"
        );
    }
}
