//! Language Server Protocol client and server lifecycle management.
//!
//! One tokio task per server, JSON-RPC over stdio. Supports several servers
//! per language (Python uses `ruff` for lint/format alongside `basedpyright`
//! for types) with merged, source-tagged diagnostics.
//!
//! Two things this module must get right, because both are silent failures:
//!
//! - **Degradation.** With no server installed the editor still works. See the
//!   ladder in PLAN.md §3.6 — buffer words, keywords and tree-sitter symbols.
//! - **Process cleanup.** On Windows, killing the child is not enough;
//!   `rust-analyzer` spawns its own children and they outlive us. Servers must
//!   be assigned to a Job Object so the whole tree dies with the editor.

// This crate spawns and reaps external processes. Panics leak them.
#![deny(clippy::unwrap_used)]

// M6 populates these.
//
// pub mod client;      // LspClient: request/response/notification plumbing
// pub mod transport;   // JSON-RPC framing over stdio
// pub mod registry;    // which server for which language, discovery on PATH
// pub mod lifecycle;   // spawn, initialize, shutdown, crash restart w/ backoff
// pub mod diagnostics; // merge and route publishDiagnostics
// pub mod fallback;    // the no-server completion ladder
