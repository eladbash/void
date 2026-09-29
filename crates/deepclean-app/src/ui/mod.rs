//! The GPUI window: one root view, one screen at a time, overlays on top.

pub mod agents;
pub mod app_view;
pub mod components;
pub mod confirm;
pub mod drawer;
pub mod history;
pub mod icons;
pub mod issues;
pub mod results;
pub mod settings;
pub mod theme;

use gpui_kit::{App, KeyBinding};

gpui_kit::actions!(
    void,
    [
        GoResults,
        GoAgents,
        GoHistory,
        GoSettings,
        Scan,
        Dismiss,
        Review,
        FocusFilter,
        SelectAll,
        SelectNone,
        SelectSafe,
        ToggleDensity,
        ExpandAll,
        FocusNext,
        FocusPrev,
        ToggleFocused,
        OpenFocused,
        DrawerNext,
        DrawerPrev,
        Quit,
    ]
);

/// The window's key context. `LIST` bindings only apply while no text field
/// has focus, so typing a space or an arrow into the filter stays typing.
pub const CONTEXT: &str = "Void";
const LIST: &str = "Void && !Input";

/// The shortcuts of the Tauri build's `keydown` handler. `secondary` is ⌘ on
/// macOS and Ctrl elsewhere.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-1", GoResults, Some(CONTEXT)),
        KeyBinding::new("secondary-2", GoAgents, Some(CONTEXT)),
        KeyBinding::new("secondary-3", GoHistory, Some(CONTEXT)),
        KeyBinding::new("secondary-4", GoSettings, Some(CONTEXT)),
        KeyBinding::new("secondary-,", GoSettings, Some(CONTEXT)),
        KeyBinding::new("secondary-r", Scan, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("secondary-enter", Review, Some(CONTEXT)),
        KeyBinding::new("secondary-f", FocusFilter, Some(CONTEXT)),
        KeyBinding::new("secondary-q", Quit, Some(CONTEXT)),
        KeyBinding::new("/", FocusFilter, Some(LIST)),
        KeyBinding::new("secondary-a", SelectAll, Some(LIST)),
        KeyBinding::new("secondary-shift-a", SelectNone, Some(LIST)),
        KeyBinding::new("secondary-alt-a", SelectSafe, Some(LIST)),
        KeyBinding::new("secondary-d", ToggleDensity, Some(LIST)),
        KeyBinding::new("secondary-shift-e", ExpandAll, Some(LIST)),
        KeyBinding::new("down", FocusNext, Some(LIST)),
        KeyBinding::new("up", FocusPrev, Some(LIST)),
        KeyBinding::new("space", ToggleFocused, Some(LIST)),
        KeyBinding::new("enter", OpenFocused, Some(LIST)),
        KeyBinding::new("alt-down", DrawerNext, Some(LIST)),
        KeyBinding::new("alt-up", DrawerPrev, Some(LIST)),
    ]);
}

#[cfg(test)]
mod tests;
