//! The product's own icon set: 16×16 strokes from the original sprite,
//! embedded so the app renders identically with no files beside it.

use std::borrow::Cow;

use gpui_kit::{svg, AssetSource, Hsla, IntoElement, Pixels, Result, SharedString, Styled};

use deepclean_core::model::Ecosystem;

static ICONS: &[(&str, &[u8])] = &[
    (
        "alert-circle",
        include_bytes!("../../assets/icons/alert-circle.svg"),
    ),
    (
        "arrow-right",
        include_bytes!("../../assets/icons/arrow-right.svg"),
    ),
    ("check", include_bytes!("../../assets/icons/check.svg")),
    (
        "chevron-down",
        include_bytes!("../../assets/icons/chevron-down.svg"),
    ),
    (
        "chevron-right",
        include_bytes!("../../assets/icons/chevron-right.svg"),
    ),
    (
        "chevron-up",
        include_bytes!("../../assets/icons/chevron-up.svg"),
    ),
    ("clock", include_bytes!("../../assets/icons/clock.svg")),
    ("close", include_bytes!("../../assets/icons/close.svg")),
    ("copy", include_bytes!("../../assets/icons/copy.svg")),
    (
        "eco-agent_data",
        include_bytes!("../../assets/icons/eco-agent_data.svg"),
    ),
    (
        "eco-apple",
        include_bytes!("../../assets/icons/eco-apple.svg"),
    ),
    (
        "eco-docker",
        include_bytes!("../../assets/icons/eco-docker.svg"),
    ),
    (
        "eco-dot_net",
        include_bytes!("../../assets/icons/eco-dot_net.svg"),
    ),
    ("eco-go", include_bytes!("../../assets/icons/eco-go.svg")),
    (
        "eco-homebrew",
        include_bytes!("../../assets/icons/eco-homebrew.svg"),
    ),
    (
        "eco-java",
        include_bytes!("../../assets/icons/eco-java.svg"),
    ),
    (
        "eco-jet_brains",
        include_bytes!("../../assets/icons/eco-jet_brains.svg"),
    ),
    (
        "eco-models",
        include_bytes!("../../assets/icons/eco-models.svg"),
    ),
    (
        "eco-node",
        include_bytes!("../../assets/icons/eco-node.svg"),
    ),
    (
        "eco-projects",
        include_bytes!("../../assets/icons/eco-projects.svg"),
    ),
    (
        "eco-python",
        include_bytes!("../../assets/icons/eco-python.svg"),
    ),
    (
        "eco-rust",
        include_bytes!("../../assets/icons/eco-rust.svg"),
    ),
    (
        "eco-system",
        include_bytes!("../../assets/icons/eco-system.svg"),
    ),
    (
        "eco-worktrees",
        include_bytes!("../../assets/icons/eco-worktrees.svg"),
    ),
    (
        "external-link",
        include_bytes!("../../assets/icons/external-link.svg"),
    ),
    ("file", include_bytes!("../../assets/icons/file.svg")),
    ("filter", include_bytes!("../../assets/icons/filter.svg")),
    ("folder", include_bytes!("../../assets/icons/folder.svg")),
    (
        "git-branch",
        include_bytes!("../../assets/icons/git-branch.svg"),
    ),
    (
        "hard-drive",
        include_bytes!("../../assets/icons/hard-drive.svg"),
    ),
    ("history", include_bytes!("../../assets/icons/history.svg")),
    ("info", include_bytes!("../../assets/icons/info.svg")),
    ("lock", include_bytes!("../../assets/icons/lock.svg")),
    ("mark", include_bytes!("../../assets/icons/mark.svg")),
    ("minus", include_bytes!("../../assets/icons/minus.svg")),
    (
        "more-horizontal",
        include_bytes!("../../assets/icons/more-horizontal.svg"),
    ),
    ("plug", include_bytes!("../../assets/icons/plug.svg")),
    ("plus", include_bytes!("../../assets/icons/plus.svg")),
    ("refresh", include_bytes!("../../assets/icons/refresh.svg")),
    ("scan", include_bytes!("../../assets/icons/scan.svg")),
    ("search", include_bytes!("../../assets/icons/search.svg")),
    (
        "settings",
        include_bytes!("../../assets/icons/settings.svg"),
    ),
    (
        "shield-check",
        include_bytes!("../../assets/icons/shield-check.svg"),
    ),
    (
        "sidebar-toggle",
        include_bytes!("../../assets/icons/sidebar-toggle.svg"),
    ),
    ("spin", include_bytes!("../../assets/icons/spin.svg")),
    ("sparkle", include_bytes!("../../assets/icons/sparkle.svg")),
    ("stop", include_bytes!("../../assets/icons/stop.svg")),
    (
        "terminal",
        include_bytes!("../../assets/icons/terminal.svg"),
    ),
    ("trash", include_bytes!("../../assets/icons/trash.svg")),
    ("warning", include_bytes!("../../assets/icons/warning.svg")),
];

/// Serves `icons/<name>.svg` from the table above.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"));
        Ok(name
            .and_then(|n| ICONS.iter().find(|(k, _)| *k == n))
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(n, _)| SharedString::from(format!("icons/{n}.svg")))
            .filter(|p| p.starts_with(path))
            .collect())
    }
}

/// An icon by name, e.g. `icon("scan", px(16.), color)`.
pub fn icon(name: &str, size: Pixels, color: Hsla) -> impl IntoElement {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(size)
        .flex_none()
        .text_color(color)
}

pub fn eco_icon(e: Ecosystem, size: Pixels, color: Hsla) -> impl IntoElement {
    icon(
        &format!("eco-{}", crate::model::labels::eco_id(e)),
        size,
        color,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ecosystem_has_an_icon() {
        for e in Ecosystem::ALL {
            let path = format!("icons/eco-{}.svg", crate::model::labels::eco_id(e));
            assert!(Assets.load(&path).unwrap().is_some(), "{path}");
        }
    }
}
