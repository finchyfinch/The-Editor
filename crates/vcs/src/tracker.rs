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
    /// Commit what is staged.
    ///
    /// `amend` rewrites the commit HEAD is on rather than adding one. That is
    /// safe on work nobody else has seen and a nuisance on work they have, so
    /// it is the caller's decision and never the default.
    Commit { message: String, amend: bool },
    /// Move onto a branch.
    Checkout(String),
    /// Start a branch at HEAD.
    CreateBranch { name: String, switch: bool },
    /// Delete a branch.
    ///
    /// With `force`, this drops commits that are not merged anywhere. The
    /// reflog can still reach them for a while; nothing in the interface can.
    /// Callers must confirm before forcing.
    DeleteBranch { name: String, force: bool },
    /// Ask a remote what it has, without taking any of it.
    Fetch(String),
    /// Bring the current branch up to date, fast-forward only.
    Pull,
    /// Send the current branch to a remote. Never forced.
    Push {
        remote: String,
        branch: String,
        set_upstream: bool,
    },
}

impl Action {
    /// Whether this touches a remote, and so may take a while.
    ///
    /// The worker runs one thing at a time — deliberately, because two git
    /// processes on one repository contend for the index lock — so a slow
    /// fetch holds up the gutter behind it. Saying which of these is running
    /// is what makes that legible rather than mysterious.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Fetch(_) | Self::Pull | Self::Push { .. })
    }

    /// Whether this changes which branches exist, or where they point.
    fn touches_branches(&self) -> bool {
        matches!(
            self,
            Self::Checkout(_)
                | Self::CreateBranch { .. }
                | Self::DeleteBranch { .. }
                | Self::Commit { .. }
                | Self::Fetch(_)
                | Self::Pull
                | Self::Push { .. }
        )
    }
}

