//! Noticing when files change outside The Editor.
//!
//! Two things depend on this: the explorer showing what is really on disk, and
//! open documents not silently drifting from the file they came from. The
//! second is the one that costs people work — editing a file that has since
//! been rewritten by `git checkout` and then saving over it.
//!
//! Events are debounced. A single save from another program can produce half a
//! dozen filesystem events, and a `git checkout` produces thousands; reacting
//! to each one would re-read the tree continuously.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, Debouncer, new_debouncer};

/// How long to wait for a burst of events to settle.
///
/// Long enough to collapse the several events one save produces, short enough
/// that the tree does not feel stale.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// A settled batch of filesystem changes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Changes {
    /// Paths that were created, modified, renamed or removed.
    pub(crate) touched: Vec<PathBuf>,
    /// True when something structural happened and the tree needs re-reading.
    pub(crate) structural: bool,
}

impl Changes {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.touched.is_empty() && !self.structural
    }
}

/// Watches one project folder, plus the directories holding any open files
/// that are not inside it.
///
/// The second part matters more than it sounds. Open a file with no folder
/// open, or a file from somewhere else entirely, and a project-only watch never
/// sees it change — so the one document you are actually looking at is the one
/// document nothing is watching.
pub(crate) struct Watcher {
    /// Dropping this stops the watch, so it must be kept alive.
    debouncer: Debouncer<notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>,
    events: Receiver<Changes>,
    root: Option<PathBuf>,
    /// Directories watched non-recursively for the sake of individual files.
    /// Kept so they can be un-watched when the last file in one is closed.
    loose: Vec<PathBuf>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("root", &self.root)
            .field("loose", &self.loose.len())
            .finish()
    }
}

impl Watcher {
    /// Start a watcher with nothing watched yet.
    ///
    /// `wake` is the egui context. Changes arrive on the watcher's own thread,
    /// and an idle egui draws no frames — so without waking it the event sits
    /// in the channel until something else happens to cause a repaint. Which is
    /// to say: the file you are looking at changes and the editor tells you
    /// about it only once you touch the keyboard.
    ///
    /// # Errors
    /// If the platform watcher cannot be created.
    pub(crate) fn new(wake: Option<eframe::egui::Context>) -> anyhow::Result<Self> {
        let (tx, events) = channel();

        let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            let changes = match result {
                Ok(events) => summarise(&events),
                // A watch error is usually a directory disappearing
                // underneath us. Treat it as "re-read everything" rather
                // than as fatal.
                Err(_) => Changes {
                    touched: Vec::new(),
                    structural: true,
                },
            };
            if !changes.is_empty() {
                let _ = tx.send(changes);
                if let Some(ctx) = wake.as_ref() {
                    ctx.request_repaint();
                }
            }
        })?;

        Ok(Self {
            debouncer,
            events,
            root: None,
            loose: Vec::new(),
        })
    }

    /// Watch `root` and everything under it, replacing any previous root.
    ///
    /// # Errors
    /// If the path cannot be watched.
    pub(crate) fn set_root(&mut self, root: &Path) -> anyhow::Result<()> {
        if let Some(old) = self.root.take() {
            self.debouncer.unwatch(&old).ok();
        }
        self.debouncer.watch(root, RecursiveMode::Recursive)?;
        self.root = Some(root.to_path_buf());
        // Files that are now inside the project no longer need their own watch,
        // and leaving it would report every change twice.
        self.prune_loose();
        Ok(())
    }

    #[must_use]
    pub(crate) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Make sure every one of `files` is covered by some watch.
    ///
    /// Watches the *directory*, not the file, and non-recursively. Watching a
    /// file directly misses the commonest way files change: written to a
    /// temporary and renamed over the top, which replaces the thing being
    /// watched rather than modifying it. The directory sees that as a rename
    /// and reports it.
    ///
    /// Directories no longer holding any open file are dropped, so closing
    /// tabs does not leave watches accumulating for the session's lifetime.
    pub(crate) fn set_files(&mut self, files: &[PathBuf]) {
        let mut wanted: Vec<PathBuf> = files
            .iter()
            .filter_map(|f| f.parent())
            .filter(|dir| !self.covered_by_root(dir))
            .map(Path::to_path_buf)
            .collect();
        wanted.sort_unstable();
        wanted.dedup();

        for dir in &self.loose {
            if !wanted.contains(dir) {
                self.debouncer.unwatch(dir).ok();
            }
        }
        for dir in &wanted {
            if !self.loose.contains(dir) {
                // A directory that has since been removed is not an error worth
                // reporting: the file in it will show as deleted anyway.
                self.debouncer.watch(dir, RecursiveMode::NonRecursive).ok();
            }
        }
        self.loose = wanted;
    }

    /// Whether the recursive project watch already covers `dir`.
    fn covered_by_root(&self, dir: &Path) -> bool {
        self.root.as_ref().is_some_and(|root| dir.starts_with(root))
    }

    /// Drop loose watches that the project watch has taken over.
    fn prune_loose(&mut self) {
        let mut kept = Vec::new();
        for dir in std::mem::take(&mut self.loose) {
            if self.covered_by_root(&dir) {
                self.debouncer.unwatch(&dir).ok();
            } else {
                kept.push(dir);
            }
        }
        self.loose = kept;
    }

    /// Take everything that has settled since the last call. Never blocks.
    pub(crate) fn drain(&self) -> Changes {
        let mut all = Changes::default();
        while let Ok(changes) = self.events.try_recv() {
            all.structural |= changes.structural;
            all.touched.extend(changes.touched);
        }
        all.touched.sort_unstable();
        all.touched.dedup();
        all
    }
}

