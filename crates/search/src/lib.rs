//! In-file and project-wide search and replace.
//!
//! Project search runs the ripgrep engine (`grep-searcher` + `ignore`) on the
//! background runtime, streaming results so a large tree stays responsive and
//! remains cancellable. A project-wide replace is applied as a single undoable
//! operation. See PLAN.md §3.7.

pub mod files;
pub mod project;
pub mod query;

// Still to come.
//
// pub mod project;     // streaming walker + searcher over a whole tree
// pub mod replace;     // preview, per-match exclusion, atomic multi-file apply