impl Action {
    /// A short description, for an error message that needs to say what failed.
    fn verb(&self) -> &'static str {
        match self {
            Self::Stage(_) => "stage",
            Self::Unstage(_) => "unstage",
            Self::Discard(_) => "discard",
            Self::Commit { amend: false, .. } => "commit",
            Self::Commit { amend: true, .. } => "amend",
            Self::Checkout(_) => "switch branch",
            Self::CreateBranch { .. } => "create the branch",
            Self::DeleteBranch { .. } => "delete the branch",
            Self::Fetch(_) => "fetch",
            Self::Pull => "pull",
            Self::Push { .. } => "push",
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
    /// Read the recent commits.
    Log { limit: usize },
    /// Read one commit in full.
    Show(String),
    /// Read who last touched each line of a file.
    Blame(PathBuf),
    /// Read the branches and the configured remotes.
    Branches,
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
    ///
    /// The action comes back with the message because some failures are worth
    /// *offering something about* rather than only reporting: git refusing to
    /// delete an unmerged branch is the prompt to ask whether to force it, and
    /// that needs to know which branch.
    Failed(Action, String),
    /// A commit landed. The only action whose *success* needs announcing: it is
    /// what tells the panel it may clear the message box, and clearing it on
    /// anything less certain would throw away a message a failing hook rejected.
    Committed,
    /// The recent commits, and the message of the one HEAD is on. The second is
    /// what an amend starts from, and it is free to fetch alongside the first.
    Log {
        commits: Result<Vec<crate::log::Commit>, String>,
        last_message: Option<String>,
    },
    Show {
        id: String,
        detail: Result<crate::log::Detail, String>,
    },
    Blame {
        path: PathBuf,
        lines: Result<Vec<crate::blame::Line>, String>,
    },
    Branches {
        branches: Result<Vec<crate::branch::Branch>, String>,
        remotes: Vec<String>,
    },
    /// A remote operation said something worth reading — git's summary of what
    /// it fetched or pushed, which is the only confirmation that it worked.
    Remote(String),
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
    /// The recent commits, newest first.
    log: Vec<crate::log::Commit>,
    log_known: bool,
    log_pending: bool,
    /// How far back the last request went, so asking for more can be told from
    /// asking again.
    log_limit: usize,
    /// The message of the commit HEAD is on, for an amend to start from.
    last_message: Option<String>,
    /// A commit landed and nobody has been told yet.
    committed: bool,
    /// The action that produced [`Self::error`], for a caller that can offer
    /// something better than the message alone.
    failed: Option<Action>,
    /// The branches, local and remote-tracking.
    branches: Vec<crate::branch::Branch>,
    branches_known: bool,
    branches_pending: bool,
    /// The configured remotes, in git's order. The first is the default for
    /// pushing, which is what `origin` being first means in practice.
    remotes: Vec<String>,
    /// The remote operation currently running, if any, so the interface can say
    /// what it is waiting for instead of appearing to have stopped.
    busy: Option<&'static str>,
    /// What a remote operation last reported. Git's own summary — "Everything
    /// up-to-date", the ref update lines — which is the only confirmation that
    /// anything happened.
    remote_said: Option<String>,
    /// Commits whose full details have been fetched, keyed by object name. A
    /// commit never changes, so this is only cleared when the project does.
    details: HashMap<String, crate::log::Detail>,
    /// Details asked for and not yet answered.
    details_pending: std::collections::HashSet<String>,
    /// Blame for one file at a time — the one being looked at. Holding every
    /// file's would be a copy of the repository's history in memory for the
    /// sake of a margin nobody is reading on the other tabs.
    blame: Option<(PathBuf, Vec<crate::blame::Line>)>,
    blame_pending: Option<PathBuf>,
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
            log: Vec::new(),
            log_known: false,
            log_pending: false,
            log_limit: 0,
            last_message: None,
            committed: false,
            failed: None,
            branches: Vec::new(),
            branches_known: false,
            branches_pending: false,
            remotes: Vec::new(),
            busy: None,
            remote_said: None,
            details: HashMap::new(),
            details_pending: std::collections::HashSet::new(),
            blame: None,
            blame_pending: None,
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
        self.log.clear();
        self.log_known = false;
        self.log_pending = false;
        self.log_limit = 0;
        self.last_message = None;
        self.committed = false;
        self.failed = None;
        self.branches.clear();
        self.branches_known = false;
        self.branches_pending = false;
        self.remotes.clear();
        self.busy = None;
        self.remote_said = None;
        self.details.clear();
        self.details_pending.clear();
        self.blame = None;
        self.blame_pending = None;
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
                        // The history gained or lost a commit, and the blame
                        // for every line of every file may have moved with it.
                        // Details are kept: a commit's contents never change,
                        // and one that was rewritten has a different name.
                        self.blame = None;
                        self.blame_pending = None;
                        if self.log_known {
                            self.refresh_log(self.log_limit);
                        }
                        // And a branch has moved, or a different one is now
                        // checked out, or both.
                        if self.branches_known {
                            self.refresh_branches();
                        }
                    }
                    self.branch = branch;
                }
                Reply::Log {
                    commits,
                    last_message,
                } => {
                    self.log_pending = false;
                    self.last_message = last_message;
                    match commits {
                        Ok(commits) => {
                            self.log = commits;
                            self.log_known = true;
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Reply::Show { id, detail } => {
                    self.details_pending.remove(&id);
                    match detail {
                        Ok(detail) => {
                            self.details.insert(id, detail);
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Reply::Blame { path, lines } => {
                    if self.blame_pending.as_ref() == Some(&path) {
                        self.blame_pending = None;
                    }
                    match lines {
                        Ok(lines) => self.blame = Some((path, lines)),
                        // Not an error worth interrupting anyone with: a file
                        // git has never seen has no blame, which is a fact
                        // about the file rather than a failure.
                        Err(_) => self.blame = Some((path, Vec::new())),
                    }
                }
                Reply::Status(result) => {
                    self.status_pending = false;
                    // Every action ends with a status, so this is where a
                    // remote operation stops being in progress.
                    self.busy = None;
                    match result {
                        Ok(status) => {
                            self.status = status;
                            self.status_known = true;
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Reply::Failed(action, message) => {
                    self.error = Some(message);
                    self.failed = Some(action);
                }
                Reply::Committed => self.committed = true,
                Reply::Branches { branches, remotes } => {
                    self.branches_pending = false;
                    self.remotes = remotes;
                    match branches {
                        Ok(branches) => {
                            self.branches = branches;
                            self.branches_known = true;
                        }
                        Err(message) => self.error = Some(message),
                    }
                }
                Reply::Remote(said) => {
                    self.remote_said = (!said.trim().is_empty()).then(|| said.trim().to_owned());
                }
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
        self.failed = None;
        self.status_pending = true;
        if action.is_remote() {
            self.busy = Some(match action {
                Action::Fetch(_) => "Fetching\u{2026}",
                Action::Pull => "Pulling\u{2026}",
                _ => "Pushing\u{2026}",
            });
            self.remote_said = None;
        }
        // A commit moves HEAD, and the HEAD check is what notices — and what
        // then reloads the baselines, the gutter and the history. Waiting up to
        // the poll interval for it means the panel sits showing the state from
        // before the commit for a second or two, which reads as a failure.
        self.last_head_check = Instant::now() - HEAD_INTERVAL;
        let _ = self.requests.send(Request::Act(action));
    }

    /// Every branch, local ones first, then the remote-tracking ones.
    #[must_use]
    pub fn branches(&self) -> &[crate::branch::Branch] {
        &self.branches
    }

    /// Whether a branch listing has ever come back.
    #[must_use]
    pub fn branches_known(&self) -> bool {
        self.branches_known
    }

    /// The branch HEAD is on, with what it knows about its upstream.
    #[must_use]
    pub fn current_branch(&self) -> Option<&crate::branch::Branch> {
        self.branches.iter().find(|b| b.is_head)
    }

    /// The configured remotes. Empty means there is nowhere to push.
    #[must_use]
    pub fn remotes(&self) -> &[String] {
        &self.remotes
    }

    /// Read the branches and remotes again.
    pub fn refresh_branches(&mut self) {
        if self.repo.is_none() || self.branches_pending {
            return;
        }
        self.branches_pending = true;
        let _ = self.requests.send(Request::Branches);
    }

    /// The remote operation currently running, if any.
    ///
    /// The worker runs one thing at a time, so while this is set the gutter and
    /// the status are waiting behind it. Saying so is the difference between a
    /// slow network and an editor that has stopped.
    #[must_use]
    pub fn busy(&self) -> Option<&'static str> {
        self.busy
    }

    /// What a remote operation last reported, in git's own words.
    #[must_use]
    pub fn remote_said(&self) -> Option<&str> {
        self.remote_said.as_deref()
    }

    /// Acknowledge that report, so it stops being shown.
    pub fn clear_remote_said(&mut self) {
        self.remote_said = None;
    }

    /// The recent commits, newest first.
    #[must_use]
    pub fn log(&self) -> &[crate::log::Commit] {
        &self.log
    }

    /// Whether a history has ever come back, so an empty list can be told from
    /// a repository with no commits in it.
    #[must_use]
    pub fn log_known(&self) -> bool {
        self.log_known
    }

    /// How far back the history currently goes.
    #[must_use]
    pub fn log_limit(&self) -> usize {
        self.log_limit
    }

    /// Read the most recent `limit` commits.
    ///
    /// Repeated calls while a request is out are free, so this is safe from a
    /// frame loop. Asking for *more* than is already loaded is not free and is
    /// how the history view grows.
    pub fn refresh_log(&mut self, limit: usize) {
        if self.repo.is_none() || self.log_pending {
            return;
        }
        self.log_pending = true;
        self.log_limit = limit;
        let _ = self.requests.send(Request::Log { limit });
    }

    /// The message of the commit HEAD is on, for an amend to start from.
    #[must_use]
    pub fn last_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }

    /// Whether a commit landed since this was last asked.
    ///
    /// Taken rather than read, because the one thing it is for — emptying the
    /// message box — must happen exactly once. A commit refused by a hook does
    /// *not* set this, so the message survives to be tried again.
    pub fn take_committed(&mut self) -> bool {
        std::mem::take(&mut self.committed)
    }

    /// One commit in full, if it has already been fetched.
    ///
    /// Separate from [`Self::detail`] because it takes `&self`: a caller that
    /// has asked once needs to read the answer *while* borrowing the log, and
    /// asking again to do so would need a second mutable borrow.
    #[must_use]
    pub fn known_detail(&self, id: &str) -> Option<&crate::log::Detail> {
        self.details.get(id)
    }

    /// One commit in full, if it has been fetched. Asks for it if not.
    pub fn detail(&mut self, id: &str) -> Option<&crate::log::Detail> {
        if self.repo.is_some()
            && !self.details.contains_key(id)
            && self.details_pending.insert(id.to_owned())
        {
            let _ = self.requests.send(Request::Show(id.to_owned()));
        }
        self.details.get(id)
    }

    /// Who last touched each line of `path`, if it has been fetched.
    ///
    /// Asks for it when the answer on hand is about a different file, and
    /// returns nothing this time round. One file's blame is held at a time.
    pub fn blame(&mut self, path: &Path) -> Option<&[crate::blame::Line]> {
        let have = self
            .blame
            .as_ref()
            .is_some_and(|(cached, _)| cached == path);
        if !have && self.repo.is_some() && self.blame_pending.as_deref() != Some(path) {
            self.blame_pending = Some(path.to_path_buf());
            let _ = self.requests.send(Request::Blame(path.to_path_buf()));
        }
        self.blame
            .as_ref()
            .filter(|(cached, _)| cached == path)
            .map(|(_, lines)| lines.as_slice())
    }

    /// Read `path`'s blame again.
    ///
    /// Blame describes the file *on disk*, so saving invalidates it even though
    /// HEAD has not moved — which is the one case the HEAD check cannot catch.
    pub fn refresh_blame(&mut self, path: &Path) {
        if self
            .blame
            .as_ref()
            .is_some_and(|(cached, _)| cached == path)
        {
            self.blame = None;
        }
        self.blame_pending = None;
    }

    /// Whatever git last complained about, if anything.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The action that produced the current error, if there is one.
    ///
    /// Taken rather than read: a caller that turns a refusal into an offer must
    /// do so once, and leaving it set would raise the same dialog every frame.
    pub fn take_failed(&mut self) -> Option<Action> {
        self.failed.take()
    }

    /// Acknowledge the error, so it stops being shown.
    pub fn clear_error(&mut self) {
        self.error = None;
        self.failed = None;
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
            Request::Log { limit } => {
                let Some(repo) = repo.as_ref() else { continue };
                Reply::Log {
                    commits: repo.log(limit, None),
                    // Fetched here rather than on its own request: it is one
                    // more cheap call on a thread that is already running one,
                    // and it is always wanted at the same moment.
                    last_message: repo.last_message(),
                }
            }
            Request::Show(id) => {
                let Some(repo) = repo.as_ref() else { continue };
                let detail = repo.show(&id);
                Reply::Show { id, detail }
            }
            Request::Blame(path) => {
                let Some(repo) = repo.as_ref() else { continue };
                let lines = repo
                    .relative(&path)
                    .ok_or_else(|| "that file is not in this repository".to_owned())
                    .and_then(|relative| repo.blame(&relative));
                Reply::Blame { path, lines }
            }
            Request::Branches => {
                let Some(repo) = repo.as_ref() else { continue };
                Reply::Branches {
                    branches: repo.branches(),
                    // Not a `Result`: a repository with no remotes is the
                    // ordinary case, and so is one where the question failed —
                    // either way there is nowhere to push.
                    remotes: repo.remotes().unwrap_or_default(),
                }
            }
            Request::Act(action) => {
                let Some(repo) = repo.as_ref() else { continue };
                // Remote operations have something to report even when they
                // succeed — "Everything up-to-date", the ref update lines —
                // and that report is the only confirmation anything happened.
                let mut said = None;
                let outcome = match &action {
                    Action::Stage(paths) => repo.stage(paths),
                    Action::Unstage(paths) => repo.unstage(paths),
                    Action::Discard(paths) => repo.discard(paths),
                    Action::Commit { message, amend } => repo.commit(message, *amend).map(drop),
                    Action::Checkout(name) => repo.checkout(name),
                    Action::CreateBranch { name, switch } => repo.create_branch(name, *switch),
                    Action::DeleteBranch { name, force } => repo.delete_branch(name, *force),
                    Action::Fetch(remote) => repo.fetch(remote),
                    Action::Pull => repo.pull().map(|text| said = Some(text)),
                    Action::Push {
                        remote,
                        branch,
                        set_upstream,
                    } => repo
                        .push(remote, branch, *set_upstream)
                        .map(|text| said = Some(text)),
                };

                if let Some(text) = said
                    && outbox.send(Reply::Remote(text)).is_err()
                {
                    return;
                }
                // A branch action changes which branches exist or where they
                // point, and the panel is showing the answer from before it.
                if action.touches_branches()
                    && outbox
                        .send(Reply::Branches {
                            branches: repo.branches(),
                            remotes: repo.remotes().unwrap_or_default(),
                        })
                        .is_err()
                {
                    return;
                }
                match outcome {
                    Err(message) => {
                        // Say what was being attempted. Git's own message is
                        // about paths and refs and says nothing about which
                        // button was pressed.
                        let failure = format!("Could not {}: {message}", action.verb());
                        if outbox.send(Reply::Failed(action.clone(), failure)).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(()) if matches!(action, Action::Commit { .. }) => {
                        if outbox.send(Reply::Committed).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(()) => {}
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
            // Through the read-only-aware remover: git marks every object file
            // read-only, so `remove_dir_all` fails on the first one and leaves
            // a repository behind for every test, every run.
            crate::repo::tests::remove_tree(&self.root);
            crate::repo::tests::remove_tree(&self.root.with_extension("origin.git"));
        }
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "the-editor-tracker-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            crate::repo::tests::remove_tree(&root);
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

        /// Give this repository an `origin` it can push to.
        ///
        /// A bare repository next door — which is exactly the backup remote
        /// PLAN.md §12 describes, so this exercises the real arrangement and
        /// touches no network.
        fn with_origin(&self) -> PathBuf {
            let origin = self.root.with_extension("origin.git");
            crate::repo::tests::remove_tree(&origin);
            std::fs::create_dir_all(&origin).expect("origin directory");

            let git = |cwd: &Path, args: &[&str]| {
                let mut command = editor_proc::spawn::quiet("git");
                command.current_dir(cwd).args(args);
                let output = command.output().expect("running git");
                assert!(
                    output.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&origin, &["init", "--bare", "--quiet"]);
            let url = origin.to_string_lossy().replace('\\', "/");
            git(&self.root, &["remote", "add", "origin", &url]);
            origin
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

    // ---- committing, history and blame ----------------------------------

    #[test]
    fn committing_clears_the_tree_and_lands_in_the_history() {
        let sandbox = Sandbox::new("commit");
        let mut tracker = sandbox.tracker();
        tracker.refresh_log(20);
        assert!(wait_for(&mut tracker, |t| t.log_known()));
        let before = tracker.log().len();

        sandbox.write("first.txt", "one\nCHANGED\n");
        tracker.act(Action::Stage(vec!["first.txt".to_owned()]));
        assert!(wait_for(&mut tracker, |t| t.status().staged().count() == 1));

        tracker.act(Action::Commit {
            message: "A committed change".to_owned(),
            amend: false,
        });
        assert!(
            wait_for(&mut tracker, |t| t.status().is_clean()),
            "the tree should be clean once the commit lands"
        );
        // The history follows without being asked, because HEAD moved and the
        // tracker notices that for itself.
        assert!(
            wait_for(&mut tracker, |t| t.log().len() > before),
            "the new commit should appear in the history"
        );
        assert_eq!(tracker.log()[0].subject, "A committed change");
        assert_eq!(tracker.error(), None);
    }

    #[test]
    fn a_commit_with_nothing_staged_reports_what_git_said() {
        let sandbox = Sandbox::new("commit-empty");
        let mut tracker = sandbox.tracker();
        tracker.act(Action::Commit {
            message: "nothing to say".to_owned(),
            amend: false,
        });
        assert!(wait_for(&mut tracker, |t| t.error().is_some()));
        let message = tracker.error().expect("a message").to_owned();
        assert!(message.starts_with("Could not commit:"), "got {message:?}");
    }

    #[test]
    fn amending_offers_the_previous_message_to_start_from() {
        let sandbox = Sandbox::new("amend-message");
        let mut tracker = sandbox.tracker();
        tracker.refresh_log(20);
        assert!(wait_for(&mut tracker, |t| t.log_known()));
        assert_eq!(
            tracker.last_message(),
            Some("initial"),
            "amending edits that commit, so its message is where to start"
        );
    }

    #[test]
    fn amending_rewrites_rather_than_adds() {
        let sandbox = Sandbox::new("amend");
        let mut tracker = sandbox.tracker();
        tracker.refresh_log(20);
        assert!(wait_for(&mut tracker, |t| t.log_known()));
        let before = tracker.log().len();

        sandbox.write("first.txt", "one\nCHANGED\n");
        tracker.act(Action::Stage(vec!["first.txt".to_owned()]));
        assert!(wait_for(&mut tracker, |t| t.status().staged().count() == 1));
        tracker.act(Action::Commit {
            message: "second thoughts".to_owned(),
            amend: true,
        });

        assert!(wait_for(&mut tracker, |t| t
            .log()
            .first()
            .is_some_and(|c| c.subject == "second thoughts")));
        assert_eq!(
            tracker.log().len(),
            before,
            "the commit was rewritten, not added to"
        );
    }

    #[test]
    fn a_commits_details_are_fetched_once_and_then_kept() {
        let sandbox = Sandbox::new("detail");
        let mut tracker = sandbox.tracker();
        tracker.refresh_log(20);
        assert!(wait_for(&mut tracker, |t| t.log_known()));
        let id = tracker.log()[0].id.clone();

        assert_eq!(tracker.detail(&id), None, "asked for; not answered yet");
        assert!(wait_for(&mut tracker, |t| t.detail(&id).is_some()));

        let detail = tracker.detail(&id).expect("details").clone();
        assert_eq!(detail.message, "initial");
        assert_eq!(detail.files.len(), 1);
        assert_eq!(detail.files[0].1, "first.txt");

        // A commit never changes, so a second ask must not queue a second
        // subprocess — the pending set is what stops it.
        for _ in 0..50 {
            tracker.detail(&id);
        }
        assert!(tracker.details_pending.is_empty());
    }

    #[test]
    fn blame_says_who_last_touched_each_line() {
        let sandbox = Sandbox::new("blame");
        let mut tracker = sandbox.tracker();
        let file = sandbox.root.join("first.txt");

        assert_eq!(tracker.blame(&file), None, "asked for; not answered yet");
        assert!(wait_for(&mut tracker, |t| t.blame(&file).is_some()));

        let lines = tracker.blame(&file).expect("blame").to_vec();
        assert_eq!(lines.len(), 2, "first.txt has two lines");
        assert_eq!(lines[0].origin.summary, "initial");
        assert_eq!(lines[0].origin.author, "Test");
    }

    /// Blame describes the file on disk, so saving invalidates it even though
    /// HEAD has not moved — the one case the HEAD check cannot catch.
    #[test]
    fn saving_makes_the_blame_stale() {
        let sandbox = Sandbox::new("blame-stale");
        let mut tracker = sandbox.tracker();
        let file = sandbox.root.join("first.txt");
        assert!(wait_for(&mut tracker, |t| t.blame(&file).is_some()));
        assert_eq!(tracker.blame(&file).expect("blame").len(), 2);

        sandbox.write("first.txt", "one\ntwo\nthree\nfour\n");
        tracker.refresh_blame(&file);
        assert_eq!(tracker.blame(&file), None, "the old answer was dropped");
        assert!(wait_for(&mut tracker, |t| t
            .blame(&file)
            .is_some_and(|lines| lines.len() == 4)));
        assert!(
            tracker.blame(&file).expect("blame")[3]
                .origin
                .is_uncommitted(),
            "the lines just typed belong to no commit"
        );
    }

    #[test]
    fn a_file_outside_the_repository_has_no_blame_and_no_error() {
        let sandbox = Sandbox::new("blame-outside");
        let mut tracker = sandbox.tracker();
        let outside = std::env::temp_dir().join("nothing-to-do-with-this-repo.txt");
        assert!(wait_for(&mut tracker, |t| t.blame(&outside).is_some()));
        assert_eq!(tracker.blame(&outside), Some(&[][..]));
        assert_eq!(
            tracker.error(),
            None,
            "a file with no history is a fact about the file, not a failure"
        );
    }

    #[test]
    fn a_repository_with_no_commits_has_an_empty_history() {
        let mut tracker = tracker();
        assert!(!tracker.log_known());
        tracker.refresh_log(20);
        tracker.poll();
        assert_eq!(tracker.log(), []);
    }

    // ---- branches and remotes -------------------------------------------

    #[test]
    fn the_branches_come_back_with_the_one_we_are_on_marked() {
        let sandbox = Sandbox::new("branches");
        let mut tracker = sandbox.tracker();
        assert!(!tracker.branches_known());

        tracker.refresh_branches();
        assert!(wait_for(&mut tracker, |t| t.branches_known()));
        assert_eq!(tracker.branches().len(), 1);
        assert_eq!(
            tracker.current_branch().map(|b| b.name.as_str()),
            Some("trial")
        );
        assert_eq!(tracker.remotes(), Vec::<String>::new());
    }

    #[test]
    fn creating_a_branch_switches_to_it_and_the_listing_follows() {
        let sandbox = Sandbox::new("create");
        let mut tracker = sandbox.tracker();
        tracker.refresh_branches();
        assert!(wait_for(&mut tracker, |t| t.branches_known()));

        tracker.act(Action::CreateBranch {
            name: "feature/thing".to_owned(),
            switch: true,
        });
        // The listing is re-read by the worker, not asked for here.
        assert!(
            wait_for(&mut tracker, |t| t
                .current_branch()
                .is_some_and(|b| b.name == "feature/thing")),
            "the new branch should become the current one"
        );
        assert_eq!(tracker.branches().len(), 2);
        assert_eq!(tracker.error(), None);
    }

    #[test]
    fn switching_back_and_forth_is_reflected() {
        let sandbox = Sandbox::new("checkout");
        let mut tracker = sandbox.tracker();
        tracker.act(Action::CreateBranch {
            name: "other".to_owned(),
            switch: false,
        });
        assert!(wait_for(&mut tracker, |t| t.branches().len() == 2));

        tracker.act(Action::Checkout("other".to_owned()));
        assert!(wait_for(&mut tracker, |t| t
            .current_branch()
            .is_some_and(|b| b.name == "other")));

        tracker.act(Action::Checkout("trial".to_owned()));
        assert!(wait_for(&mut tracker, |t| t
            .current_branch()
            .is_some_and(|b| b.name == "trial")));
    }

    #[test]
    fn a_branch_name_git_would_refuse_is_reported_rather_than_attempted() {
        let sandbox = Sandbox::new("bad-name");
        let mut tracker = sandbox.tracker();
        tracker.act(Action::CreateBranch {
            name: "has space".to_owned(),
            switch: true,
        });
        assert!(wait_for(&mut tracker, |t| t.error().is_some()));
        let message = tracker.error().expect("a message").to_owned();
        assert!(
            message.contains("spaces"),
            "the complaint should name the problem, got {message:?}"
        );
    }

    #[test]
    fn deleting_a_branch_removes_it_from_the_listing() {
        let sandbox = Sandbox::new("delete");
        let mut tracker = sandbox.tracker();
        tracker.act(Action::CreateBranch {
            name: "doomed".to_owned(),
            switch: false,
        });
        assert!(wait_for(&mut tracker, |t| t.branches().len() == 2));

        tracker.act(Action::DeleteBranch {
            name: "doomed".to_owned(),
            force: false,
        });
        assert!(wait_for(&mut tracker, |t| t.branches().len() == 1));
        assert_eq!(tracker.error(), None);
    }

    /// A remote operation blocks the one worker, so the interface has to be
    /// able to say what it is waiting for rather than appearing to have stopped.
    #[test]
    fn a_remote_operation_reports_that_it_is_running() {
        let sandbox = Sandbox::new("busy");
        let mut tracker = sandbox.tracker();
        assert_eq!(tracker.busy(), None);

        tracker.act(Action::Fetch("origin".to_owned()));
        assert_eq!(
            tracker.busy(),
            Some("Fetching\u{2026}"),
            "the moment it is asked for, not when it starts"
        );

        // There is no `origin`, so it fails — and the status that follows any
        // action is what clears the flag either way.
        assert!(wait_for(&mut tracker, |t| t.busy().is_none()));
        assert!(tracker.error().is_some(), "and it says what went wrong");
    }

    #[test]
    fn pushing_to_a_bare_repository_next_door_works_and_reports_what_it_did() {
        let sandbox = Sandbox::new("push");
        let _origin = sandbox.with_origin();
        let mut tracker = sandbox.tracker();

        tracker.refresh_branches();
        assert!(wait_for(&mut tracker, |t| t.branches_known()));
        assert_eq!(tracker.remotes(), ["origin"]);

        tracker.act(Action::Push {
            remote: "origin".to_owned(),
            branch: "trial".to_owned(),
            set_upstream: true,
        });
        assert!(
            wait_for(&mut tracker, |t| t.busy().is_none() && t.branches_known()),
            "the push should finish"
        );
        assert_eq!(tracker.error(), None, "and succeed");

        // The upstream is now known, and the two are level.
        assert!(wait_for(&mut tracker, |t| t
            .current_branch()
            .is_some_and(|b| b.upstream.as_deref() == Some("origin/trial"))));
        let branch = tracker.current_branch().expect("branch");
        assert_eq!((branch.ahead, branch.behind), (0, 0));
    }

    #[test]
    fn a_commit_after_pushing_shows_as_ahead() {
        let sandbox = Sandbox::new("ahead");
        let _origin = sandbox.with_origin();
        let mut tracker = sandbox.tracker();
        tracker.act(Action::Push {
            remote: "origin".to_owned(),
            branch: "trial".to_owned(),
            set_upstream: true,
        });
        assert!(wait_for(&mut tracker, |t| t
            .current_branch()
            .is_some_and(|b| b.upstream.is_some())));

        sandbox.write("first.txt", "more\n");
        tracker.act(Action::Stage(vec!["first.txt".to_owned()]));
        assert!(wait_for(&mut tracker, |t| t.status().staged().count() == 1));
        tracker.act(Action::Commit {
            message: "one more".to_owned(),
            amend: false,
        });

        assert!(
            wait_for(&mut tracker, |t| t
                .current_branch()
                .is_some_and(|b| b.ahead == 1)),
            "committing should leave the branch one ahead of its upstream"
        );
    }

    #[test]
    fn a_tracker_with_no_repository_lists_no_branches() {
        let mut tracker = tracker();
        tracker.refresh_branches();
        tracker.poll();
        assert!(!tracker.branches_known());
        assert_eq!(tracker.branches(), []);
        assert_eq!(tracker.current_branch(), None);
    }

    #[test]
    fn changing_project_forgets_the_branches() {
        let sandbox = Sandbox::new("forget-branches");
        let mut tracker = sandbox.tracker();
        tracker.refresh_branches();
        assert!(wait_for(&mut tracker, |t| t.branches_known()));

        tracker.set_project(None);
        assert!(!tracker.branches_known());
        assert_eq!(tracker.branches(), []);
        assert_eq!(tracker.remotes(), Vec::<String>::new());
        assert_eq!(tracker.busy(), None);
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
