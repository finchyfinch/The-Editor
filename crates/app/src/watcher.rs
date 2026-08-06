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

/// Watches one project folder.
pub(crate) struct Watcher {
    /// Dropping this stops the watch, so it must be kept alive.
    _debouncer: Debouncer<notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>,
    events: Receiver<Changes>,
    root: PathBuf,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").field("root", &self.root).finish()
    }
}

impl Watcher {
    /// Start watching `root` recursively.
    ///
    /// # Errors
    /// If the platform watcher cannot be created or the path cannot be watched.
    pub(crate) fn new(root: &Path) -> anyhow::Result<Self> {
        let (tx, events) = channel();

        let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
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
            }
        })?;

        debouncer.watch(root, RecursiveMode::Recursive)?;

        Ok(Self {
            _debouncer: debouncer,
            events,
            root: root.to_path_buf(),
        })
    }

    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
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

        let watcher = Watcher::new(&dir).expect("watcher starts");
        assert_eq!(watcher.root(), dir);

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

        let watcher = Watcher::new(&dir).expect("watcher starts");
        let started = std::time::Instant::now();
        assert!(watcher.drain().is_empty());
        assert!(started.elapsed() < Duration::from_millis(100));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watching_a_path_that_does_not_exist_is_an_error_not_a_panic() {
        assert!(Watcher::new(Path::new("/nonexistent/project/xyzzy")).is_err());
    }
}
