//! Keeping unsaved work through a crash.
//!
//! The panic hook has always been able to write buffers out, but a panic is the
//! least of it: the editor can also be killed by the window manager, by a
//! driver fault in the GPU backend, by `taskkill`, or by the machine losing
//! power. None of those run a hook. What survives all of them is having already
//! written the text down.
//!
//! So every dirty buffer is copied to a recovery directory a couple of seconds
//! after it stops being typed into, and the copy is deleted the moment the real
//! file is saved. On the next start, anything still lying there is work that
//! never made it to disk.
//!
//! ## Telling a crash from another window
//!
//! Recovery files from an instance that is *still running* must not be offered
//! back — that is somebody's other window, mid-edit. Each session gets its own
//! directory containing an `alive` file, on which it holds an exclusive lock
//! for as long as it runs. The operating system releases that lock when the
//! process dies, however it dies, so another instance can tell the two apart by
//! trying to take it: succeed and the owner is gone.
//!
//! The lock is the whole mechanism where it works, and it works immediately —
//! which matters, because the moment people restart after a crash is the moment
//! after the crash. Where locking is unavailable (some network filesystems) the
//! file's modification time is kept fresh as a fallback, and a session is
//! presumed alive until that goes stale.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long after the last edit a buffer is copied aside.
///
/// Short enough that a crash costs a sentence rather than a session; long
/// enough that holding a key down does not write a file per character.
const DELAY: Duration = Duration::from_secs(2);

/// How often the fallback heartbeat is touched.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// A heartbeat older than this means the session is gone.
///
/// Only consulted where the lock could not be taken at all. Generously more
/// than [`HEARTBEAT`], because a machine that has been asleep or a session
/// stalled in a long synchronous save must not have its live buffers offered
/// to another window as crash debris.
const STALE: Duration = Duration::from_secs(60);

/// The first line of every recovery file, and the version of the format.
const MAGIC: &str = "The Editor recovery 1";

/// The per-session file that is locked to prove the session is running.
const ALIVE: &str = "alive";

/// One buffer that outlived its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recovered {
    /// Where it was meant to go. `None` for a buffer that was never saved.
    pub(crate) path: Option<PathBuf>,
    /// What to call it in the list when there is no path.
    pub(crate) name: String,
    pub(crate) text: String,
    /// The file this came from, so it can be deleted once dealt with.
    pub(crate) source: PathBuf,
}

/// The recovery directory for this session.
#[derive(Debug)]
pub(crate) struct Recovery {
    dir: PathBuf,
    /// The exclusive lock proving this session is running. Held open for the
    /// process's whole life and released by the operating system when it ends,
    /// which is what makes a crash detectable the instant it happens.
    ///
    /// Released by hand in [`Recovery::clear`], which is the one case where the
    /// process outlives the need for it: the handle is open on a file *inside*
    /// the directory being deleted.
    lock: Option<std::fs::File>,
    /// When the pending write is due, if anything is waiting.
    due: Option<Instant>,
    last_heartbeat: Instant,
    /// Document version last written, per recovery id, so an untouched buffer
    /// is not rewritten every time some other buffer changes.
    written: std::collections::HashMap<u64, u64>,
}

