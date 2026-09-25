//! Language Server Protocol client and server lifecycle management.
//!
//! One thread pair per server, JSON-RPC over stdio. Several servers may serve
//! one language — Python uses `ruff` for linting alongside a type checker —
//! with merged, source-tagged diagnostics.
//!
//! **Threads rather than tokio**, departing from PLAN.md §1 D5. Everything else
//! that talks to a child process is already built this way, and a handful of
//! language servers does not need a scheduler; one concurrency model in the
//! codebase is worth more than the efficiency of a second.
//!
//! Two things this crate must get right, because both are silent failures:
//!
//! - **Degradation.** With no server installed the editor still works, and
//!   noticing a server is absent costs one filesystem check rather than one per
//!   keystroke. See PLAN.md §3.6.
//! - **Cleanup.** A server left running holds a workspace index and a few
//!   hundred megabytes, so every exit path stops them — including `Drop`.

// This crate spawns and reaps external processes. Panics leak them.
#![deny(clippy::unwrap_used)]

pub mod diagnostics;
pub mod position;
pub mod registry;
pub mod server;
pub mod session;
pub mod transport;
