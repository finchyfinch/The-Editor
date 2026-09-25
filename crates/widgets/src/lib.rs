//! egui widgets for The Editor.
//!
//! This is the only library crate that knows the GUI toolkit exists. Keeping
//! that boundary intact is what makes decision D1 (PLAN.md §1) reversible: if
//! egui ever has to be replaced, only this crate and `the-editor` change.
//!
//! The editor view is a fully custom widget — no toolkit ships a usable code
//! editor — and its correctness rests on virtualised rendering: lay out and
//! paint only the visible line range, so a 200k-line file costs the same per
//! frame as a 50-line one. See PLAN.md §2.4 and `docs/SPIKE-NOTES.md`.

pub mod console;
pub mod editor_view;
pub mod file_tree;
pub mod find_bar;
mod folding;
pub mod glyphs;
pub mod icon;
pub mod tab_bar;
pub mod theme;

// Still to come.
//