impl Recovery {
    /// Claim a directory for this session under `backups`.
    pub(crate) fn new(backups: &Path) -> Self {
        // Process id and start time together: pids are reused, and a reused pid
        // pointing at a stale directory would make live work look like debris.
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let dir = backups.join(format!("session-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();

        let recovery = Self {
            lock: None,
            dir,
            due: None,
            last_heartbeat: Instant::now(),
            written: std::collections::HashMap::new(),
        };
        recovery.touch_heartbeat();

        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(recovery.dir.join(ALIVE))
            .ok()
            .filter(|file| file.try_lock().is_ok());
        Self { lock, ..recovery }
    }

    /// Note that something was edited. The write happens [`DELAY`] later.
    pub(crate) fn mark_dirty(&mut self) {
        self.due = Some(Instant::now() + DELAY);
    }

    /// Whether a write is due now.
    pub(crate) fn is_due(&self) -> bool {
        self.due.is_some_and(|at| Instant::now() >= at)
    }

    /// When to wake up next, so the frame loop can sleep until then rather than
    /// spinning.
    pub(crate) fn next_wake(&self) -> Option<Duration> {
        self.due
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Called once a write has been done, whatever it wrote.
    pub(crate) fn settle(&mut self) {
        self.due = None;
    }

    /// Touch the heartbeat if it is time, so other instances can tell this
    /// session apart from a dead one.
    pub(crate) fn beat(&mut self) {
        if self.last_heartbeat.elapsed() < HEARTBEAT {
            return;
        }
        self.last_heartbeat = Instant::now();
        self.touch_heartbeat();
    }

    fn touch_heartbeat(&self) {
        // Rewriting the file is the portable way to move its modification time;
        // there is no `utimes` in `std`. Appending rather than truncating, so
        // the lock held on the same file is not disturbed -- and the file is
        // reset once it has grown enough to notice.
        let path = self.dir.join(ALIVE);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4096) {
            std::fs::write(&path, b".").ok();
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&path) {
            use std::io::Write as _;
            file.write_all(b".").ok();
        } else {
            std::fs::write(&path, b".").ok();
        }
    }

    /// Write one buffer aside, unless this exact version already has been.
    ///
    /// `version` is the document's edit counter. Comparing it means a session
    /// with thirty tabs open writes only the ones that actually changed.
    pub(crate) fn store(
        &mut self,
        id: u64,
        version: u64,
        path: Option<&Path>,
        name: &str,
        text: &str,
    ) {
        if self.written.get(&id) == Some(&version) {
            return;
        }
        let mut body = String::with_capacity(text.len() + 128);
        body.push_str(MAGIC);
        body.push('\n');
        body.push_str("path: ");
        if let Some(path) = path {
            body.push_str(&path.to_string_lossy());
        }
        body.push('\n');
        body.push_str("name: ");
        body.push_str(name);
        body.push_str("\n---\n");
        body.push_str(text);

        if std::fs::write(self.file_for(id), body.as_bytes()).is_ok() {
            self.written.insert(id, version);
        }
    }

    /// Forget a buffer: it was saved, or closed, and is no longer at risk.
    pub(crate) fn discard(&mut self, id: u64) {
        self.written.remove(&id);
        std::fs::remove_file(self.file_for(id)).ok();
    }

    fn file_for(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id:04}.recover"))
    }

    /// Remove this session's directory. Called on a clean exit — after which
    /// there is, by definition, nothing to recover.
    ///
    /// The lock goes first, and it has to: it is a handle on the `alive` file
    /// *inside* the directory, and Windows will not delete a directory that
    /// something still has open. Leaving it held made this a silent no-op
    /// there — the failure is discarded, `collect` skips a directory with no
    /// recovery files in it, and so nothing looked wrong while the backups
    /// folder filled up with one empty directory per run.
    pub(crate) fn clear(&mut self) {
        drop(self.lock.take());
        std::fs::remove_dir_all(&self.dir).ok();
    }

    #[cfg(test)]
    fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Everything left behind by sessions that are no longer running.
///
/// Skips `mine`, and skips any directory still being kept warm — that is
/// another window with unsaved work in it, not debris.
///
/// Tidies as it goes, the way [`dispose`] does: a dead session's directory with
/// no recovery files in it is removed. That is the state a clean exit leaves
/// behind on a machine where [`Recovery::clear`] could not delete the directory
/// -- which was every Windows machine until the lock was released first -- and
/// without sweeping them the folder stays as full as the bug left it. It is
/// also, on any platform, what a session that died before writing anything
/// leaves: an empty directory nothing will ever come back for.
pub(crate) fn collect(backups: &Path, mine: Option<&Path>) -> Vec<Recovered> {
    let Ok(entries) = std::fs::read_dir(backups) else {
        return Vec::new();
    };
    let mut found = Vec::new();

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || Some(dir.as_path()) == mine {
            continue;
        }
        if !dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("session-"))
        {
            continue;
        }
        if is_alive(&dir) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Counted rather than derived from what parsed: a `.recover` file this
        // version cannot read is still somebody's unsaved work, and a
        // directory holding one must survive to be read by hand or by a later
        // version. Only a directory with no recovery files at all is debris.
        let mut had_recovery_file = false;
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("recover") {
                continue;
            }
            had_recovery_file = true;
            if let Some(recovered) = read_one(&path) {
                found.push(recovered);
            }
        }
        // Old enough to be sure, as well as empty. A session claims its
        // directory and writes `alive` a few calls before it manages to lock
        // it, and in that window `is_alive` answers "dead" about a window that
        // is in fact opening: sweeping then would delete a live session's
        // directory out from under it and leave it writing recovery copies
        // into a folder that is not there. This is the same doubt [`STALE`]
        // already exists for, so it is the same answer -- and it costs
        // nothing, because the directories actually being swept up are from
        // previous runs and are minutes or months old.
        if !had_recovery_file && !heartbeat_is_fresh(&dir.join(ALIVE)) {
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    // Stable order, so the list does not shuffle between runs.
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.source.cmp(&b.source)));
    found
}