/// Collapse a batch of debounced events into what the application cares about.
fn summarise(events: &[notify_debouncer_full::DebouncedEvent]) -> Changes {
    use notify::EventKind;

    let mut changes = Changes::default();
    for event in events {
        // Ignore our own noise: the atomic-save temporary files, and anything
        // inside directories nobody wants to watch. Without this, saving a file
        // makes the tree flicker as the temporary appears and vanishes.
        if event.paths.iter().any(|p| is_noise(p)) {
            continue;
        }
        match event.kind {
            EventKind::Create(_) | EventKind::Remove(_) => {
                changes.structural = true;
                changes.touched.extend(event.paths.iter().cloned());
            }
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
                changes.structural = true;
                changes.touched.extend(event.paths.iter().cloned());
            }
            EventKind::Modify(_) => {
                changes.touched.extend(event.paths.iter().cloned());
            }
            _ => {}
        }
    }
    changes
}

/// Paths whose changes are not worth reacting to.
fn is_noise(path: &Path) -> bool {
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        "target",
        "node_modules",
        "__pycache__",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
    ];

    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| IGNORED_DIRS.contains(&name))
    }) || path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_noise_and_temporary_files_are_ignored() {
        // A save writes `<file>.<ext>.tmp` and renames it; reacting to that
        // makes the tree flicker on every save.
        assert!(is_noise(Path::new("/project/src/main.rs.tmp")));
        assert!(is_noise(Path::new("/project/.git/index")));
        assert!(is_noise(Path::new("/project/target/debug/thing")));
        assert!(is_noise(Path::new("/project/x/__pycache__/y.pyc")));
        assert!(is_noise(Path::new("/project/node_modules/left-pad/x.js")));

        assert!(!is_noise(Path::new("/project/src/main.rs")));
        assert!(!is_noise(Path::new("/project/README.md")));
    }

    #[test]
    fn a_path_merely_containing_a_noise_word_is_not_ignored() {
        // `targeting.py` is not `target/`.
        assert!(!is_noise(Path::new("/project/targeting.py")));
        assert!(!is_noise(Path::new("/project/git_helpers.py")));
    }

    #[test]
    fn empty_changes_are_recognised() {
        assert!(Changes::default().is_empty());
        assert!(
            !Changes {
                structural: true,
                ..Changes::default()
            }
            .is_empty()
        );
    }

    /// The watcher is a thin wrapper over `notify`, so what is worth testing is
    /// that it actually reports a real change on this platform.
    #[test]
    fn a_real_file_change_is_reported() {
        let dir = std::env::temp_dir().join("the-editor-watcher-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create dir");

        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_root(&dir).expect("root is watchable");
        assert_eq!(watcher.root(), Some(dir.as_path()));

        // Give the platform watcher a moment to register before changing
        // anything, or the event can be missed entirely.
        std::thread::sleep(Duration::from_millis(300));
        let target = dir.join("appeared.txt");
        std::fs::write(&target, b"hello").expect("write");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = Changes::default();
        while std::time::Instant::now() < deadline {
            let batch = watcher.drain();
            seen.structural |= batch.structural;
            seen.touched.extend(batch.touched);
            if seen.touched.iter().any(|p| p.ends_with("appeared.txt")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            seen.touched.iter().any(|p| p.ends_with("appeared.txt")),
            "the new file was not reported: {seen:?}"
        );
        assert!(seen.structural, "creating a file is a structural change");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn draining_an_idle_watcher_returns_nothing_promptly() {
        let dir = std::env::temp_dir().join("the-editor-watcher-idle");
        std::fs::create_dir_all(&dir).expect("create dir");

        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_root(&dir).expect("root is watchable");
        let started = std::time::Instant::now();
        assert!(watcher.drain().is_empty());
        assert!(started.elapsed() < Duration::from_millis(100));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watching_a_path_that_does_not_exist_is_an_error_not_a_panic() {
        let mut watcher = Watcher::new(None).expect("watcher starts");
        assert!(
            watcher
                .set_root(Path::new("/nonexistent/project/xyzzy"))
                .is_err()
        );
        assert_eq!(watcher.root(), None, "a failed watch must not be recorded");
    }

    /// The case a project-only watch misses entirely: a file opened from
    /// outside the project, or with no project open at all. That is often the
    /// only document on screen, so it is the worst one to leave unwatched.
    #[test]
    fn a_file_outside_the_project_is_watched_through_its_own_directory() {
        let dir = std::env::temp_dir().join("the-editor-watcher-loose");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create dir");
        let file = dir.join("loose.txt");
        std::fs::write(&file, b"before").expect("write");

        // No root at all: this is the "opened a single file" case.
        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_files(std::slice::from_ref(&file));

        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&file, b"after").expect("rewrite");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            seen.extend(watcher.drain().touched);
            if seen.iter().any(|p| p.ends_with("loose.txt")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            seen.iter().any(|p| p.ends_with("loose.txt")),
            "the loose file was not watched: {seen:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Closing every tab in a directory should release its watch, or a long
    /// session accumulates watches for files nobody has open any more.
    #[test]
    fn a_directory_is_unwatched_once_no_open_file_needs_it() {
        let dir = std::env::temp_dir().join("the-editor-watcher-release");
        std::fs::create_dir_all(&dir).expect("create dir");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").expect("write");

        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_files(std::slice::from_ref(&file));
        assert_eq!(watcher.loose.len(), 1);

        watcher.set_files(&[]);
        assert!(
            watcher.loose.is_empty(),
            "the watch should have been dropped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file inside the project is already covered recursively. Watching its
    /// directory as well would report every change to it twice.
    #[test]
    fn a_file_inside_the_project_gets_no_second_watch() {
        let dir = std::env::temp_dir().join("the-editor-watcher-inside");
        std::fs::create_dir_all(dir.join("src")).expect("create dirs");

        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_root(&dir).expect("root is watchable");
        watcher.set_files(&[dir.join("src").join("main.rs")]);

        assert!(
            watcher.loose.is_empty(),
            "the recursive project watch already covers this"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opening a folder that contains an already-open loose file should take
    /// the file over rather than leaving both watches in place.
    #[test]
    fn opening_the_project_takes_over_watches_for_files_now_inside_it() {
        let dir = std::env::temp_dir().join("the-editor-watcher-takeover");
        std::fs::create_dir_all(&dir).expect("create dir");
        let file = dir.join("b.txt");
        std::fs::write(&file, b"x").expect("write");

        let mut watcher = Watcher::new(None).expect("watcher starts");
        watcher.set_files(std::slice::from_ref(&file));
        assert_eq!(watcher.loose.len(), 1);

        watcher.set_root(&dir).expect("root is watchable");
        assert!(watcher.loose.is_empty(), "the project watch covers it now");

        std::fs::remove_dir_all(&dir).ok();
    }
}
