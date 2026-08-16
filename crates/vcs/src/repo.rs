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

    /// The state of the working tree, file by file.
    ///
    /// # Errors
    /// Whatever git said, verbatim.
    pub fn status(&self) -> Result<crate::status::Status, String> {
        // `--untracked-files=normal` lists new files but collapses a wholly new
        // directory to the directory itself, which is what the panel wants: a
        // freshly cloned `node_modules` should be one row, not forty thousand.
        //
        // `--no-renames` is deliberately *not* passed. A rename shown as a
        // delete plus an add is two rows describing one thing.
        let output = self.run(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"])?;
        Ok(crate::status::parse(&output))
    }

    /// Whether HEAD points at a commit yet.
    ///
    /// A repository with nothing committed cannot be asked half the questions
    /// git normally answers, and unstaging in particular has to be done a
    /// different way there.
    #[must_use]
    pub fn has_commits(&self) -> bool {
        self.run(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .is_ok_and(|id| !id.trim().is_empty())
    }

    /// Add `paths` to the index.
    ///
    /// # Errors
    /// Whatever git said, verbatim.
    pub fn stage(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        // `--all` so that a deleted file is staged *as* a deletion. Plain
        // `git add` on a path that no longer exists is an error in older git
        // and a no-op in newer, and neither is what the button says it does.
        self.run_with_paths(&["add", "--all", "--"], paths)
            .map(drop)
    }

    /// Take `paths` back out of the index, leaving the working tree alone.
    ///
    /// # Errors
    /// Whatever git said, verbatim.
    pub fn unstage(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        if self.has_commits() {
            // `reset` rather than `restore --staged`: the latter needs git
            // 2.23, and this one has meant the same thing for twenty years.
            self.run_with_paths(&["reset", "--quiet", "HEAD", "--"], paths)
                .map(drop)
        } else {
            // Before the first commit there is no HEAD to reset to, and the
            // only way back out of the index is to leave it.
            self.run_with_paths(&["rm", "--cached", "--quiet", "-r", "--"], paths)
                .map(drop)
        }
    }

    /// Throw away the unstaged changes to `paths`.
    ///
    /// **This destroys work that exists nowhere else.** Unlike everything else
    /// here it cannot be undone by git — the discarded text was never committed,
    /// never stashed, and is not in the reflog. Callers must confirm with the
    /// user first, naming the files.
    ///
    /// Untracked files are left alone. Deleting a file git has never seen is
    /// even less recoverable, and "discard changes" is not what anyone means by
    /// it.
    ///
    /// # Errors
    /// Whatever git said, verbatim.
    pub fn discard(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        self.run_with_paths(&["checkout", "--quiet", "--"], paths)
            .map(drop)
    }

    /// Commit whatever is staged.
    ///
    /// Hooks run, because it is the user's own git doing the work — a
    /// `pre-commit` that formats the tree or refuses the change behaves exactly
    /// as it does on the command line, and its output comes back in the error.
    /// Signing likewise: a configured key signs this commit as it would any
    /// other.
    ///
    /// # Errors
    /// Whatever git said, verbatim — including "nothing to commit", which is a
    /// perfectly ordinary thing for it to say.
    pub fn commit(&self, message: &str, amend: bool) -> Result<String, String> {
        if message.trim().is_empty() {
            // Git would accept `-m ""` for an amend and silently keep the old
            // message, which is not what an empty box means.
            return Err("A commit needs a message.".to_owned());
        }
        let mut args = vec!["commit", "--quiet"];
        if amend {
            args.push("--amend");
        }
        args.push("-m");
        args.push(message);
        self.run(&args)
    }

    /// The message of the commit HEAD is on.
    ///
    /// What an amend starts from: amending is editing that commit, so offering
    /// a blank box would invite replacing a good message with a hurried one.
    #[must_use]
    pub fn last_message(&self) -> Option<String> {
        let text = self.run(&["log", "-1", "--pretty=%B"]).ok()?;
        let text = text.trim_end();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// The most recent commits, newest first.
    ///
    /// `path` narrows it to one file's history. `limit` is a bound on how many
    /// to read, because a repository's log is unbounded and a panel showing all
    /// of it would spend its time formatting rows nobody scrolls to.
    ///
    /// # Errors
    /// Whatever git said. A repository with no commits yet is *not* an error
    /// here: it has an empty history, which is a true answer.
    pub fn log(&self, limit: usize, path: Option<&str>) -> Result<Vec<crate::log::Commit>, String> {
        if !self.has_commits() {
            return Ok(Vec::new());
        }
        let count = limit.to_string();
        let format = format!("--format={}", crate::log::FORMAT);
        let mut args = vec!["log", "--max-count", &count, &format];
        if let Some(path) = path {
            // `--follow` tracks the file through renames, which is what anyone
            // asking for one file's history wants. It only works for a single
            // path, which is all this takes.
            args.push("--follow");
            args.push("--");
            args.push(path);
        }
        Ok(crate::log::parse(&self.run(&args)?))
    }

    /// One commit in full: its message, and the files it touched.
    ///
    /// # Errors
    /// Whatever git said — including that there is no such object.
    pub fn show(&self, id: &str) -> Result<crate::log::Detail, String> {
        // Two calls rather than one, because the message is free-form text and
        // the file list is NUL-separated records: reading both out of one
        // stream means agreeing on a boundary that a message could contain.
        let message = self.run(&["show", "--no-patch", "--format=%B", id])?;
        // `--format=` with nothing after it suppresses the header entirely, so
        // what comes back is only the records.
        let files = self.run(&["show", "--name-status", "-z", "--format=", id])?;
        Ok(crate::log::Detail {
            message: message.trim_end().to_owned(),
            files: crate::log::changed_files(&files),
        })
    }

    /// Who last touched each line of `path`, and when.
    ///
    /// # Errors
    /// Whatever git said — including that the file is not tracked, which is the
    /// answer for anything new.
    pub fn blame(&self, path: &str) -> Result<Vec<crate::blame::Line>, String> {
        // The porcelain format is the one meant for machines: stable, and it
        // states each commit's details once rather than per line.
        let output = self.run(&["blame", "--porcelain", "--", path])?;
        Ok(crate::blame::parse(&output))
    }

    /// Run git with a fixed set of arguments and then a list of paths.
    ///
    /// Separate from [`run`] because the paths are owned strings from the
    /// status listing rather than literals, and because they must always come
    /// after a `--` so a file named like an option is still just a file.
    fn run_with_paths(&self, args: &[&str], paths: &[String]) -> Result<String, String> {
        let mut all: Vec<&str> = args.to_vec();
        all.extend(paths.iter().map(String::as_str));
        self.run(&all)
    }

    /// Run git in this repository.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        run(&self.root, args)
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
        // Both streams, because a failing *hook* is a common way for a commit
        // to be refused and hooks print to stdout as often as to stderr.
        // Reporting only stderr there leaves "the commit failed" with no reason
        // attached, which is the least useful thing an editor can say.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = match (stderr.trim(), stdout.trim()) {
            ("", "") => format!("git exited with {}", output.status),
            (err, "") => err.to_owned(),
            ("", out) => out.to_owned(),
            (err, out) => format!("{err}\n{out}"),
        };
        return Err(message);
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

    /// A repository of its own, in a temporary directory, deleted afterwards.
    ///
    /// Every test that *changes* anything gets one of these. Running staging
    /// tests against this project's own repository would stage this project's
    /// own files, which is not a thing a test may do.
    struct Fixture {
        repo: Repo,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.repo.root);
        }
    }

    impl Fixture {
        /// An empty repository on a branch called `trial`, and nothing else.
        fn bare(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "the-editor-vcs-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temporary directory");

            let git = |args: &[&str]| {
                run(&root, args).unwrap_or_else(|e| panic!("git {args:?} in the fixture: {e}"));
            };
            git(&["init", "--quiet", "-b", "trial"]);
            git(&["config", "user.email", "test@example.invalid"]);
            git(&["config", "user.name", "Test"]);
            // A signing key the machine running the tests may or may not have
            // would fail every commit here for reasons nothing to do with the
            // test.
            git(&["config", "commit.gpgsign", "false"]);
            // Otherwise checking a file out rewrites its line endings, and a
            // test comparing restored text against what was written fails on
            // Windows and passes everywhere else.
            git(&["config", "core.autocrlf", "false"]);

            // Discovered rather than assumed: git reports the top level through
            // its own eyes, and on Windows the temporary directory often comes
            // back spelled differently from the path handed to `create_dir_all`.
            let repo = Repo::discover(&root).expect("the fixture is a repository");
            Self { repo }
        }

        /// A repository with `first.txt` committed.
        fn new(name: &str) -> Self {
            let fixture = Self::bare(name);
            fixture.write("first.txt", "one\ntwo\n");
            let root = &fixture.repo.root;
            run(root, &["add", "."]).expect("add");
            run(root, &["commit", "--quiet", "-m", "initial"]).expect("commit");
            fixture
        }

        /// The same, with nothing committed yet.
        fn unborn(name: &str) -> Self {
            Self::bare(name)
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.repo.root.join(name), text).expect("write");
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.repo.root.join(name)).expect("read")
        }

        fn status(&self) -> crate::status::Status {
            self.repo.status().expect("status")
        }
    }

    #[test]
    fn a_fresh_fixture_is_clean() {
        let fixture = Fixture::new("clean");
        assert!(fixture.status().is_clean());
        assert!(fixture.repo.has_commits());
        assert_eq!(fixture.repo.branch().as_deref(), Some("trial"));
    }

    #[test]
    fn an_edit_shows_as_unstaged_and_staging_moves_it_across() {
        let fixture = Fixture::new("stage");
        fixture.write("first.txt", "one\nCHANGED\n");

        let before = fixture.status();
        assert_eq!(before.unstaged().count(), 1);
        assert_eq!(before.staged().count(), 0);

        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("staging");

        let after = fixture.status();
        assert_eq!(after.staged().count(), 1);
        assert_eq!(after.unstaged().count(), 0);
    }

    #[test]
    fn unstaging_puts_it_back_without_touching_the_file() {
        let fixture = Fixture::new("unstage");
        fixture.write("first.txt", "one\nCHANGED\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        fixture
            .repo
            .unstage(&["first.txt".to_owned()])
            .expect("unstage");

        let status = fixture.status();
        assert_eq!(status.staged().count(), 0);
        assert_eq!(status.unstaged().count(), 1);
        assert_eq!(
            fixture.read("first.txt"),
            "one\nCHANGED\n",
            "unstaging is about the index, and must not touch the working tree"
        );
    }

    #[test]
    fn a_new_file_is_untracked_until_it_is_staged() {
        let fixture = Fixture::new("untracked");
        fixture.write("second.txt", "new\n");

        let before = fixture.status();
        assert_eq!(before.untracked().count(), 1);
        assert_eq!(before.staged().count(), 0);

        fixture
            .repo
            .stage(&["second.txt".to_owned()])
            .expect("stage");
        let after = fixture.status();
        assert_eq!(after.untracked().count(), 0);
        assert_eq!(after.staged().count(), 1);
    }

    /// The case `--all` is passed for: a deletion has to be staged *as* a
    /// deletion, and plain `git add` on a path that is gone does not do it.
    #[test]
    fn deleting_a_file_can_be_staged() {
        let fixture = Fixture::new("delete");
        std::fs::remove_file(fixture.repo.root.join("first.txt")).expect("remove");
        assert_eq!(fixture.status().unstaged().count(), 1);

        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("staging a deletion");
        let status = fixture.status();
        assert_eq!(status.staged().count(), 1);
        assert_eq!(status.entries[0].index, crate::status::Change::Deleted);
    }

    /// Before the first commit there is no HEAD to reset against, and unstaging
    /// has to take a different route entirely.
    #[test]
    fn unstaging_works_before_the_first_commit() {
        let fixture = Fixture::unborn("unborn");
        assert!(!fixture.repo.has_commits());

        fixture.write("only.txt", "hello\n");
        fixture.repo.stage(&["only.txt".to_owned()]).expect("stage");
        assert_eq!(fixture.status().staged().count(), 1);

        fixture
            .repo
            .unstage(&["only.txt".to_owned()])
            .expect("unstaging with no HEAD to reset to");
        let status = fixture.status();
        assert_eq!(status.staged().count(), 0);
        assert_eq!(status.untracked().count(), 1);
        assert_eq!(fixture.read("only.txt"), "hello\n", "the file survives");
    }

    /// A repository with no commits still has a branch to name.
    #[test]
    fn an_unborn_branch_still_has_a_name() {
        let fixture = Fixture::unborn("unborn-branch");
        assert_eq!(fixture.repo.branch().as_deref(), Some("trial"));
        assert_eq!(fixture.repo.head_id(), None);
    }

    #[test]
    fn discarding_restores_the_committed_text() {
        let fixture = Fixture::new("discard");
        fixture.write("first.txt", "wrecked\n");
        fixture
            .repo
            .discard(&["first.txt".to_owned()])
            .expect("discard");

        assert_eq!(fixture.read("first.txt"), "one\ntwo\n");
        assert!(fixture.status().is_clean());
    }

    /// "Discard changes" must not mean "delete a file git has never seen".
    #[test]
    fn discarding_leaves_untracked_files_alone() {
        let fixture = Fixture::new("discard-untracked");
        fixture.write("second.txt", "mine\n");
        // git refuses the path outright, which is the safe failure; either way
        // the file must still be there afterwards.
        let _ = fixture.repo.discard(&["second.txt".to_owned()]);
        assert_eq!(fixture.read("second.txt"), "mine\n");
    }

    #[test]
    fn staging_nothing_does_nothing_rather_than_everything() {
        let fixture = Fixture::new("empty-list");
        fixture.write("first.txt", "changed\n");
        // `git add --all --` with no paths would stage the whole tree, which is
        // emphatically not what an empty selection means.
        fixture.repo.stage(&[]).expect("no-op");
        assert_eq!(fixture.status().staged().count(), 0);
        fixture.repo.unstage(&[]).expect("no-op");
        fixture.repo.discard(&[]).expect("no-op");
        assert_eq!(fixture.read("first.txt"), "changed\n");
    }

    #[test]
    fn several_files_move_together() {
        let fixture = Fixture::new("several");
        fixture.write("first.txt", "edited\n");
        fixture.write("second.txt", "new\n");
        fixture.write("third.txt", "also new\n");

        fixture
            .repo
            .stage(&[
                "first.txt".to_owned(),
                "second.txt".to_owned(),
                "third.txt".to_owned(),
            ])
            .expect("stage");
        assert_eq!(fixture.status().staged().count(), 3);
    }

    // ---- committing, history and blame ----------------------------------
    //
    // The parsers have their own tests against handwritten input. These check
    // the same parsers against what git *actually* writes, which is the half a
    // handwritten fixture cannot prove.

    #[test]
    fn committing_what_is_staged_leaves_a_clean_tree() {
        let fixture = Fixture::new("commit");
        fixture.write("first.txt", "one\nCHANGED\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");

        fixture
            .repo
            .commit("Change the first file", false)
            .expect("commit");

        assert!(fixture.status().is_clean());
        assert_eq!(
            fixture.repo.last_message().as_deref(),
            Some("Change the first file")
        );
    }

    #[test]
    fn committing_with_nothing_staged_says_so_rather_than_pretending() {
        let fixture = Fixture::new("commit-empty");
        let outcome = fixture.repo.commit("nothing to say", false);
        assert!(outcome.is_err(), "git should refuse, and did not");
    }

    #[test]
    fn a_commit_needs_a_message() {
        let fixture = Fixture::new("commit-blank");
        fixture.write("first.txt", "changed\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        // Refused here rather than by git, which would take `-m ""` for an
        // amend and quietly keep the old message.
        assert!(fixture.repo.commit("   \n ", false).is_err());
    }

    #[test]
    fn amending_replaces_the_last_commit_rather_than_adding_one() {
        let fixture = Fixture::new("amend");
        let before = fixture.repo.log(100, None).expect("log").len();

        fixture.write("first.txt", "one\nCHANGED\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        fixture.repo.commit("first go", false).expect("commit");

        fixture.write("second.txt", "more\n");
        fixture
            .repo
            .stage(&["second.txt".to_owned()])
            .expect("stage");
        fixture
            .repo
            .commit("second thoughts", true)
            .expect("amending");

        let log = fixture.repo.log(100, None).expect("log");
        assert_eq!(
            log.len(),
            before + 1,
            "one commit was added and then rewritten, not two"
        );
        assert_eq!(log[0].subject, "second thoughts");
        assert!(fixture.status().is_clean(), "both files went in");
    }

    /// A multi-line message must survive intact — the body is where the reason
    /// for a change lives, and losing it is losing the point of committing.
    #[test]
    fn a_message_keeps_its_body() {
        let fixture = Fixture::new("commit-body");
        fixture.write("first.txt", "changed\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");

        let message =
            "A subject line\n\nAnd a body, with a blank line above it\nand two lines in it.";
        fixture.repo.commit(message, false).expect("commit");

        assert_eq!(fixture.repo.last_message().as_deref(), Some(message));
        assert_eq!(
            fixture.repo.log(1, None).expect("log")[0].subject,
            "A subject line",
            "the log shows the subject alone"
        );
    }

    /// The separators exist because these characters are ordinary in a subject.
    #[test]
    fn a_subject_full_of_punctuation_comes_back_unmangled() {
        let fixture = Fixture::new("commit-punctuation");
        let subject = "Fix --format=%H | don't \"quote\" me\ttabbed";
        fixture.write("first.txt", "changed\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        fixture.repo.commit(subject, false).expect("commit");

        assert_eq!(fixture.repo.log(1, None).expect("log")[0].subject, subject);
    }

    #[test]
    fn the_log_is_newest_first_and_bounded() {
        let fixture = Fixture::new("log-order");
        for n in 1..=5 {
            fixture.write("first.txt", &format!("version {n}\n"));
            fixture
                .repo
                .stage(&["first.txt".to_owned()])
                .expect("stage");
            fixture
                .repo
                .commit(&format!("change {n}"), false)
                .expect("commit");
        }

        let log = fixture.repo.log(3, None).expect("log");
        assert_eq!(log.len(), 3, "the limit is a limit");
        assert_eq!(log[0].subject, "change 5", "newest first");
        assert_eq!(log[2].subject, "change 3");
        assert!(!log[0].id.is_empty() && log[0].short.len() >= 4);
        assert!(!log[0].relative.is_empty(), "git phrases the age for us");
        assert_eq!(log[0].date().len(), 10, "an ISO date, ten characters");
    }

    #[test]
    fn one_files_history_leaves_out_the_others() {
        let fixture = Fixture::new("log-path");
        fixture.write("second.txt", "new file\n");
        fixture
            .repo
            .stage(&["second.txt".to_owned()])
            .expect("stage");
        fixture
            .repo
            .commit("add the second", false)
            .expect("commit");

        let all = fixture.repo.log(100, None).expect("log");
        let one = fixture.repo.log(100, Some("second.txt")).expect("log");
        assert_eq!(one.len(), 1, "one commit touched this file");
        assert_eq!(one[0].subject, "add the second");
        assert!(all.len() > one.len());
    }

    /// A repository with nothing in it has an empty history, which is a true
    /// answer rather than a failure.
    #[test]
    fn an_unborn_repository_has_an_empty_log() {
        let fixture = Fixture::unborn("log-unborn");
        assert_eq!(fixture.repo.log(10, None).expect("log"), []);
        assert_eq!(fixture.repo.last_message(), None);
    }

    #[test]
    fn a_commits_details_are_its_message_and_what_it_touched() {
        let fixture = Fixture::new("show");
        fixture.write("first.txt", "changed\n");
        fixture.write("second.txt", "new\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned(), "second.txt".to_owned()])
            .expect("stage");
        fixture
            .repo
            .commit("A subject\n\nAnd a body.", false)
            .expect("commit");

        let id = fixture.repo.head_id().expect("a commit");
        let detail = fixture.repo.show(&id).expect("show");
        assert_eq!(detail.message, "A subject\n\nAnd a body.");

        let mut names: Vec<&str> = detail.files.iter().map(|(_, p)| p.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["first.txt", "second.txt"]);
        let kinds: Vec<crate::status::Change> = detail.files.iter().map(|(c, _)| *c).collect();
        assert!(
            kinds.contains(&crate::status::Change::Modified),
            "{kinds:?}"
        );
        assert!(kinds.contains(&crate::status::Change::Added), "{kinds:?}");
    }

    /// A rename spends two path fields, and the second is the name that
    /// matters — the same trap as the status listing.
    #[test]
    fn a_renamed_file_is_listed_under_its_new_name() {
        let fixture = Fixture::new("show-rename");
        std::fs::rename(
            fixture.repo.root.join("first.txt"),
            fixture.repo.root.join("renamed.txt"),
        )
        .expect("rename");
        fixture
            .repo
            .stage(&["first.txt".to_owned(), "renamed.txt".to_owned()])
            .expect("stage");
        fixture.repo.commit("rename it", false).expect("commit");

        let id = fixture.repo.head_id().expect("a commit");
        let detail = fixture.repo.show(&id).expect("show");
        let names: Vec<&str> = detail.files.iter().map(|(_, p)| p.as_str()).collect();
        assert!(
            names.contains(&"renamed.txt"),
            "expected the new name, got {names:?}"
        );
        assert!(
            !names.contains(&"first.txt"),
            "and not the old one, got {names:?}"
        );
    }

    #[test]
    fn blame_attributes_each_line_to_the_commit_that_wrote_it() {
        let fixture = Fixture::new("blame");
        // `first.txt` is "one\ntwo\n" from the initial commit. Add a third line
        // in a commit of its own, so the two lines have different origins.
        fixture.write("first.txt", "one\ntwo\nthree\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        fixture
            .repo
            .commit("add the third line", false)
            .expect("commit");

        let blame = fixture.repo.blame("first.txt").expect("blame");
        assert_eq!(blame.len(), 3, "one entry per line");
        assert_eq!(blame[0].number, 1);
        assert_eq!(blame[2].number, 3);
        assert_eq!(blame[0].origin.summary, "initial");
        assert_eq!(blame[2].origin.summary, "add the third line");
        assert_eq!(blame[0].origin.author, "Test");
        assert!(!blame[0].origin.is_uncommitted());
        assert_eq!(
            blame[0].origin.date().len(),
            10,
            "a date, worked out from the epoch seconds"
        );
        assert_ne!(
            blame[0].origin.id, blame[2].origin.id,
            "two lines, two commits"
        );
    }

    /// A line that has been typed and not committed has no commit to name, and
    /// blame says so with a run of zeros rather than by omitting it.
    #[test]
    fn blame_marks_lines_that_are_not_committed_yet() {
        let fixture = Fixture::new("blame-uncommitted");
        fixture.write("first.txt", "one\ntwo\njust typed\n");

        let blame = fixture.repo.blame("first.txt").expect("blame");
        assert_eq!(blame.len(), 3);
        assert!(!blame[0].origin.is_uncommitted());
        assert!(blame[2].origin.is_uncommitted());
        assert_eq!(blame[2].origin.label(), "Not committed");
    }

    #[test]
    fn blaming_a_file_git_has_never_seen_is_an_error_not_a_panic() {
        let fixture = Fixture::new("blame-untracked");
        fixture.write("second.txt", "never added\n");
        assert!(fixture.repo.blame("second.txt").is_err());
    }

    /// The two columns are independent, and the panel shows a file in both.
    #[test]
    fn a_file_edited_after_staging_appears_on_both_sides() {
        let fixture = Fixture::new("both-sides");
        fixture.write("first.txt", "staged version\n");
        fixture
            .repo
            .stage(&["first.txt".to_owned()])
            .expect("stage");
        fixture.write("first.txt", "and then edited again\n");

        let status = fixture.status();
        assert_eq!(status.staged().count(), 1);
        assert_eq!(status.unstaged().count(), 1);
        assert_eq!(status.entries.len(), 1, "one file, listed under both");
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
