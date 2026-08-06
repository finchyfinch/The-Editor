//! Layered settings, keymap, themes and project detection.
//!
//! Settings resolve as *built-in defaults → user → project*, each layer a TOML
//! file. Reads and writes go through `toml_edit` rather than `toml` so that
//! unknown keys and the user's comments survive a round trip — downgrading The
//! Editor must never silently delete settings written by a newer version.
//! See PLAN.md §3.9.

pub mod paths;
pub mod settings;
pub mod theme;

// Still to come.
//
// pub mod keymap;      // M8: bindings, contexts, chords, conflict detection
// pub mod project;     // M7: ProjectKind detection (Cargo.toml, pyproject.toml)
// pub mod session;     // M1: open tabs, scroll positions, window geometry

/// Application identity. Used for window titles, the About box, and to derive
/// the per-platform config, data and log directory paths.
pub const APP_NAME: &str = "The Editor";
/// Copyright holder, shown in the About box.
pub const APP_AUTHOR: &str = "Gareth Finch";
/// Qualifier/organisation/application triple for the `directories` crate.
pub const APP_DIRS: (&str, &str, &str) = ("", APP_AUTHOR, APP_NAME);
