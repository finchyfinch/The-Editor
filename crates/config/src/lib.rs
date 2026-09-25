//! Layered settings, keymap, themes and project detection.
//!
//! Settings resolve as *built-in defaults → user → project*, each layer a TOML
//! file. Reads and writes go through `toml_edit` rather than `toml` so that
//! unknown keys and the user's comments survive a round trip — downgrading The
//! Editor must never silently delete settings written by a newer version.
//! See PLAN.md §3.9.

pub mod editorconfig;
pub mod paths;
pub mod session;
pub mod settings;
pub mod theme;
pub mod trust;

// Still to come.
//
// pub mod keymap;      // M8: bindings, contexts, chords, conflict detection

/// Application identity. Used for window titles, the About box, and to derive
/// the per-platform config, data and log directory paths.
pub const APP_NAME: &str = "The Editor";
/// Copyright holder, shown in the About box.
pub const APP_AUTHOR: &str = "Gareth Finch";
/// Qualifier/organisation/application triple for the `directories` crate.
pub const APP_DIRS: (&str, &str, &str) = ("", APP_AUTHOR, APP_NAME);
