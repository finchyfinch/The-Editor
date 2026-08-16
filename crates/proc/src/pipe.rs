//! Running a child on plain pipes rather than a pseudo-terminal.
//!
//! The console runs everything under a PTY, and that is right for the common
//! case: a program that can see a terminal turns on colour, draws progress
//! bars, and answers prompts. It is exactly wrong for a program whose output is
//! going to be *read*.
//!
//! pytest on a terminal writes its results with carriage returns and cursor
//! movement, redrawing a line to keep a percentage at the right-hand edge. What
//! comes out is a stream with almost no newlines in it — the result line, the
//! `FAILURES` banner and the traceback all run together — and a parser reading
//! it recovers about half of what happened. On a pipe the same pytest writes
//! one plain line per test, because it knows nobody is watching it live.
//!
//! Nobody types into a test run, so nothing is lost by taking the terminal
//! away. [`send_input`](Session::send_input) still reaches the child's standard
//! input, which is enough for a test that reads a line.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::pty::{Event, Waker};
use crate::run_config::RunConfig;

/// A child process on pipes.
pub struct Session {
    events: Receiver<Event>,
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    child: Arc<Mutex<std::process::Child>>,
    finished: Arc<AtomicBool>,
    pid: Option<u32>,
    cwd: std::path::PathBuf,
    label: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeSession")
            .field("label", &self.label)
            .field("pid", &self.pid)
            .field("running", &self.is_running())
            .finish()
    }
}

impl Session {
    /// Start a child with its output on pipes.
    ///
    /// # Errors
    /// If the program cannot be started — a missing interpreter, a working
    /// directory that is not there.
    pub fn spawn(config: &RunConfig, wake: Option<Waker>) -> Result<Self> {
        let mut command = crate::spawn::quiet(&config.program);
        command
            .args(&config.args)
            .current_dir(&config.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (key, value) in &config.env {
            command.env(key, value);
        }
        // The opposite of what the PTY session says. A program that believes it
        // is on a terminal is a program that formats for one.
        command.env("TERM", "dumb");
        // And the two conventions for asking politely.
        command.env("NO_COLOR", "1");
        command.env("PY_COLORS", "0");

        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", config.program.display()))?;

        let pid = Some(child.id());
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = Arc::new(Mutex::new(child.stdin.take()));

        let (tx, events) = channel();
        let finished = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(child));

        // Two readers, because stdout and stderr are separate pipes and reading
        // them in turn would block on whichever the child is not writing to.
        // They interleave in arrival order, which is as close to the terminal's
        // ordering as pipes can get.
        for (name, stream) in [
            ("stdout", stdout.map(StreamKind::Out)),
            ("stderr", stderr.map(StreamKind::Err)),
        ] {
            let Some(stream) = stream else { continue };
            let tx = tx.clone();
            let wake = wake.clone();
            let _ = std::thread::Builder::new()
                .name(format!("run-{name}"))
                .spawn(move || read_stream(stream, &tx, wake.as_ref()));
        }

        let waiter_child = Arc::clone(&child);
        let waiter_finished = Arc::clone(&finished);
        let _ = std::thread::Builder::new()
            .name("run-wait".to_owned())
            .spawn(move || {
                let status = waiter_child.lock().map_or(None, |mut c| c.wait().ok());
                waiter_finished.store(true, Ordering::SeqCst);
                let code = status.and_then(|s| s.code());
                let _ = tx.send(Event::Exited(code));
                if let Some(wake) = wake {
                    wake();
                }
            });

        Ok(Self {
            events,
            stdin,
            child,
            finished,
            pid,
            cwd: config.cwd.clone(),
            label: config.label.clone(),
        })
    }

    /// Take everything the child has produced since the last call.
    #[must_use]
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Send a line to the child's standard input.
    ///
    /// # Errors
    /// If the pipe has already been closed, which is what a finished child
    /// leaves behind.
    pub fn send_input(&self, text: &str) -> Result<()> {
        let mut guard = self
            .stdin
            .lock()
            .map_err(|_| anyhow::anyhow!("the input pipe is poisoned"))?;
        let pipe = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("the program is not accepting input"))?;
        pipe.write_all(text.as_bytes())?;
        pipe.flush()?;
        Ok(())
    }

    /// Stop the child, and everything it started.
    pub fn stop(&self) {
        if let Some(pid) = self.pid {
            crate::pty::kill_tree(pid);
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.is_running() {
            self.stop();
        }
    }
}

