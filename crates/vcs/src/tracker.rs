//! Keeping "what git says" available without ever waiting for git.
//!
//! Two costs have to be kept off the frame loop, and they are different costs.
//!
//! Asking git anything means starting a process: milliseconds when the cache is
//! warm, seconds on a network drive or a cold repository. So every git call
//! happens on a worker thread, and the interface draws whatever answer arrived
//! last — which for the first frame after opening a file is "nothing yet", and
//! that is the honest thing to draw.
//!
//! Diffing is cheap by comparison and happens here, in process, against a
//! cached copy of HEAD's version of the file. That is the whole reason for the
//! cache: the buffer changes on every keystroke, HEAD does not, so fetching the
//! baseline once per file and diffing in memory turns one subprocess per
//! keystroke into one subprocess per file.
//!
//! The cache is dropped whole when HEAD moves. Which commit HEAD is on is
//! checked on a timer rather than watched, because a commit, a checkout, a pull
//! and a rebase all show up the same way — the id changes — and one cheap
//! question every couple of seconds is simpler than watching the four files
//! that could have caused it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::diff::{LineStatus, line_status, lines};
use crate::repo::Repo;

/// Called from the worker thread when an answer arrives.
///
/// An idle egui draws no frames, so an answer that nobody asks for is an answer
/// nobody sees. Every reply wakes the loop. (PLAN.md §2.5.)
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How often to ask whether HEAD has moved.
///
/// One process start at this interval, and only on frames that are being drawn
/// anyway — an editor sitting untouched asks nothing.
const HEAD_INTERVAL: Duration = Duration::from_secs(2);

/// Files larger than this are not tracked.
///
/// The diff itself is fast, but it needs the buffer as lines, and getting there
/// from a rope means copying the file on every keystroke. Past a point that
/// copy costs more than the marks are worth — and a file this size is generated
/// or vendored, where nobody is reading the gutter anyway.
const MAX_TRACKED_BYTES: usize = 1 << 20;

/// Something to do to the index or the working tree.
///
/// Paths are as git spells them: relative to the top level, forward slashes —
/// which is exactly how they arrive from [`crate::status`], so a selection made
/// in the panel can be handed straight back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Add these to the index.
    Stage(Vec<String>),
    /// Take these back out of it, leaving the files alone.
    Unstage(Vec<String>),
    /// Throw away the unstaged changes to these.
    ///
    /// Destroys work that exists nowhere else — see [`Repo::discard`]. The
    /// tracker will do it when asked; asking is the caller's responsibility,
    /// and the caller must have confirmed it with the user first.
    Discard(Vec<String>),
}

impl Action {
    /// A short description, for an error message that needs to say what failed.
    fn verb(&self) -> &'static str {
        match self {
            Self::Stage(_) => "stage",
            Self::Unstage(_) => "unstage",
            Self::Discard(_) => "discard",
        }
    }
}

/// What the worker is asked to do.
enum Request {
    /// Find the repository for a project folder, or forget the one we had.
    Project(Option<PathBuf>),
    /// Fetch HEAD's version of a file.
    Baseline(PathBuf),
    /// Re-read the branch and the commit HEAD is on.
    Head,
    /// Re-read the working tree's state.
    Status,
    /// Change the index or the working tree, then re-read the state.
    Act(Action),
}

/// What the worker found.
enum Reply {
    Project {
        repo: Option<Repo>,
        branch: Option<String>,
        head: Option<String>,
    },
    Baseline {
        path: PathBuf,
        /// `None` when git has no such file at HEAD — newly added, ignored, or
        /// outside the repository.
        text: Option<String>,
    },
    Head {
        branch: Option<String>,
        head: Option<String>,
    },
    Status(Result<crate::status::Status, String>),
    /// An action failed. Success says nothing here — the status that follows
    /// it says everything worth saying.
    Failed(String),
}

