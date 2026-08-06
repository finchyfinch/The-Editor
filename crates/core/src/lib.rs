//! Text buffers, edits, undo history, selections and indentation.
//!
//! This crate is the model layer of The Editor. It knows nothing about egui,
//! about the filesystem beyond reading and writing bytes, or about language
//! servers. Everything in here is unit-testable without a window.
//!
//! # The central invariant
//!
//! All mutation of a [`Document`]'s text flows through a single `apply`
//! function taking a transaction and returning the resulting changes. Nothing
//! else may touch the rope directly. That choke point is what keeps the undo
//! history, the tree-sitter parse tree and the language server's document
//! version from ever drifting apart. See PLAN.md §2.3.

// This crate handles user data. A panic here loses someone's work.
#![deny(clippy::unwrap_used)]
#![deny(clippy::indexing_slicing)]

// M2 populates these. Declared now so the module boundaries are visible from
// the start rather than being discovered halfway through.
//
// pub mod buffer;      // Rope wrapper, line/offset conversions
// pub mod document;    // Document, encoding, line endings, dirty tracking
// pub mod edit;        // Transaction, Change, apply()
// pub mod history;     // undo/redo with coalescing
// pub mod selection;   // Selection, multi-cursor arithmetic
// pub mod indent;      // language-aware indentation engine
// pub mod encoding;    // detection and transcoding

/// The version of The Editor, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
