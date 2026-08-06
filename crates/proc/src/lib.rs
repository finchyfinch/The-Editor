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

// M7 populates these.
//
// pub mod pty;         // PtySession: spawn, read, write, resize, kill tree
// pub mod run_config;  // RunConfig resolution: cargo / python / node / browser
// pub mod interpreter; // Python discovery: PATH, registry, py -0p, pyenv, venv
// pub mod venv;        // `python -m venv` creation, pip bootstrap
// pub mod ansi;        // VT parsing into styled output lines
// pub mod links;       // file:line:col detection in output (rustc, tracebacks)