/// HEAD's version of one file, or the knowledge that there isn't one.
#[derive(Debug)]
enum Baseline {
    /// Asked for; no answer yet.
    Pending,
    /// Git has no such file at HEAD, so there is nothing to compare against.
    Absent,
    Text(String),
}

/// What is known about the committed version of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// Asked for; the worker has not answered yet.
    Waiting,
    /// Git has no such file at HEAD: it is new, ignored, or outside the
    /// repository.
    Absent,
    /// Fetched and available.
    Ready,
}

/// The marks computed for one buffer, and the version they were computed from.
#[derive(Debug)]
struct Marks {
    version: u64,
    lines: Vec<(usize, LineStatus)>,
}

/// What the repository says about the files being edited.
#[derive(Debug)]
pub struct Tracker {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    repo: Option<Repo>,
    branch: Option<String>,
    head: Option<String>,
    last_head_check: Instant,
    baselines: HashMap<PathBuf, Baseline>,
    marks: HashMap<PathBuf, Marks>,
    /// The working tree's state, as of the last time it was asked for.
    status: crate::status::Status,
    /// Whether a status has ever come back, so an empty list can be told from
    /// a clean tree — they draw very differently.
    status_known: bool,
    /// A status request is out. Stops a panel that is open every frame from
    /// queueing a subprocess every frame.
    status_pending: bool,
    /// Whatever git last said when an action failed, for the caller to show.
    error: Option<String>,
    /// Whether the worker has answered anything at all. Distinguishes "no
    /// repository" from "have not looked yet", which look identical otherwise
    /// and mean opposite things to anything drawing a branch name.
    replied: bool,
    /// How many baselines have been asked for. Only the tests read it, and only
    /// to check that painting a file every frame does not spawn git every
    /// frame — the whole point of the cache.
    requested: usize,
}

impl Tracker {
    /// Start tracking. The worker lives as long as this does.
    #[must_use]
    pub fn new(wake: Waker) -> Self {
        let (requests, inbox) = channel::<Request>();
        let (outbox, replies) = channel::<Reply>();

        std::thread::Builder::new()
            .name("git".to_owned())
            .spawn(move || worker(&inbox, &outbox, &wake))
            // A machine that cannot start a thread has worse problems than a
            // gutter without marks, and the tracker degrades to "no repository"
            // rather than taking the editor down with it.
            .ok();

        Self {
            requests,
            replies,
            repo: None,
            branch: None,
            head: None,
            // Far enough in the past that the first `poll` asks immediately.
            last_head_check: Instant::now() - HEAD_INTERVAL,
            baselines: HashMap::new(),
            marks: HashMap::new(),
            status: crate::status::Status::default(),
            status_known: false,
            status_pending: false,
            error: None,
            replied: false,
            requested: 0,
        }
    }

    /// Point the tracker at a project folder, or at nothing.
    ///
    /// Everything cached from the previous project is dropped: a path means
    /// nothing without the repository it was relative to.
    pub fn set_project(&mut self, root: Option<&Path>) {
        self.repo = None;
        self.branch = None;
        self.head = None;
        self.baselines.clear();
        self.marks.clear();
        self.status = crate::status::Status::default();
        self.status_known = false;
        self.status_pending = false;
        self.error = None;
        self.replied = false;
        self.requested = 0;
        let _ = self
            .requests
            .send(Request::Project(root.map(Path::to_path_buf)));
    }

    /// Take whatever the worker has answered, and ask again if HEAD is due a
    /// check. Call once per frame.
    ///
    /// Returns true when something changed, so the caller can repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;

