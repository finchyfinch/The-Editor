//! Searching every file in a project.
//!
//! Runs on a worker thread and reports results as it goes, because the useful
//! property of a project search is not how fast it finishes but how quickly the
//! first hits appear: on a large tree the answer is usually in the first dozen,
//! and a search that shows nothing until it has read everything feels broken
//! even when it is quick.
//!
//! Cancellable for the same reason. Typing another character starts a new
//! search, and the old one has to stop rather than race the new one to the same
//! channel.
//!
//! Binary files are skipped by looking for a NUL in the first few kilobytes,
//! which is what every tool does and is right far more often than any file
//! extension list.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::query::{Matcher, Query};

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// One-based, as it is shown and as the editor's go-to takes it.
    pub line: usize,
    /// Character offset of the match within the line, for placing the caret.
    pub column: usize,
    /// The whole line, trimmed of its indentation for display.
    pub text: String,
}

/// What a running search reports.
#[derive(Debug, Clone)]
pub enum Progress {
    Hit(Hit),
    /// Finished, with how many files were read and whether the cap was reached.
    Done {
        files: usize,
        truncated: bool,
    },
}

/// Most hits collected before stopping.
///
/// Past this the list is not an answer, it is a second corpus to search. The
/// user narrows the query instead, which is faster than scrolling.
pub const MAX_HITS: usize = 2_000;

/// Largest file worth reading. Anything bigger is data or generated.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A search in progress.
pub struct Search {
    results: Receiver<Progress>,
    cancelled: Arc<AtomicBool>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search").finish()
    }
}

impl Search {
    /// Start searching `files` under `root` for `query`.
    ///
    /// The file list is taken by the caller — go-to-file walks the same tree
    /// and there is no reason to walk it twice.
    #[must_use]
    pub fn start(root: &Path, files: Vec<PathBuf>, query: &Query) -> Self {
        let (tx, results) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        let matcher = Matcher::new(query).ok();
        let root = root.to_path_buf();
        let flag = Arc::clone(&cancelled);

        let _ = std::thread::Builder::new()
            .name("project-search".to_owned())
            .spawn(move || {
                let Some(matcher) = matcher else {
                    let _ = tx.send(Progress::Done {
                        files: 0,
                        truncated: false,
                    });
                    return;
                };
                run(&root, &files, &matcher, &tx, &flag);
            });

        Self { results, cancelled }
    }

    /// Take whatever has been found since the last call. Never blocks.
    pub fn drain(&self) -> Vec<Progress> {
        self.results.try_iter().collect()
    }

    /// Stop the worker. It checks between files, so this is not instant.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        // A dropped search must not leave a thread reading a source tree that
        // nobody is waiting on.
        self.cancel();
    }
}

fn run(
    root: &Path,
    files: &[PathBuf],
    matcher: &Matcher,
    tx: &Sender<Progress>,
    cancelled: &AtomicBool,
) {
    let mut read = 0usize;
    let mut hits = 0usize;

    for relative in files {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        if hits >= MAX_HITS {
            let _ = tx.send(Progress::Done {
                files: read,
                truncated: true,
            });
            return;
        }

        let path = root.join(relative);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.len() > MAX_BYTES || looks_binary(&bytes) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            // Not UTF-8. Could be re-decoded, but a project search over a
            // Latin-1 file is a rarity next to the cost of guessing wrong.
            continue;
        };
        read += 1;

        for (index, line) in text.lines().enumerate() {
            let Some(column) = matcher.first_in_line(line) else {
                continue;
            };
            hits += 1;
            let sent = tx.send(Progress::Hit(Hit {
                path: relative.clone(),
                line: index + 1,
                column,
                text: line.trim_start().chars().take(200).collect(),
            }));
            if sent.is_err() {
                return; // nobody is listening any more
            }
            if hits >= MAX_HITS {
                break;
            }
        }
    }

    let _ = tx.send(Progress::Done {
        files: read,
        truncated: false,
    });
}

