//! Finding the repository, and asking git about it.
//!
//! Everything here shells out to `git`. That is deliberate: the user's own git
//! does the work, so their configuration, credential helper, hooks, ignore
//! rules and signing key all apply without being reimplemented. It also means
//! this works with whatever git they have, including one wrapped by a corporate
//! policy that a library would not know about.
//!
//! Nothing here blocks the frame loop. A `git` invocation is a process start,
//! which is milliseconds at best and seconds on a cold cache or a network
//! drive, so the callers run these on a worker thread and hold the answers.

use std::path::{Path, PathBuf};

/// A git repository, and where its top level is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The working tree's top level, as git reports it.
    pub root: PathBuf,
}

impl Repo {
    /// Find the repository containing `path`, if there is one.
    ///
    /// Asks git rather than looking for a `.git` directory. A worktree's `.git`
    /// is a *file*, a submodule's points elsewhere, and `GIT_DIR` overrides
    /// both — all cases where walking up the tree gets the wrong answer or no
    /// answer at all.
    #[must_use]
    pub fn discover(path: &Path) -> Option<Self> {
        let start = if path.is_dir() { path } else { path.parent()? };
        let output = run(start, &["rev-parse", "--show-toplevel"]).ok()?;
        let root = output.trim();
        (!root.is_empty()).then(|| Self {
            // Git reports forward slashes even on Windows; `PathBuf` is happy
            // with those, and normalising them would only make the strings
            // disagree with what git prints elsewhere.
            root: PathBuf::from(root),
        })
    }

    /// The branch being worked on, or a short commit id when detached.
    ///
    /// `None` only when git cannot answer at all — a repository with no commits
    /// yet reports its unborn branch, which is worth showing.
    #[must_use]
    pub fn branch(&self) -> Option<String> {
        // `--show-current` is empty on a detached HEAD, which is exactly when
        // the commit is the useful thing to show instead.
        if let Ok(name) = run(&self.root, &["branch", "--show-current"]) {
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
        if let Ok(short) = run(&self.root, &["rev-parse", "--short", "HEAD"]) {
            let short = short.trim();
            if !short.is_empty() {
                return Some(format!("detached at {short}"));
            }
        }
        // No commits yet: `symbolic-ref` still knows what branch is being made.
        let unborn = run(&self.root, &["symbolic-ref", "--short", "HEAD"]).ok()?;
        let unborn = unborn.trim();
        (!unborn.is_empty()).then(|| unborn.to_owned())
    }

    /// The commit HEAD points at.
    ///
    /// The cheapest question that answers "is everything I cached still
    /// current?" — a commit, a checkout, a pull, a rebase all change it, and
    /// nothing else does. `None` before the first commit.
    #[must_use]
    pub fn head_id(&self) -> Option<String> {
        let id = run(&self.root, &["rev-parse", "HEAD"]).ok()?;
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_owned())
    }

    /// The committed contents of `path` at HEAD.
    ///
    /// `None` for a file git does not have at HEAD — one newly added, or
    /// ignored, or outside the repository. The caller treats that as "no
    /// baseline", which is right: every line of a new file is new, but marking
    /// all of them says nothing.
    #[must_use]
    pub fn head_contents(&self, path: &Path) -> Option<String> {
        let relative = self.relative(path)?;
        // `--` guards against a path that looks like a revision. A file called
        // `HEAD` is unusual and entirely legal.
        run(&self.root, &["show", &format!("HEAD:{relative}"), "--"])
            .ok()
            .or_else(|| {
                // Older git rejects the trailing `--` for this form.
                run(&self.root, &["show", &format!("HEAD:{relative}")]).ok()
            })
    }

    /// `path` as git spells it: relative to the top level, forward slashes.
    #[must_use]
    pub fn relative(&self, path: &Path) -> Option<String> {
        let path = path.strip_prefix(&self.root).ok()?;
        let text = path.to_str()?.replace('\\', "/");
        (!text.is_empty()).then_some(text)
    }
}