        while let Ok(reply) = self.replies.try_recv() {
            changed = true;
            self.replied = true;
            match reply {
                Reply::Project { repo, branch, head } => {
                    self.repo = repo;
                    self.branch = branch;
                    self.head = head;
                }
                Reply::Baseline { path, text } => {
                    self.baselines
                        .insert(path.clone(), text.map_or(Baseline::Absent, Baseline::Text));
                    // The marks were computed against no baseline, or a stale
                    // one; either way they no longer describe anything.
                    self.marks.remove(&path);
                }
                Reply::Head { branch, head } => {
                    if head != self.head {
                        // A commit, a checkout, a pull, a rebase. Every
                        // baseline is now a comparison against the wrong
                        // commit, and there is no way to tell which of them
                        // survived except by asking again.
                        self.baselines.clear();
                        self.marks.clear();
                        self.head = head;
                        // And the working tree is now described against a
                        // different commit too.
                        self.refresh_status();
                    }
                    self.branch = branch;
                }
                Reply::Status(result) => {
                    self.status_pending = false;
                    match result {
                        Ok(status) => {
                            self.status = status;
                            self.status_known = true;
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Reply::Failed(message) => self.error = Some(message),
            }
        }

        if self.repo.is_some() && self.last_head_check.elapsed() >= HEAD_INTERVAL {
            self.last_head_check = Instant::now();
            let _ = self.requests.send(Request::Head);
        }

        changed
    }

    /// The branch being worked on, for the status bar.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Whether there is a repository at all.
    #[must_use]
    pub fn has_repo(&self) -> bool {
        self.repo.is_some()
    }

    /// The repository's top level.
    ///
    /// Not the same thing as the open folder, and the difference matters: git
    /// reports every path relative to *this*, so joining one onto the project
    /// folder points at nothing whenever the folder is a subdirectory of the
    /// repository — which is the normal way to open one crate of a workspace.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.repo.as_ref().map(|repo| repo.root.as_path())
    }

    /// The working tree's state, as of the last answer.
    ///
    /// Empty both before the first answer and when the tree is clean;
    /// [`Self::status_known`] separates them, because "nothing to commit" and
    /// "have not looked" are different things to put on screen.
    #[must_use]
    pub fn status(&self) -> &crate::status::Status {
        &self.status
    }

    /// Whether a status has ever come back.
    #[must_use]
    pub fn status_known(&self) -> bool {
        self.status_known
    }

    /// Ask for the working tree's state again.
    ///
    /// Call when something might have changed it: a file saved, a panel opened,
    /// the window regaining focus. Repeated calls while a request is already
    /// out are free, so this is safe to call from a frame loop.
    pub fn refresh_status(&mut self) {
        if self.repo.is_none() || self.status_pending {
            return;
        }
        self.status_pending = true;
        let _ = self.requests.send(Request::Status);
    }

    /// Do something to the index or the working tree.
    ///
    /// The status is re-read afterwards by the worker, so the caller does not
    /// have to ask — and cannot ask too early and get the state from before the
    /// action, which is what makes a staging panel appear not to work.
    ///
    /// [`Action::Discard`] destroys work that exists nowhere else. Confirm with
    /// the user before calling this with one.
    pub fn act(&mut self, action: Action) {
        if self.repo.is_none() {
            return;
        }
        self.error = None;
        self.status_pending = true;
        let _ = self.requests.send(Request::Act(action));
    }

    /// Whatever git last complained about, if anything.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Acknowledge the error, so it stops being shown.
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// What is known about HEAD's version of `path`.
    ///
    /// [`Self::baseline`] returns `None` for two different situations — not
    /// asked yet, and no such file at HEAD — and anything explaining itself to
    /// the user has to tell them apart.
    pub fn baseline_state(&mut self, path: &Path) -> Known {
        self.request_baseline(path);
        match self.baselines.get(path) {
            Some(Baseline::Text(_)) => Known::Ready,
            Some(Baseline::Absent) => Known::Absent,
            _ => Known::Waiting,
        }
    }

    /// HEAD's version of `path`, if it has been fetched.
    ///
    /// Asks for it when it has not, and returns `None` this time round. The
    /// answer arrives on a later frame.
    #[must_use]
    pub fn baseline(&mut self, path: &Path) -> Option<&str> {
        self.request_baseline(path);
        match self.baselines.get(path) {
            Some(Baseline::Text(text)) => Some(text),
            _ => None,
        }
    }

    /// Gutter marks for a buffer: which of its lines differ from HEAD.
    ///
    /// `version` is the document's edit counter and `bytes` its size. `text` is
    /// called only when the diff actually has to be redone — the buffer lives
    /// in a rope, so producing a `&str` copies the whole file, and the common
    /// case by far is a frame where nothing has changed since the last one.
    pub fn marks(
        &mut self,
        path: &Path,
        version: u64,
        bytes: usize,
        text: impl FnOnce() -> String,
    ) -> &[(usize, LineStatus)] {
        self.request_baseline(path);

        if bytes > MAX_TRACKED_BYTES {
            self.marks.remove(path);
            return &[];
        }

        let fresh = self
            .marks
            .get(path)
            .is_some_and(|marks| marks.version == version);
        if !fresh {
            let computed = match self.baselines.get(path) {
                Some(Baseline::Text(committed)) => {
                    let current = text();
                    line_status(&lines(committed), &lines(&current))
                }
                // Not in HEAD, or not answered yet. Marking every line of a new
                // file says nothing that the file being new does not already
                // say, so mark none of them.
                _ => Vec::new(),
            };
            self.marks.insert(
                path.to_path_buf(),
                Marks {
                    version,
                    lines: computed,
                },
            );
        }

        self.marks
            .get(path)
            .map_or(&[][..], |marks| marks.lines.as_slice())
    }

    /// Forget a file that is no longer open.
    pub fn forget(&mut self, path: &Path) {
        self.baselines.remove(path);
        self.marks.remove(path);
    }

    /// Ask for a baseline once, and only once, per file per HEAD.
    fn request_baseline(&mut self, path: &Path) {
        if self.repo.is_none() || self.baselines.contains_key(path) {
            return;
        }
        // Recorded as pending before the request goes out, so a file being
        // painted every frame does not queue a subprocess every frame.
        self.baselines.insert(path.to_path_buf(), Baseline::Pending);
        self.requested += 1;
        let _ = self.requests.send(Request::Baseline(path.to_path_buf()));
    }
}

/// The worker: one git call per request, in order, until the tracker is dropped.
fn worker(inbox: &Receiver<Request>, outbox: &Sender<Reply>, wake: &Waker) {
    let mut repo: Option<Repo> = None;

    while let Ok(request) = inbox.recv() {
        let reply = match request {
            Request::Project(root) => {
                repo = root.as_deref().and_then(Repo::discover);
                Reply::Project {
                    repo: repo.clone(),
                    branch: repo.as_ref().and_then(Repo::branch),
                    head: repo.as_ref().and_then(Repo::head_id),
                }
            }
            Request::Baseline(path) => {
                let text = repo.as_ref().and_then(|r| r.head_contents(&path));
                Reply::Baseline { path, text }
            }
            Request::Head => {
                let Some(repo) = repo.as_ref() else { continue };
                Reply::Head {
                    branch: repo.branch(),
                    head: repo.head_id(),
                }
            }
            Request::Status => {
                let Some(repo) = repo.as_ref() else { continue };
                Reply::Status(repo.status())
            }
            Request::Act(action) => {
                let Some(repo) = repo.as_ref() else { continue };
                let outcome = match &action {
                    Action::Stage(paths) => repo.stage(paths),
                    Action::Unstage(paths) => repo.unstage(paths),
                    Action::Discard(paths) => repo.discard(paths),
                };
                if let Err(message) = outcome {
                    // Say what was being attempted. Git's own message is about
                    // paths and refs and says nothing about which button was
                    // pressed.
                    let failure = format!("Could not {}: {message}", action.verb());
                    if outbox.send(Reply::Failed(failure)).is_err() {
                        return;
                    }
                    wake();
                }
                // The status follows either way. After a success it is the
                // result; after a failure it is proof of what actually
                // happened, which may be some of what was asked for.
                Reply::Status(repo.status())
            }
        };

        if outbox.send(reply).is_err() {
            // The tracker has gone.
            return;
        }
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A waker that counts, so one test can check the repaint rule is kept.
    fn counting() -> (Waker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let mine = Arc::clone(&count);
        (
            Arc::new(move || {
                mine.fetch_add(1, Ordering::SeqCst);
            }),
            count,
        )
    }

    /// Poll until `ready`, or give up.
    ///
    /// Waiting on the condition rather than on a count of replies, because
    /// `poll` itself sends the HEAD request — so replies arrive that the test
    /// never asked for, and counting them is a race the test loses about one
    /// run in three.
    fn wait_for(tracker: &mut Tracker, mut ready: impl FnMut(&mut Tracker) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            tracker.poll();
            if ready(tracker) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn tracker() -> Tracker {
        Tracker::new(Arc::new(|| {}))
    }

    fn project() -> PathBuf {
        // The workspace root: this crate's manifest directory, up two.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    /// A tracker pointed at this project, once git has answered. `None` when
    /// there is no repository, which is a skip rather than a failure.
    fn opened() -> Option<(Tracker, PathBuf)> {
        let root = project();
        let mut tracker = tracker();
        tracker.set_project(Some(&root));
        assert!(
            wait_for(&mut tracker, |t| t.replied),
            "git did not answer within the timeout"
        );
        if !tracker.has_repo() {
            eprintln!("skipping: {} is not a repository", root.display());
            return None;
        }
        Some((tracker, root))
    }

    /// HEAD's version of the workspace manifest, fetched and waited for.
    fn committed_manifest(tracker: &mut Tracker, root: &Path) -> (PathBuf, String) {
        let manifest = root.join("Cargo.toml");
        assert!(
            wait_for(tracker, |t| t.baseline(&manifest).is_some()),
            "Cargo.toml is committed, so HEAD has a version of it"
        );
        let text = tracker
            .baseline(&manifest)
            .expect("just waited for it")
            .to_owned();
        (manifest, text)
    }

    #[test]
    fn nothing_is_known_before_the_worker_answers() {
        let mut tracker = tracker();
        assert_eq!(tracker.branch(), None);
        assert!(!tracker.has_repo());
        assert!(
            tracker
                .marks(Path::new("a.rs"), 1, 3, || "x\n".to_owned())
                .is_empty()
        );
    }

    #[test]
    fn a_project_in_a_repository_reports_its_branch() {
        let Some((tracker, _)) = opened() else { return };
        assert!(
            tracker.branch().is_some_and(|b| !b.is_empty()),
            "a repository always has a branch or a commit to name"
        );
    }

    #[test]
    fn a_project_outside_a_repository_has_no_branch() {
        let outside = std::env::temp_dir();
        if Repo::discover(&outside).is_some() {
            eprintln!("skipping: the temporary directory is inside a repository");
            return;
        }
        let mut tracker = tracker();
        tracker.set_project(Some(&outside));
        wait_for(&mut tracker, |t| t.replied);

        assert!(!tracker.has_repo());
        assert_eq!(tracker.branch(), None);
    }

    /// Every reply has to wake the loop. An idle egui draws no frames, so an
    /// answer that arrives without asking for a repaint is one nobody sees
    /// until the next keystroke — the bug this project has shipped three times.
    #[test]
    fn every_answer_wakes_the_frame_loop() {
        let (wake, count) = counting();
        let mut tracker = Tracker::new(wake);
        tracker.set_project(Some(&project()));
        wait_for(&mut tracker, |t| t.replied);
        assert!(
            count.load(Ordering::SeqCst) > 0,
            "the worker answered without asking for a repaint"
        );
    }

    #[test]
    fn an_unmodified_file_has_no_marks() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let (manifest, committed) = committed_manifest(&mut tracker, &root);
        assert!(
            tracker
                .marks(&manifest, 2, committed.len(), || committed.clone())
                .is_empty(),
            "a buffer identical to HEAD differs from it nowhere"
        );
    }

    #[test]
    fn an_edited_file_marks_the_edited_line() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let (manifest, committed) = committed_manifest(&mut tracker, &root);

        let edited = format!("# a line that is not in HEAD\n{committed}");
        assert_eq!(
            tracker.marks(&manifest, 2, edited.len(), || edited.clone()),
            [(0, LineStatus::Added)],
            "one line added at the top, and nothing else disturbed"
        );
    }

    #[test]
    fn marks_are_not_recomputed_for_the_same_version() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let (manifest, committed) = committed_manifest(&mut tracker, &root);

        let edited = format!("{committed}# trailing\n");
        let first = tracker
            .marks(&manifest, 7, edited.len(), || edited.clone())
            .to_vec();
        assert!(!first.is_empty(), "an added line should be marked");
        // Same version, deliberately contradictory text: the cached answer
        // coming back is the only way to observe that nothing was redone.
        assert_eq!(
            tracker.marks(&manifest, 7, 17, || "totally different".to_owned()),
            first
        );
        // A new version does the work again.
        assert!(
            tracker
                .marks(&manifest, 8, committed.len(), || committed.clone())
                .is_empty()
        );
    }

    #[test]
    fn a_file_git_has_never_seen_gets_no_marks() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let absent = root.join("never-existed-plugh.txt");
        assert!(
            wait_for(&mut tracker, |t| {
                t.marks(&absent, 1, 8, || "one\ntwo\n".to_owned());
                matches!(t.baselines.get(&absent), Some(Baseline::Absent))
            }),
            "git should have reported that it has no such file"
        );
        assert!(
            tracker
                .marks(&absent, 2, 8, || "one\ntwo\n".to_owned())
                .is_empty(),
            "every line of a new file is new, and marking all of them says nothing"
        );
    }

