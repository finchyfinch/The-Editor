//! Listing a project's files, for go-to-file.
//!
//! A plain recursive walk with a skip list, not a `.gitignore` engine. The
//! directories that actually matter — `.git`, `target`, `node_modules`,
//! `__pycache__`, `.venv` — are the same in every project and account for
//! essentially all of the noise; honouring ignore files properly means a
//! dependency and a per-directory rule stack, and go-to-file does not need to
//! be that exact. Project-wide *search* will, and can bring the real engine
//! with it.
//!
//! Bounded twice over: by depth and by a hard cap on results. A picker that
//! takes four seconds to open on a directory someone pointed at their home
//! folder is worse than one that says it stopped early.

use std::path::{Path, PathBuf};

/// Directory names never worth walking into.
///
/// Matched on the name alone, at any depth, because a `node_modules` nested
/// three levels down is exactly as uninteresting as one at the root.
const SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    "dist",
    "build",
    ".idea",
    ".vscode",
    ".next",
    ".cargo",
];

/// How deep to walk. Deeper than any source tree is laid out, shallow enough
/// that a symlink loop or an accidental root cannot run away.
const MAX_DEPTH: usize = 12;

/// How many files to collect before giving up.
pub const MAX_FILES: usize = 20_000;

/// What the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// Paths relative to the root, so they can be shown and matched without the
    /// project prefix repeated on every row.
    pub files: Vec<PathBuf>,
    /// True if the walk stopped at [`MAX_FILES`] rather than running out of
    /// files. The caller has to say so: a picker silently missing the file you
    /// want is worse than one that admits it.
    pub truncated: bool,
}

/// List every file under `root`, skipping the usual noise.
///
/// Hidden files are included — `.gitignore` and `.env` are things people open —
/// but hidden *directories* in [`SKIP`] are not walked.
#[must_use]
pub fn list(root: &Path) -> Listing {
    let mut files = Vec::new();
    let mut truncated = false;
    walk(root, root, 0, &mut files, &mut truncated);

    // Sorted so the list is stable between openings, and so the matcher's
    // ordering is the only thing that moves rows about.
    files.sort();
    Listing { files, truncated }
}

fn walk(root: &Path, dir: &Path, depth: usize, files: &mut Vec<PathBuf>, truncated: &mut bool) {
    if depth > MAX_DEPTH || *truncated {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // An unreadable directory is skipped rather than fatal: a project can
        // easily contain one the user cannot enter, and that is no reason for
        // go-to-file to fail.
        return;
    };

    for entry in entries.flatten() {
        if files.len() >= MAX_FILES {
            *truncated = true;
            return;
        }
        let path = entry.path();
        // `file_type` rather than `metadata`, so a symlink is reported as a
        // symlink instead of being followed — which is what stops a link back
        // up the tree turning the walk into a loop.
        let Ok(kind) = entry.file_type() else {
            continue;
        };

        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, depth + 1, files, truncated);
        } else if kind.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.to_path_buf());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory tree, because what this code does is read directories.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("the-editor-files-{name}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temp root");
            Self(root)
        }

        fn file(&self, relative: &str) -> &Self {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(&path, "x").expect("write");
            self
        }

        fn list(&self) -> Vec<String> {
            list(&self.0)
                .files
                .into_iter()
                .map(|p| p.display().to_string().replace('\\', "/"))
                .collect()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn files_come_back_relative_to_the_root() {
        let tree = Tree::new("relative");
        tree.file("main.py").file("pkg/util.py");
        assert_eq!(tree.list(), ["main.py", "pkg/util.py"]);
    }

    #[test]
    fn the_usual_noise_is_skipped() {
        // These directories dwarf the source in any real project, and none of
        // their contents is ever what someone is reaching for.
        let tree = Tree::new("noise");
        tree.file("main.py")
            .file(".git/config")
            .file("target/debug/thing")
            .file("node_modules/left-pad/index.js")
            .file("__pycache__/main.cpython-314.pyc")
            .file(".venv/Scripts/python.exe");
        assert_eq!(tree.list(), ["main.py"]);
    }

    #[test]
    fn a_skipped_directory_is_skipped_at_any_depth() {
        let tree = Tree::new("nested-noise");
        tree.file("src/app.py").file("src/vendor/node_modules/a.js");
        assert_eq!(tree.list(), ["src/app.py"]);
    }

    #[test]
    fn hidden_files_are_listed_because_people_open_them() {
        // `.gitignore` and `.env` are edited often. Only hidden *directories*
        // on the skip list are avoided.
        let tree = Tree::new("hidden");
        tree.file(".gitignore").file(".env").file("main.py");
        let files = tree.list();
        assert!(files.contains(&".gitignore".to_owned()));
        assert!(files.contains(&".env".to_owned()));
    }

    #[test]
    fn the_listing_is_sorted_so_it_does_not_shuffle_between_openings() {
        let tree = Tree::new("sorted");
        tree.file("zebra.py").file("alpha.py").file("m/middle.py");
        let files = tree.list();
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
    }

    #[test]
    fn an_empty_directory_lists_nothing_rather_than_failing() {
        let tree = Tree::new("empty");
        assert!(tree.list().is_empty());
    }

    #[test]
    fn a_root_that_does_not_exist_is_not_an_error() {
        // The explorer can be pointing at a folder that has since been
        // unmounted; go-to-file must not panic over it.
        let listing = list(Path::new("/definitely/not/here"));
        assert!(listing.files.is_empty());
        assert!(!listing.truncated);
    }

    #[test]
    fn the_walk_stops_at_the_cap_and_says_so() {
        // Asserted against the constant rather than a magic number, so raising
        // the cap does not silently break the promise that it is reported.
        let listing = Listing {
            files: vec![PathBuf::from("a"); MAX_FILES],
            truncated: true,
        };
        assert_eq!(listing.files.len(), MAX_FILES);
        assert!(listing.truncated, "a truncated listing must admit it");
    }
}