/// Whether the session that owns `dir` is still running.
///
/// Taking the lock is the answer wherever locking works: the owner holds it
/// until the process ends, and the operating system releases it on a crash just
/// as on a clean exit. The lock taken here is dropped immediately with the
/// file — this only asks the question.
///
/// The modification time is the fallback for filesystems that cannot lock. A
/// missing `alive` file counts as dead: it is written when the directory is
/// created, so its absence means that session never got going.
fn is_alive(dir: &Path) -> bool {
    let path = dir.join(ALIVE);
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    else {
        // Cannot even open it: on Windows that is itself a sign somebody has it
        // open exclusively. Fall back to the timestamp rather than guessing.
        return heartbeat_is_fresh(&path);
    };
    match file.try_lock() {
        // Nobody was holding it, so nobody is running.
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        // Locking is not supported here; the heartbeat is all there is.
        Err(std::fs::TryLockError::Error(_)) => heartbeat_is_fresh(&path),
    }
}

fn heartbeat_is_fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|age| age < STALE)
}

/// Parse one recovery file. Returns `None` for anything that is not one.
fn read_one(path: &Path) -> Option<Recovered> {
    let raw = std::fs::read_to_string(path).ok()?;
    let (header, text) = raw.split_once("\n---\n")?;
    let mut lines = header.lines();
    if lines.next()? != MAGIC {
        return None;
    }

    let mut file_path = None;
    let mut name = String::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("path: ") {
            if !rest.is_empty() {
                file_path = Some(PathBuf::from(rest));
            }
        } else if let Some(rest) = line.strip_prefix("name: ") {
            name = rest.to_owned();
        }
    }
    if name.is_empty() {
        name = "Untitled".to_owned();
    }

    Some(Recovered {
        path: file_path,
        name,
        text: text.to_owned(),
        source: path.to_path_buf(),
    })
}