    /// One request per file, however many frames paint it. This is the whole
    /// reason the baseline is cached at all.
    #[test]
    fn a_baseline_is_asked_for_once() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let manifest = root.join("Cargo.toml");
        for version in 0..50 {
            tracker.marks(&manifest, version, 9, || "anything\n".to_owned());
        }
        assert_eq!(
            tracker.requested, 1,
            "one subprocess per file, not one per frame"
        );
    }

    #[test]
    fn a_very_large_buffer_is_not_tracked() {
        let mut tracker = tracker();
        let huge = "x\n".repeat(MAX_TRACKED_BYTES);
        assert!(
            tracker
                .marks(Path::new("big.txt"), 1, huge.len(), || huge.clone())
                .is_empty()
        );
    }

    #[test]
    fn changing_project_forgets_the_old_one() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        committed_manifest(&mut tracker, &root);

        tracker.set_project(None);
        assert_eq!(tracker.branch(), None);
        assert!(!tracker.has_repo());
        assert!(
            tracker.baseline(&root.join("Cargo.toml")).is_none(),
            "a cached baseline means nothing without the repository it came from"
        );
    }

    #[test]
    fn a_closed_file_is_forgotten() {
        let Some((mut tracker, root)) = opened() else {
            return;
        };
        let (manifest, _) = committed_manifest(&mut tracker, &root);
        tracker.forget(&manifest);
        assert!(!tracker.baselines.contains_key(&manifest));
        assert!(!tracker.marks.contains_key(&manifest));
    }

    // ---- status and staging ---------------------------------------------

    /// A repository of its own, so staging tests cannot touch this project's.
    ///
    /// Deliberately not shared with `repo::tests::Fixture`: a test helper that
    /// two modules reach into stops being obvious about what it sets up, and
    /// this one wants a tracker pointed at it as well.
    struct Sandbox {
        root: PathBuf,
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "the-editor-tracker-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temporary directory");

            let git = |args: &[&str]| {
                let mut command = editor_proc::spawn::quiet("git");
                command.current_dir(&root).args(args);
                let output = command.output().expect("running git");
                assert!(
                    output.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "--quiet", "-b", "trial"]);
            git(&["config", "user.email", "test@example.invalid"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "commit.gpgsign", "false"]);
            git(&["config", "core.autocrlf", "false"]);
            std::fs::write(root.join("first.txt"), "one\ntwo\n").expect("write");
            git(&["add", "."]);
            git(&["commit", "--quiet", "-m", "initial"]);

            // Discovered, because git spells the temporary directory its own
            // way and the tracker will be comparing against that spelling.
            let root = Repo::discover(&root).expect("a repository").root;
            Self { root }
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.root.join(name), text).expect("write");
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.root.join(name)).expect("read")
        }

        /// A tracker pointed here, with its first status already in.
        fn tracker(&self) -> Tracker {
            let mut tracker = super::Tracker::new(Arc::new(|| {}));
            tracker.set_project(Some(&self.root));
            assert!(
                wait_for(&mut tracker, |t| t.has_repo()),
                "the sandbox should be found as a repository"
            );
            tracker.refresh_status();
            assert!(
                wait_for(&mut tracker, |t| t.status_known()),
                "the first status never arrived"
            );
            tracker
        }
    }

    #[test]
    fn nothing_is_known_about_the_working_tree_before_it_is_asked_for() {
        let tracker = tracker();
        assert!(!tracker.status_known());
        assert!(tracker.status().is_clean());
        assert_eq!(tracker.error(), None);
    }

    #[test]
    fn a_clean_tree_is_reported_as_clean_rather_than_unknown() {
        let sandbox = Sandbox::new("clean");
        let tracker = sandbox.tracker();
        assert!(tracker.status_known(), "the answer arrived");
        assert!(tracker.status().is_clean(), "and it was: nothing to do");
    }

    #[test]
    fn an_edit_shows_up_once_the_status_is_refreshed() {
        let sandbox = Sandbox::new("edit");
        let mut tracker = sandbox.tracker();
        assert!(tracker.status().is_clean());

        sandbox.write("first.txt", "one\nCHANGED\n");
        tracker.refresh_status();
        assert!(
            wait_for(&mut tracker, |t| !t.status().is_clean()),
            "the edit should be reported"
        );
        assert_eq!(tracker.status().unstaged().count(), 1);
    }

    #[test]
    fn staging_moves_a_file_across_and_the_status_follows_by_itself() {
        let sandbox = Sandbox::new("stage");
        let mut tracker = sandbox.tracker();
        sandbox.write("first.txt", "one\nCHANGED\n");
        tracker.refresh_status();
        assert!(wait_for(&mut tracker, |t| t.status().unstaged().count() == 1));

        tracker.act(Action::Stage(vec!["first.txt".to_owned()]));
        // No `refresh_status` here on purpose: the worker re-reads the status
        // after acting, because a caller that asks for itself can ask too early
        // and get the state from *before* the action — which is exactly what
        // makes a staging panel look like it does nothing.
        assert!(
            wait_for(&mut tracker, |t| t.status().staged().count() == 1),
            "staging should be reflected without being asked for"
        );
        assert_eq!(tracker.status().unstaged().count(), 0);
        assert_eq!(tracker.error(), None);
    }

    #[test]
    fn unstaging_puts_it_back() {
        let sandbox = Sandbox::new("unstage");
        let mut tracker = sandbox.tracker();
        sandbox.write("first.txt", "one\nCHANGED\n");
        tracker.act(Action::Stage(vec!["first.txt".to_owned()]));
        assert!(wait_for(&mut tracker, |t| t.status().staged().count() == 1));

        tracker.act(Action::Unstage(vec!["first.txt".to_owned()]));
        assert!(
            wait_for(&mut tracker, |t| t.status().unstaged().count() == 1),
            "unstaging should be reflected"
        );
        assert_eq!(tracker.status().staged().count(), 0);
        assert_eq!(
            sandbox.read("first.txt"),
            "one\nCHANGED\n",
            "and must not have touched the file"
        );
    }

    #[test]
    fn discarding_restores_the_committed_text() {
        let sandbox = Sandbox::new("discard");
        let mut tracker = sandbox.tracker();
        sandbox.write("first.txt", "wrecked\n");
        // The tree has to be seen as dirty *first*, or waiting for it to become
        // clean afterwards is waiting for something that is already true and
        // the test passes without the discard having happened at all.
        tracker.refresh_status();
        assert!(wait_for(&mut tracker, |t| !t.status().is_clean()));

        tracker.act(Action::Discard(vec!["first.txt".to_owned()]));
        assert!(
            wait_for(&mut tracker, |t| t.status().is_clean()),
            "discarding should leave nothing to report"
        );
        assert_eq!(sandbox.read("first.txt"), "one\ntwo\n");
    }

    /// Git's own message is the useful one, so it has to survive the trip.
    #[test]
    fn a_failed_action_reports_what_git_said() {
        let sandbox = Sandbox::new("failure");
        let mut tracker = sandbox.tracker();
        tracker.act(Action::Discard(vec!["no-such-file.txt".to_owned()]));
        assert!(
            wait_for(&mut tracker, |t| t.error().is_some()),
            "git refused, and the refusal should have been reported"
        );
        let message = tracker.error().expect("a message").to_owned();
        assert!(
            message.starts_with("Could not discard:"),
            "it should say which action failed, got {message:?}"
        );
        assert!(
            message.len() > "Could not discard:".len() + 1,
            "and carry git's own words, got {message:?}"
        );

        tracker.clear_error();
        assert_eq!(tracker.error(), None);
    }

    /// The whole reason the panel does not spawn git on every frame it is open.
    #[test]
    fn refreshing_repeatedly_queues_one_request_not_many() {
        let sandbox = Sandbox::new("coalesce");
        let mut tracker = sandbox.tracker();
        for _ in 0..100 {
            tracker.refresh_status();
        }
        assert!(
            wait_for(&mut tracker, |t| !t.status_pending),
            "the outstanding request should have completed"
        );
        // A second round is allowed once the first has landed; the guarantee is
        // that a hundred calls in one frame do not become a hundred processes.
        tracker.refresh_status();
        assert!(tracker.status_pending);
        tracker.refresh_status();
        assert!(tracker.status_pending, "still just the one");
    }

    #[test]
    fn a_tracker_with_no_repository_refuses_to_act() {
        let mut tracker = tracker();
        tracker.act(Action::Stage(vec!["anything".to_owned()]));
        tracker.refresh_status();
        tracker.poll();
        assert!(!tracker.status_known());
        assert_eq!(tracker.error(), None, "there was nothing to fail");
    }

    /// Opening one crate of a workspace is the normal case, and it makes the
    /// project folder and the repository root two different directories. Every
    /// path git reports is relative to the second, so anything joining them
    /// onto the first points at nothing.
    #[test]
    fn opening_a_subdirectory_still_reports_the_repositorys_top_level() {
        let sandbox = Sandbox::new("subdirectory");
        let inner = sandbox.root.join("crates").join("thing");
        std::fs::create_dir_all(&inner).expect("subdirectory");

        let mut tracker = super::Tracker::new(Arc::new(|| {}));
        tracker.set_project(Some(&inner));
        assert!(wait_for(&mut tracker, |t| t.replied));

        assert_eq!(
            tracker.root(),
            Some(sandbox.root.as_path()),
            "the top level, not the folder that was opened"
        );
    }

    #[test]
    fn a_tracker_with_no_repository_has_no_root() {
        let tracker = tracker();
        assert_eq!(tracker.root(), None);
    }

    #[test]
    fn changing_project_forgets_the_working_tree_too() {
        let sandbox = Sandbox::new("forget-status");
        let mut tracker = sandbox.tracker();
        sandbox.write("first.txt", "changed\n");
        tracker.refresh_status();
        assert!(wait_for(&mut tracker, |t| !t.status().is_clean()));

        tracker.set_project(None);
        assert!(!tracker.status_known());
        assert!(tracker.status().is_clean());
    }
}
