//! egui widgets for The Editor.
//!
//! This is the only library crate that knows the GUI toolkit exists. Keeping
//! that boundary intact is what makes decision D1 (PLAN.md §1) reversible: if
//! egui ever has to be replaced, only this crate and `the-editor` change.
//!
//! The editor view is a fully custom widget — no toolkit ships a usable code
//! editor — and its correctness rests on virtualised rendering: lay out and
//! paint only the visible line range, so a 200k-line file costs the same per
//! frame as a 50-line one. See PLAN.md §2.4 and the spike in `crates/spike`.

pub mod file_tree;
pub mod tab_bar;
pub mod theme;

// Still to come.
//
// pub mod editor_view; // M2: the custom code editor widget
// pub mod gutter;      // M2: line numbers, diagnostics, folds, modified bar
// pub mod find_bar;    // M5
// pub mod console;     // M7: run output dock