/// Run `git` in `cwd` and return its standard output.
///
/// # Errors
/// If git cannot be run, or exits non-zero — which for the queries here means
/// "no", not "something broke": `git show HEAD:new-file` fails because the file
/// is not in HEAD.
fn run(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = editor_proc::spawn::quiet("git");
    command.current_dir(cwd);
    // Configuration comes before the subcommand — git reads `-c` as its own
    // option, and anything after the subcommand belongs to the subcommand.
    // Colour codes in a string being parsed are noise.
    command.args(["-c", "color.ui=false"]);
    command.args(args);
    // A pager attached to a captured pipe would hang, and a credential prompt
    // on a terminal that is not there would hang for longer.
    command.env("GIT_PAGER", "cat");
    command.env("GIT_TERMINAL_PROMPT", "0");

    let output = command.output().map_err(|e| format!("running git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether git can be found at all.
///
/// Asked once, so the interface can say "git is not installed" rather than
/// silently showing a repository as having no changes.
#[must_use]
pub fn is_available() -> bool {
    editor_proc::interpreter::which("git").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This project is a git repository, which makes it the test fixture.
    fn here() -> Option<Repo> {
        Repo::discover(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    #[test]
    fn git_is_available_in_this_environment() {
        assert!(is_available(), "these tests need git on PATH");
    }

    #[test]
    fn the_repository_containing_this_crate_is_found() {
        let repo = here().expect("this crate is inside a repository");
        assert!(
            repo.root.join(".git").exists(),
            "got {}",
            repo.root.display()
        );
    }

    #[test]
    fn a_directory_outside_any_repository_reports_none() {
        let outside = std::env::temp_dir();
        // The temporary directory is not in a repository on any sane machine;
        // if it were, this would be a false failure worth knowing about.
        if Repo::discover(&outside).is_some() {
            eprintln!("skipping: {} is inside a repository", outside.display());
            return;
        }
        assert_eq!(Repo::discover(&outside), None);
    }

    #[test]
    fn the_branch_has_a_name() {
        let repo = here().expect("repository");
        let branch = repo.branch().expect("a branch or a commit");
        assert!(!branch.is_empty());
    }

    /// The committed text of a file that is definitely committed.
    #[test]
    fn head_contents_returns_the_committed_version() {
        let repo = here().expect("repository");
        let manifest = repo.root.join("Cargo.toml");
        let committed = repo
            .head_contents(&manifest)
            .expect("Cargo.toml is committed");
        assert!(
            committed.contains("[workspace]"),
            "got {:?}",
            &committed[..committed.len().min(80)]
        );
    }

    /// A file git has never seen has no baseline, and saying so is the point:
    /// every line of it is new, and marking all of them says nothing.
    #[test]
    fn a_file_not_in_head_has_no_committed_version() {
        let repo = here().expect("repository");
        let absent = repo.root.join("this-file-has-never-existed-xyzzy.txt");
        assert_eq!(repo.head_contents(&absent), None);
    }

    #[test]
    fn paths_are_made_relative_the_way_git_spells_them() {
        let repo = Repo {
            root: PathBuf::from(if cfg!(windows) {
                r"C:\projects\thing"
            } else {
                "/projects/thing"
            }),
        };
        let inside = repo.root.join("crates").join("core").join("lib.rs");
        assert_eq!(
            repo.relative(&inside).as_deref(),
            Some("crates/core/lib.rs"),
            "forward slashes, relative to the top level"
        );
    }

    #[test]
    fn a_path_outside_the_repository_has_no_relative_form() {
        let repo = Repo {
            root: PathBuf::from(if cfg!(windows) {
                r"C:\projects\thing"
            } else {
                "/projects/thing"
            }),
        };
        let outside = PathBuf::from(if cfg!(windows) {
            r"C:\elsewhere\other.rs"
        } else {
            "/elsewhere/other.rs"
        });
        assert_eq!(repo.relative(&outside), None);
        assert_eq!(
            repo.relative(&repo.root),
            None,
            "the root itself is not a file"
        );
    }
}
