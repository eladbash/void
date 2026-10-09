//! UI logic with no GPUI in it: what to show, in which order, and what a
//! click means. Ported from the old frontend's `js/` modules and tested the
//! same way — without a window.

pub mod actions;
pub mod agents;
pub mod format;
pub mod labels;
pub mod selection;
pub mod ui_state;

#[cfg(test)]
pub mod fixtures;