/// Delete a recovery file once the user has decided what to do with it.
///
/// Removes the containing session directory too when it empties out, so the
/// backup folder does not fill with the skeletons of dead sessions.
pub(crate) fn dispose(source: &Path) {
    std::fs::remove_file(source).ok();
    let Some(dir) = source.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let leftover = entries
        .flatten()
        .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("recover"));
    if !leftover {
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("the-editor-recovery-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    #[test]
    fn a_stored_buffer_comes_back_with_its_path_and_text() {
        let backups = temp("roundtrip");
        let mut recovery = Recovery::new(&backups);
        recovery.store(
            1,
            7,
            Some(Path::new("/project/main.py")),
            "main.py",
            "print('hello')\n",
        );

        // Nothing comes back while the session is still beating.
        assert!(collect(&backups, Some(recovery.dir())).is_empty());

        // Pretend the session died: remove the heartbeat.
        drop(recovery); // as a crash would: the lock goes with the process
        let found = collect(&backups, None);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].path.as_deref(),
            Some(Path::new("/project/main.py"))
        );
        assert_eq!(found[0].name, "main.py");
        assert_eq!(found[0].text, "print('hello')\n");

        std::fs::remove_dir_all(&backups).ok();
    }

    /// The commonest thing to lose, and the one with no file to fall back on.
    #[test]
    fn a_buffer_that_was_never_saved_survives_with_no_path() {
        let backups = temp("untitled");
        let mut recovery = Recovery::new(&backups);
        recovery.store(3, 1, None, "Untitled 1", "notes to self\n");
        drop(recovery); // as a crash would: the lock goes with the process

        let found = collect(&backups, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, None);
        assert_eq!(found[0].name, "Untitled 1");
        assert_eq!(found[0].text, "notes to self\n");

        std::fs::remove_dir_all(&backups).ok();
    }

    /// Another window's unsaved work is not crash debris. Offering it back
    /// would mean two windows editing the same buffer.
    ///
    /// Note what this test does *not* do: sleep, or touch a clock. The live
    /// session is recognised because it is holding the lock, and that is what
    /// makes a crash detectable the moment it happens rather than a minute
    /// later — which is when people actually restart after one.
    #[test]
    fn a_running_session_is_left_alone_because_it_holds_the_lock() {
        let backups = temp("alive");
        let mut other = Recovery::new(&backups);
        other.store(1, 1, None, "Theirs", "in progress\n");

        // From a different session's point of view, with no `mine` to skip.
        assert!(
            collect(&backups, None).is_empty(),
            "a live session must not be treated as a crash"
        );

        // And the instant it goes away, its work is recoverable.
        drop(other);
        assert_eq!(collect(&backups, None).len(), 1);

        std::fs::remove_dir_all(&backups).ok();
    }

    #[test]
    fn saving_the_real_file_removes_the_recovery_copy() {
        let backups = temp("discard");
        let mut recovery = Recovery::new(&backups);
        recovery.store(1, 1, None, "a", "text\n");
        recovery.discard(1);
        drop(recovery);

        assert!(collect(&backups, None).is_empty());
        std::fs::remove_dir_all(&backups).ok();
    }

    /// Thirty tabs open and one being typed into should be one write, not
    /// thirty, or a large session writes megabytes every couple of seconds.
    #[test]
    fn an_unchanged_buffer_is_not_rewritten() {
        let backups = temp("versions");
        let mut recovery = Recovery::new(&backups);
        recovery.store(1, 5, None, "a", "first\n");
        let file = recovery.file_for(1);
        let first = std::fs::metadata(&file).and_then(|m| m.modified()).ok();

        std::thread::sleep(Duration::from_millis(50));
        recovery.store(1, 5, None, "a", "SHOULD NOT BE WRITTEN\n");
        let second = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        assert_eq!(first, second, "same version, so no write");

        recovery.store(1, 6, None, "a", "second\n");
        assert!(
            std::fs::read_to_string(&file)
                .expect("read")
                .ends_with("second\n"),
            "a new version does write"
        );

        std::fs::remove_dir_all(&backups).ok();
    }

    #[test]
    fn a_clean_exit_leaves_nothing_to_recover() {
        let backups = temp("clean-exit");
        let mut recovery = Recovery::new(&backups);
        recovery.store(1, 1, None, "a", "text\n");
        let dir = recovery.dir().to_path_buf();
        assert!(dir.is_dir(), "there is a directory to remove to begin with");

        recovery.clear();

        assert!(collect(&backups, None).is_empty());

        // Asserting on `collect` alone is not enough, and for a long time it
        // was all this test did. `collect` reports nothing to recover from a
        // directory with no recovery files in it whether that directory is
        // there or not -- so on Windows, where the removal was failing
        // silently against this session's own open lock, the observable
        // behaviour stayed correct while the backups folder grew by one empty
        // directory every single run.
        assert!(
            !dir.exists(),
            "the session directory itself is gone, not merely emptied"
        );
        assert_eq!(
            std::fs::read_dir(&backups)
                .expect("read backups")
                .flatten()
                .count(),
            0,
            "nothing whatever left behind in the backups folder"
        );
        std::fs::remove_dir_all(&backups).ok();
    }

    /// Backdate a session's heartbeat, so it reads as one from a previous run
    /// rather than one from a moment ago.
    fn age(dir: &Path) {
        let path = dir.join(ALIVE);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open alive");
        file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - STALE * 2))
            .expect("backdate");
    }

    /// The directories a machine has already accumulated are swept up on the
    /// next start, so fixing the leak also clears what the leak produced.
    #[test]
    fn a_dead_session_that_left_nothing_behind_is_swept_away() {
        let backups = temp("sweep");
        let debris = backups.join("session-999-0");
        std::fs::create_dir_all(&debris).expect("create dir");
        std::fs::write(debris.join(ALIVE), b".").expect("write");
        age(&debris);

        assert!(collect(&backups, None).is_empty());
        assert!(!debris.exists(), "an empty dead session is debris");

        std::fs::remove_dir_all(&backups).ok();
    }

    /// A directory that never got an `alive` file at all is a session that
    /// died between claiming its folder and announcing itself. Nothing is ever
    /// coming back for it, and there is no timestamp to wait on.
    #[test]
    fn a_session_that_never_announced_itself_is_swept_too() {
        let backups = temp("sweep-unborn");
        let debris = backups.join("session-997-0");
        std::fs::create_dir_all(&debris).expect("create dir");

        assert!(collect(&backups, None).is_empty());
        assert!(!debris.exists());

        std::fs::remove_dir_all(&backups).ok();
    }

    /// A window that has only just opened has not written anything yet, and on
    /// some filesystems cannot prove it is running by holding a lock. Sweeping
    /// it would delete a live session's directory out from under it — so an
    /// empty directory is left alone until it is old enough to be sure about.
    #[test]
    fn a_session_too_young_to_judge_is_left_alone_even_though_it_is_empty() {
        let backups = temp("sweep-young");
        let opening = backups.join("session-996-0");
        std::fs::create_dir_all(&opening).expect("create dir");
        // Written just now, and nobody holding the lock: exactly the state a
        // session is in for the moment between claiming its folder and locking
        // it.
        std::fs::write(opening.join(ALIVE), b".").expect("write");

        assert!(collect(&backups, None).is_empty());
        assert!(
            opening.is_dir(),
            "a fresh heartbeat means it may be a window that is still opening"
        );

        // And once it is plainly from another run, it goes.
        age(&opening);
        assert!(collect(&backups, None).is_empty());
        assert!(!opening.exists());

        std::fs::remove_dir_all(&backups).ok();
    }

    /// Sweeping must not take unsaved work with it. A recovery file this
    /// version cannot parse is still somebody's text, and the directory has to
    /// survive for it to be read by hand -- or by a later version that can.
    #[test]
    fn a_directory_holding_an_unreadable_recovery_file_is_not_swept() {
        let backups = temp("sweep-keeps");
        let dir = backups.join("session-998-0");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join(ALIVE), b".").expect("write");
        std::fs::write(dir.join("a.recover"), b"truncated by the crash").expect("write");

        assert!(collect(&backups, None).is_empty(), "nothing parses");
        assert!(
            dir.join("a.recover").is_file(),
            "but the file is still there to be looked at"
        );

        std::fs::remove_dir_all(&backups).ok();
    }

    /// A live session's directory is never swept, even before it has written
    /// anything: it is empty because nothing has been typed into that window
    /// yet, not because the session is gone.
    #[test]
    fn a_running_session_with_nothing_written_yet_is_left_alone() {
        let backups = temp("sweep-live");
        let live = Recovery::new(&backups);
        let dir = live.dir().to_path_buf();

        assert!(collect(&backups, None).is_empty());
        assert!(dir.is_dir(), "the session is still running");

        drop(live);
        std::fs::remove_dir_all(&backups).ok();
    }

    /// Files with the wrong magic, or truncated by the crash that produced
    /// them, must be skipped rather than restored as garbage.
    #[test]
    fn a_file_that_is_not_a_recovery_file_is_ignored() {
        let backups = temp("junk");
        let dir = backups.join("session-999-0");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("a.recover"), b"just some text").expect("write");
        std::fs::write(dir.join("b.recover"), b"wrong magic\n---\nbody").expect("write");
        std::fs::write(dir.join("notes.txt"), b"ignored").expect("write");

        assert!(collect(&backups, None).is_empty());
        std::fs::remove_dir_all(&backups).ok();
    }

    /// Text containing the header separator must survive: a Python file can
    /// perfectly well contain a line of three dashes.
    #[test]
    fn text_containing_the_separator_round_trips_intact() {
        let backups = temp("separator");
        let body = "a\n---\nb\n---\nc\n";
        let mut recovery = Recovery::new(&backups);
        recovery.store(1, 1, None, "notes.md", body);
        drop(recovery);

        let found = collect(&backups, None);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].text, body,
            "only the first separator ends the header"
        );

        std::fs::remove_dir_all(&backups).ok();
    }

    #[test]
    fn disposing_of_the_last_file_removes_the_session_directory() {
        let backups = temp("dispose");
        let mut recovery = Recovery::new(&backups);
        recovery.store(1, 1, None, "a", "one\n");
        recovery.store(2, 1, None, "b", "two\n");
        let session = recovery.dir().to_path_buf();
        drop(recovery);

        let found = collect(&backups, None);
        assert_eq!(found.len(), 2);

        dispose(&found[0].source);
        assert!(session.is_dir(), "one file left, so the directory stays");
        dispose(&found[1].source);
        assert!(!session.exists(), "empty now, so it goes");

        std::fs::remove_dir_all(&backups).ok();
    }

    #[test]
    fn nothing_is_due_until_something_is_edited() {
        let backups = temp("timing");
        let mut recovery = Recovery::new(&backups);
        assert!(!recovery.is_due());
        assert_eq!(recovery.next_wake(), None);

        recovery.mark_dirty();
        assert!(!recovery.is_due(), "not immediately");
        assert!(recovery.next_wake().is_some_and(|d| d <= DELAY));

        recovery.settle();
        assert!(!recovery.is_due());

        std::fs::remove_dir_all(&backups).ok();
    }
}
