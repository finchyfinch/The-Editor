//! In-file and project-wide search and replace.
//!
//! Project search runs the ripgrep engine (`grep-searcher` + `ignore`) on the
//! background runtime, streaming results so a large tree stays responsive and
//! remains cancellable. A project-wide replace is applied as a single undoable
//! operation. See PLAN.md §3.7.

// M5 populates these.
//
// pub mod query;       // SearchQuery: literal/regex, case, whole word, scope
// pub mod in_file;     // incremental match over a Rope
// pub mod project;     // streaming walker + searcher
// pub mod replace;     // preview, per-match exclusion, atomic multi-file apply
