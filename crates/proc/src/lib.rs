//! Running user code: PTY sessions, run configurations, output parsing.
//!
//! Programs are launched under a pseudo-terminal rather than with piped
//! stdio, so `input()` prompts, colours, progress bars and Ctrl-C all behave
//! exactly as they would in a real terminal. See PLAN.md §3.8.
//!
//! Running a Python file is deliberately unremarkable: take the interpreter
//! path from settings (or the project venv, or `PATH`), pass it the file, set
//! the cwd to the project root. The full command line is echoed to the output
//! panel before execution so there is never any doubt what ran.

// This crate spawns and reaps external processes. Panics leak them.
#![deny(clippy::unwrap_used)]

pub mod ansi;
pub mod interpreter;
pub mod links;
pub mod packages;
pub mod pipe;
pub mod pty;
pub mod run_config;
pub mod screen;
pub mod spawn;
pub mod venv;
