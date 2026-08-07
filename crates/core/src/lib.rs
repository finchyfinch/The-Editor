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

pub mod document;
pub mod edit;
pub mod filename;
pub mod history;
pub mod selection;
pub mod word;

// Still to come.
//
// pub mod indent;      // M4: language-aware indentation engine

/// The version of The Editor, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