/// Which pipe a reader thread is on. Only the type differs; the loop does not.
enum StreamKind {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Read for StreamKind {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Out(s) => s.read(buffer),
            Self::Err(s) => s.read(buffer),
        }
    }
}

/// Read until the pipe closes, sending whatever arrives.
fn read_stream(stream: StreamKind, tx: &std::sync::mpsc::Sender<Event>, wake: Option<&Waker>) {
    let mut reader = BufReader::new(stream);
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if tx.send(Event::Output(buffer[..read].to_vec())).is_err() {
                    return;
                }
                // The same rule as everywhere else: an idle interface draws no
                // frames, so output nobody asks for is output nobody sees.
                if let Some(wake) = wake {
                    wake();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn config(program: &str, args: &[&str]) -> RunConfig {
        RunConfig {
            label: "test".to_owned(),
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        }
    }

    /// Collect output until the child exits, or give up.
    fn run(config: &RunConfig) -> (String, Option<Option<i32>>) {
        let session = Session::spawn(config, None).expect("spawn");
        let mut text = String::new();
        let mut code = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            for event in session.drain() {
                match event {
                    Event::Output(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
                    Event::Exited(c) => code = Some(c),
                    Event::Failed(_) => {}
                }
            }
            if code.is_some() && !session.is_running() {
                // One more drain: the exit can arrive before the last read.
                std::thread::sleep(Duration::from_millis(30));
                for event in session.drain() {
                    if let Event::Output(bytes) = event {
                        text.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (text, code)
    }

    fn python() -> Option<std::path::PathBuf> {
        crate::interpreter::which("python").or_else(|| crate::interpreter::which("python3"))
    }

    #[test]
    fn output_comes_back_and_so_does_the_exit_code() {
        let Some(python) = python() else {
            eprintln!("skipping: no python");
            return;
        };
        let config = config(&python.to_string_lossy(), &["-c", "print('hello')"]);
        let (text, code) = run(&config);
        assert!(text.contains("hello"), "got {text:?}");
        assert_eq!(code, Some(Some(0)));
    }

    #[test]
    fn a_non_zero_exit_is_reported_as_one() {
        let Some(python) = python() else {
            eprintln!("skipping: no python");
            return;
        };
        let config = config(&python.to_string_lossy(), &["-c", "raise SystemExit(3)"]);
        let (_, code) = run(&config);
        assert_eq!(code, Some(Some(3)));
    }

    /// Both pipes are read, and neither blocks the other.
    #[test]
    fn stderr_arrives_as_well_as_stdout() {
        let Some(python) = python() else {
            eprintln!("skipping: no python");
            return;
        };
        let config = config(
            &python.to_string_lossy(),
            &[
                "-c",
                "import sys; sys.stdout.write('out\\n'); sys.stderr.write('err\\n')",
            ],
        );
        let (text, _) = run(&config);
        assert!(text.contains("out"), "got {text:?}");
        assert!(text.contains("err"), "got {text:?}");
    }

    /// The whole point: a program that would format for a terminal does not,
    /// so what comes out can be read a line at a time.
    #[test]
    fn the_child_is_told_it_is_not_on_a_terminal() {
        let Some(python) = python() else {
            eprintln!("skipping: no python");
            return;
        };
        let config = config(
            &python.to_string_lossy(),
            &["-c", "import sys; print(sys.stdout.isatty())"],
        );
        let (text, _) = run(&config);
        assert!(text.contains("False"), "got {text:?}");
    }

    #[test]
    fn a_program_that_does_not_exist_fails_to_start() {
        let config = config("this-program-does-not-exist-xyzzy", &[]);
        assert!(Session::spawn(&config, None).is_err());
    }

    #[test]
    fn input_reaches_the_child() {
        let Some(python) = python() else {
            eprintln!("skipping: no python");
            return;
        };
        let config = config(&python.to_string_lossy(), &["-c", "print('got', input())"]);
        let session = Session::spawn(&config, None).expect("spawn");
        session.send_input("hello\n").expect("send");

        let mut text = String::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && !text.contains("got") {
            for event in session.drain() {
                if let Event::Output(bytes) = event {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(text.contains("got hello"), "got {text:?}");
    }
}
