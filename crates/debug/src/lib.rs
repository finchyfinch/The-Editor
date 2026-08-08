//! Debugging Python, over the Debug Adapter Protocol.
//!
//! The adapter is `debugpy`, run as `python -m debugpy.adapter`, which speaks
//! DAP on stdio. DAP frames its messages exactly as LSP does — a
//! `Content-Length` header, a blank line, then JSON — so the reader from
//! `editor-lsp` is reused rather than written twice.
//!
//! The shape is deliberately the same as the language-server client: a child
//! process, a reader thread, a channel the UI drains once per frame. One
//! concurrency model in the codebase is worth more than the theoretical
//! efficiency of a second.
//!
//! What this is not: a general DAP client. It implements launch, breakpoints,
//! the four stepping commands, the stack and variables — what a Python debugger
//! needs to be useful — and nothing speculative. Attaching to a running
//! process, conditional breakpoints and watch expressions are all deliberate
//! omissions rather than oversights.

pub mod adapter;
pub mod session;

pub use session::{Breakpoint, DebugEvent, Frame, Session, State, Variable};
