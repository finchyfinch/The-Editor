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

// M1–M2 populate these.
//
// pub mod editor_view; // the custom code editor widget
// pub mod gutter;      // line numbers, diagnostics, folds, modified bar
// pub mod file_tree;
// pub mod tab_bar;
// pub mod palette;     // command palette / go-to-file / go-to-symbol
// pub mod find_bar;
// pub mod console;     // run output dock
// pub mod status_bar;
