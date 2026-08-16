//! What the repository says about the files being edited.
//!
//! Git is spoken to by running `git`, not by linking a library. That is the
//! same choice the virtual-environment dialog and the packages panel made, and
//! for a stronger reason here: the user's own git does the work, so their
//! credential helper, their `.gitconfig`, their hooks and their signing key all
//! apply. A library reimplements those and gets the edges subtly different.
//!
//! The one thing too slow for a subprocess is the gutter, which cannot spawn a
//! process per keystroke. So the committed version of a file is fetched once
//! and cached, and the comparison against the buffer happens in [`diff`].

//! [`tracker::Tracker`] is what the application holds: it owns the worker
//! thread, the cache, and the rule for when the cache stops being true.

pub mod blame;
pub mod diff;
pub mod log;
pub mod repo;
pub mod status;
pub mod tracker;
pub mod unified;
