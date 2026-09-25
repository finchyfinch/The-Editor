//! In-file and project-wide search.
//!
//! `query` is the matcher both share: literal or regex, case and whole-word.
//! `project` walks the tree on a worker thread and streams results as it
//! finds them, cancellably; `files` is the walk Go to File uses. Replace works
//! in the open file only. PLAN.md §3.7 describes a project-wide replace on the
//! ripgrep engine, applied as one undoable step; neither has been built.

pub mod files;
pub mod project;
pub mod query;

// Still to come.
//
// pub mod project;     // streaming walker + searcher over a whole tree
// pub mod replace;     // preview, per-match exclusion, atomic multi-file apply
