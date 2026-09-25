//! Which project folders may run their own tools.
//!
//! Opening a folder must never run anything it contains — PLAN.md §8 promises
//! that — and some of what an IDE does on opening a folder quietly would. A
//! project's virtual environment was searched ahead of `PATH` for language
//! servers, so a repository shipping `.venv/Scripts/ruff.exe` had it started
//! the moment a Python file was opened. rust-analyzer builds a Rust project's
//! build scripts and procedural macros to understand it, and rustup honours a
//! `rust-toolchain.toml` that names a toolchain by path.
//!
//! So a folder containing any of those is asked about once, and the answer is
//! kept here. Its own file rather than the session: the session is only
//! written on a clean exit, and not at all when session restore is off, and
//! an answer about trust should not depend on either.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, value};

/// Answers given about folders, nearest-ancestor wins.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Trust {
    folders: Vec<(PathBuf, bool)>,
}

impl Trust {
    /// Read the answers. A missing or unreadable file is no answers, which
    /// means every folder is asked about again: the safe way to fail.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(doc) = std::fs::read_to_string(path)
            .unwrap_or_default()
            .parse::<DocumentMut>()
        else {
            return Self::default();
        };
        let read = |key: &str, trusted: bool| -> Vec<(PathBuf, bool)> {
            doc.get(key)
                .and_then(Item::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| (PathBuf::from(s), trusted)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut folders = read("trusted", true);
        folders.extend(read("not_trusted", false));
        Self { folders }
    }

    /// Write the answers.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut doc = DocumentMut::new();
        doc.decor_mut().set_prefix(
            "# Folders The Editor may, or may not, run tools from.\n\
             # Delete a line to be asked about that folder again.\n\n",
        );
        for (key, wanted) in [("trusted", true), ("not_trusted", false)] {
            let mut array = toml_edit::Array::new();
            for (folder, trusted) in &self.folders {
                if *trusted == wanted {
                    array.push(folder.display().to_string());
                }
            }
            doc[key] = value(array);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, doc.to_string())
    }

    /// What was said about `folder`, or the nearest folder containing it:
    /// trusting a repository trusts opening one of its crates on its own.
    #[must_use]
    pub fn decision(&self, folder: &Path) -> Option<bool> {
        self.folders
            .iter()
            .filter(|(recorded, _)| folder.starts_with(recorded))
            .max_by_key(|(recorded, _)| recorded.components().count())
            .map(|(_, trusted)| *trusted)
    }

    /// Record an answer, replacing any earlier one for the same folder.
    pub fn decide(&mut self, folder: &Path, trusted: bool) {
        self.folders.retain(|(recorded, _)| recorded != folder);
        self.folders.push((folder.to_path_buf(), trusted));
    }
}

/// Whether opening `folder` would run something it contains, and so whether
/// it needs asking about. Folders with nothing of the kind are never asked.
#[must_use]
pub fn needs_asking(folder: &Path) -> bool {
    const RUNS_SOMETHING: &[&str] = &[
        // A virtual environment's own executables.
        ".venv",
        "venv",
        "env",
        // rust-analyzer runs build scripts and procedural macros.
        "Cargo.toml",
        // rustup runs whatever toolchain these name.
        "rust-toolchain.toml",
        "rust-toolchain",
    ];
    RUNS_SOMETHING.iter().any(|name| folder.join(name).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_nobody_was_asked_about_has_no_answer() {
        assert_eq!(Trust::default().decision(Path::new("/p")), None);
    }

    #[test]
    fn the_nearest_answer_wins() {
        let mut trust = Trust::default();
        trust.decide(Path::new("/work"), true);
        trust.decide(Path::new("/work/downloaded"), false);
        assert_eq!(trust.decision(Path::new("/work/mine")), Some(true));
        assert_eq!(
            trust.decision(Path::new("/work/downloaded/sub")),
            Some(false)
        );
        assert_eq!(trust.decision(Path::new("/elsewhere")), None);
    }

    #[test]
    fn a_new_answer_replaces_the_old_one() {
        let mut trust = Trust::default();
        trust.decide(Path::new("/p"), false);
        trust.decide(Path::new("/p"), true);
        assert_eq!(trust.decision(Path::new("/p")), Some(true));
    }

    #[test]
    fn answers_survive_a_round_trip() {
        let dir = std::env::temp_dir().join("the-editor-trust");
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("trust.toml");
        let mut trust = Trust::default();
        trust.decide(Path::new("/yes"), true);
        trust.decide(Path::new("/no"), false);
        trust.save(&file).expect("save");
        assert_eq!(Trust::load(&file), trust);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_asks_about_everything() {
        let trust = Trust::load(Path::new("/does/not/exist/trust.toml"));
        assert_eq!(trust.decision(Path::new("/p")), None);
    }

    #[test]
    fn only_a_folder_that_would_run_something_needs_asking() {
        let dir = std::env::temp_dir().join("the-editor-trust-ask");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("notes.md"), "hi").expect("write");
        assert!(!needs_asking(&dir), "notes run nothing");
        std::fs::create_dir_all(dir.join(".venv")).expect("venv");
        assert!(needs_asking(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