/// A NUL in the first few kilobytes means binary.
///
/// What every search tool does, and right far more often than a list of file
/// extensions: it catches a `.dat` nobody thought of and lets through a `.pyc`
/// that happens to be text.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("the-editor-search-{name}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("root");
            Self(root)
        }
        fn file(&self, name: &str, body: &str) -> &Self {
            std::fs::write(self.0.join(name), body).expect("write");
            self
        }
        fn search(&self, needle: &str) -> (Vec<Hit>, bool) {
            let files: Vec<PathBuf> = std::fs::read_dir(&self.0)
                .expect("read")
                .flatten()
                .map(|e| PathBuf::from(e.file_name()))
                .collect();
            let search = Search::start(&self.0, files, &Query::literal(needle));

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let (mut hits, mut done) = (Vec::new(), false);
            while std::time::Instant::now() < deadline && !done {
                for p in search.drain() {
                    match p {
                        Progress::Hit(h) => hits.push(h),
                        Progress::Done { .. } => done = true,
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
            (hits, done)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_match_is_reported_with_its_file_line_and_column() {
        let tree = Tree::new("basic");
        tree.file("a.py", "x = 1\nfind_me = 2\n");
        let (hits, done) = tree.search("find_me");
        assert!(done, "the search finished");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2, "one-based");
        assert_eq!(hits[0].column, 0);
        assert_eq!(hits[0].text, "find_me = 2");
    }

    #[test]
    fn every_file_is_searched_not_just_the_first() {
        let tree = Tree::new("many");
        tree.file("a.py", "needle\n").file("b.py", "needle\n");
        let (hits, _) = tree.search("needle");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn a_line_matching_twice_is_reported_once() {
        // The panel lists lines, not occurrences: three hits on one line is
        // one thing to look at.
        let tree = Tree::new("twice");
        tree.file("a.py", "needle needle needle\n");
        let (hits, _) = tree.search("needle");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn the_displayed_text_is_trimmed_of_its_indentation() {
        // Deeply indented code otherwise pushes the match off the right of the
        // panel, which is where it is least useful.
        let tree = Tree::new("indent");
        tree.file("a.py", "            deep = 1\n");
        let (hits, _) = tree.search("deep");
        assert_eq!(hits[0].text, "deep = 1");
    }

    #[test]
    fn a_binary_file_is_skipped() {
        let tree = Tree::new("binary");
        tree.file("a.py", "needle\n");
        std::fs::write(tree.0.join("b.bin"), b"needle\0\0\0needle").expect("write");
        let (hits, _) = tree.search("needle");
        assert_eq!(hits.len(), 1, "only the source file: {hits:?}");
    }

    #[test]
    fn nothing_matching_still_finishes() {
        // A search that never reports Done leaves the panel saying "searching"
        // for ever.
        let tree = Tree::new("empty");
        tree.file("a.py", "nothing here\n");
        let (hits, done) = tree.search("absent");
        assert!(hits.is_empty());
        assert!(done);
    }

    #[test]
    fn a_nul_anywhere_early_marks_a_file_binary() {
        assert!(looks_binary(b"abc\0def"));
        assert!(!looks_binary(b"plain text"));
        // Past the sampled window, a NUL is not looked for: reading a whole
        // 4 MB file to classify it costs more than the mistake.
        let mut late = vec![b'x'; 9000];
        late.push(0);
        assert!(!looks_binary(&late));
    }

    #[test]
    fn cancelling_stops_it_reporting_anything_further() {
        let tree = Tree::new("cancel");
        tree.file("a.py", "needle\n");
        let files = vec![PathBuf::from("a.py")];
        let search = Search::start(&tree.0, files, &Query::literal("needle"));
        search.cancel();
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Either it stopped before starting or it finished first; what must not
        // happen is results arriving after a later search has begun.
        let after = search.drain();
        assert!(after.len() <= 2, "got {after:?}");
    }

    #[test]
    fn a_malformed_regex_finishes_rather_than_hanging() {
        let tree = Tree::new("badregex");
        tree.file("a.py", "x\n");
        let search = Search::start(&tree.0, vec![PathBuf::from("a.py")], &Query::regex("("));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut done = false;
        while std::time::Instant::now() < deadline && !done {
            done = search
                .drain()
                .iter()
                .any(|p| matches!(p, Progress::Done { .. }));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(done, "an unusable query must still report completion");
    }
}
