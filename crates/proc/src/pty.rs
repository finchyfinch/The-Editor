//! Running a child process under a pseudo-terminal.
//!
//! A PTY rather than piped stdio, which is the decision that makes `input()`
//! prompts, `cargo`'s colour, Ctrl-C and progress bars all behave the way they
//! do in a terminal. Pipes would break every one of those, and retrofitting a
//! PTY afterwards means rewriting everything that reads the output.
//!
//! Threading is deliberately plain: one reader thread per session, pushing
//! bytes down a channel that the UI drains once per frame. No async runtime is
//! needed for a single child process, and the UI thread never blocks on it.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

use crate::run_config::RunConfig;

/// Something that happened to the running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Raw bytes from the child, to be fed to an [`crate::ansi::AnsiSink`].
    Output(Vec<u8>),
    /// The child finished with this exit code, or `None` if it was signalled.
    Exited(Option<i32>),
    /// The session failed; the string is for the user.
    Failed(String),
}

/// A running (or just-finished) child process.
pub struct Session {
    events: Receiver<Event>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    finished: Arc<AtomicBool>,
    pid: Option<u32>,
    /// Kept so output links can be resolved against the directory the program
    /// actually ran in.
    cwd: std::path::PathBuf,
    label: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("label", &self.label)
            .field("pid", &self.pid)
            .field("running", &self.is_running())
            .finish()
    }
}

impl Session {
    /// Spawn a command under a new PTY.
    ///
    /// # Errors
    /// If the PTY cannot be opened or the program cannot be started — a missing
    /// interpreter, a bad working directory.
    pub fn spawn(config: &RunConfig, rows: u16, cols: u16) -> Result<Self> {
        let pty = NativePtySystem::default();
        let pair = pty
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(20),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening a pseudo-terminal")?;

        let mut command = CommandBuilder::new(&config.program);
        for arg in &config.args {
            command.arg(arg);
        }
        command.cwd(&config.cwd);
        for (key, value) in &config.env {
            command.env(key, value);
        }
        // Tell the child it is on a terminal that understands colour. Without
        // this many programs disable colour when they see they are not on a
        // recognised terminal.
        command.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("starting {}", config.program.display()))?;
        // The slave handle must be dropped, or the reader never sees EOF when
        // the child exits and the console appears to hang forever.
        drop(pair.slave);

        let pid = child.process_id();
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("cloning the terminal reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("taking the terminal writer")?;

        let (tx, events) = channel();
        let finished = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(child));

        let writer = Arc::new(Mutex::new(writer));
        let exit_state = Arc::new(ExitState::default());
        spawn_reader(
            reader_name(config),
            &mut reader,
            tx.clone(),
            Arc::clone(&writer),
            Arc::clone(&exit_state),
        );
        spawn_waiter(Arc::clone(&child), tx, Arc::clone(&finished), exit_state);

        Ok(Self {
            events,
            writer,
            child,
            master: pair.master,
            finished,
            pid,
            cwd: config.cwd.clone(),
            label: config.label.clone(),
        })
    }

    /// Take everything the child has produced since the last call.
    ///
    /// Never blocks: called once per frame from the UI thread.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Send text to the child's stdin, for `input()` prompts.
    ///
    /// # Errors
    /// If the terminal has already closed.
    pub fn send_input(&self, text: &str) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("the terminal writer is poisoned"))?;
        writer.write_all(text.as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    /// Tell the child the window changed size, so it can re-wrap its output.
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Stop the process and everything it started.
    ///
    /// Killing only the direct child is not enough: `cargo run` spawns the
    /// compiled binary, and killing cargo alone leaves that binary running with
    /// the terminal still open. See [`kill_tree`].
    pub fn stop(&self) {
        if let Some(pid) = self.pid {
            kill_tree(pid);
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A session dropped while running would otherwise leave an orphan
        // holding a terminal open.
        if self.is_running() {
            self.stop();
        }
    }
}

fn reader_name(config: &RunConfig) -> String {
    format!("pty-reader({})", config.label)
}

/// Device Status Report: "where is the cursor?"
///
/// Windows ConPTY sends this at startup and, in some configurations, waits for
/// an answer before letting the child get on with it — so a console that never
/// replies looks exactly like a program that produced no output and never
/// exited. Answering is a terminal's job, so the reader thread answers.
const DSR_REQUEST: &[u8] = b"\x1b[6n";
/// "Cursor is at row 1, column 1." The child only needs a well-formed answer;
/// nothing here depends on it being accurate, since there is no cursor
/// addressing to be accurate about.
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

fn spawn_reader(
    name: String,
    reader: &mut Box<dyn Read + Send>,
    tx: Sender<Event>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    state: Arc<ExitState>,
) {
    // `reader` is moved into the thread; the caller keeps nothing.
    let mut reader = std::mem::replace(reader, Box::new(std::io::empty()));
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buffer[..n];
                    if contains(chunk, DSR_REQUEST)
                        && let Ok(mut writer) = writer.lock()
                    {
                        let _ = writer.write_all(DSR_REPLY);
                        let _ = writer.flush();
                    }
                    if tx.send(Event::Output(chunk.to_vec())).is_err() {
                        break; // the console went away
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }

        // Everything the program produced has now been sent. Announcing the
        // exit from here — after the last Output on the same channel — is what
        // keeps the banner below the output rather than somewhere inside it.
        state.drained.store(true, Ordering::SeqCst);
        if state.code.lock().is_ok_and(|c| c.is_some()) {
            announce_exit(&state, &tx);
        }
    });
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Shared between the reader and the waiter so the exit event can be ordered
/// after the last of the output.
///
/// The two threads write to one channel, and a process can exit while output it
/// already produced is still sitting in the terminal buffer. Letting the waiter
/// announce the exit as soon as `try_wait` succeeds puts `[Finished]` in the
/// middle of the program's output — which is exactly what happened.
#[derive(Debug, Default)]
struct ExitState {
    /// `Some(code)` once the process has been reaped. The inner `Option` is
    /// `None` for a process that was signalled rather than exiting.
    code: Mutex<Option<Option<i32>>>,
    /// Set once the reader has seen end of stream, so nothing more is coming.
    drained: AtomicBool,
    /// Ensures the exit is announced exactly once, whichever thread gets there.
    announced: AtomicBool,
}

/// How long the waiter gives the reader to finish after the process ends.
///
/// Normally the reader sees end of stream within microseconds. The wait exists
/// for the case where something else still holds the terminal open — a
/// grandchild that outlived its parent — so the console is not left saying a
/// finished process is still running.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

fn announce_exit(state: &ExitState, tx: &Sender<Event>) {
    if state.announced.swap(true, Ordering::SeqCst) {
        return;
    }
    let code = state.code.lock().ok().and_then(|c| *c).flatten();
    let _ = tx.send(Event::Exited(code));
}

fn spawn_waiter(
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    tx: Sender<Event>,
    finished: Arc<AtomicBool>,
    state: Arc<ExitState>,
) {
    let _ = std::thread::Builder::new()
        .name("pty-waiter".to_owned())
        .spawn(move || {
            let status = loop {
                let Ok(mut guard) = child.lock() else {
                    break None;
                };
                match guard.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {
                        // Release the lock before sleeping, or `stop` cannot
                        // take it to kill the process.
                        drop(guard);
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Failed(e.to_string()));
                        break None;
                    }
                }
            };

            if let Ok(mut slot) = state.code.lock() {
                *slot = Some(status.map(|s| s.exit_code() as i32));
            }

            // Let the reader finish first, so every line the program printed is
            // already in the console before it is told the program ended.
            let deadline = Instant::now() + DRAIN_GRACE;
            while !state.drained.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            announce_exit(&state, &tx);

            // Only now is the session finished. Flipping this before the
            // announcement would let a caller that polls `is_running` stop
            // draining the channel one moment before the exit event lands on
            // it, and never see the run complete.
            finished.store(true, Ordering::Relaxed);
        });
}

/// Kill a process and its descendants.
///
/// Windows has no process groups in the Unix sense; `taskkill /T` walks the
/// parent-child chain and is what every tool ends up using. A Job Object would
/// be more robust — it survives a process re-parenting itself — but needs a
/// meaningful amount of unsafe FFI for a case that does not arise with
/// `cargo` or `python`.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = crate::spawn::quiet("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // Negative pid means "the process group", which is what the PTY put the
        // child into when it became the session leader.
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status();
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_config::RunConfig;
    use std::path::PathBuf;

    /// A command that exists on every platform and exits immediately.
    fn trivial_command(output: &str) -> RunConfig {
        let (program, args) = if cfg!(windows) {
            (
                PathBuf::from("cmd"),
                vec!["/C".to_owned(), format!("echo {output}")],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec!["-c".to_owned(), format!("echo {output}")],
            )
        };
        RunConfig {
            label: "test".to_owned(),
            program,
            args,
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        }
    }

    /// Collect events until the process exits or the deadline passes.
    fn run_to_completion(session: &Session) -> Vec<Event> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut events = Vec::new();
        while std::time::Instant::now() < deadline {
            events.extend(session.drain());
            if events.iter().any(|e| matches!(e, Event::Exited(_))) {
                // Drain once more: output can arrive after the exit event.
                std::thread::sleep(std::time::Duration::from_millis(50));
                events.extend(session.drain());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        events
    }

    fn output_text(events: &[Event]) -> String {
        let mut sink = crate::ansi::AnsiSink::new(1000);
        for event in events {
            if let Event::Output(bytes) = event {
                sink.feed(bytes);
            }
        }
        sink.lines()
            .map(crate::ansi::Line::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_command_runs_and_its_output_arrives() {
        let session = Session::spawn(&trivial_command("hello_from_pty"), 24, 80).expect("spawns");
        let events = run_to_completion(&session);

        assert!(
            output_text(&events).contains("hello_from_pty"),
            "got: {:?}",
            output_text(&events)
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Exited(Some(0)))),
            "expected a clean exit, got {events:?}"
        );
    }

    /// Regression: the exit event could overtake output still buffered in the
    /// terminal, putting `[Finished]` in the middle of a program's output.
    ///
    /// Both threads write to one channel, so nothing ordered them; a short
    /// program that printed several lines and exited immediately hit it
    /// readily. Repeated, because a race that happens sometimes is still a bug.
    #[test]
    fn the_exit_event_never_overtakes_the_output() {
        for attempt in 0..8 {
            let script = "for i in 1 2 3 4 5 6 7 8; do echo line$i; done";
            let config = if cfg!(windows) {
                RunConfig {
                    args: vec![
                        "/C".to_owned(),
                        "for %i in (1 2 3 4 5 6 7 8) do @echo line%i".to_owned(),
                    ],
                    ..trivial_command("")
                }
            } else {
                RunConfig {
                    args: vec!["-c".to_owned(), script.to_owned()],
                    ..trivial_command("")
                }
            };

            let session = Session::spawn(&config, 24, 80).expect("spawns");
            let events = run_to_completion(&session);

            // Every Output must come before the Exited on the same channel.
            let exit_index = events
                .iter()
                .position(|e| matches!(e, Event::Exited(_)))
                .unwrap_or_else(|| panic!("attempt {attempt}: no exit event"));
            let output_after = events
                .iter()
                .skip(exit_index + 1)
                .any(|e| matches!(e, Event::Output(_)));
            assert!(
                !output_after,
                "attempt {attempt}: output arrived after the exit event, so the \
                 banner would appear mid-output"
            );

            // ...and the last line the program printed really did arrive.
            let text = output_text(&events);
            assert!(
                text.contains("line8"),
                "attempt {attempt}: the last line was lost: {text}"
            );
        }
    }

    #[test]
    fn a_failing_command_reports_its_exit_code() {
        let config = if cfg!(windows) {
            RunConfig {
                args: vec!["/C".to_owned(), "exit 3".to_owned()],
                ..trivial_command("")
            }
        } else {
            RunConfig {
                args: vec!["-c".to_owned(), "exit 3".to_owned()],
                ..trivial_command("")
            }
        };

        let session = Session::spawn(&config, 24, 80).expect("spawns");
        let events = run_to_completion(&session);
        assert!(
            events.iter().any(|e| matches!(e, Event::Exited(Some(3)))),
            "expected exit code 3, got {events:?}"
        );
    }

    #[test]
    fn spawning_a_nonexistent_program_fails_with_a_useful_error() {
        let config = RunConfig {
            label: "missing".to_owned(),
            program: PathBuf::from("definitely-not-a-real-program-xyzzy"),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        };
        let error = Session::spawn(&config, 24, 80).expect_err("should not spawn");
        assert!(
            error.to_string().contains("definitely-not-a-real-program"),
            "the error should name the program: {error}"
        );
    }

    #[test]
    fn a_session_reports_when_it_has_finished() {
        let session = Session::spawn(&trivial_command("x"), 24, 80).expect("spawns");
        assert!(
            session.is_running(),
            "should be running immediately after spawn"
        );
        run_to_completion(&session);
        assert!(!session.is_running(), "should have finished");
    }

    /// Regression: a caller that polls `is_running` and stops draining when it
    /// goes false must still see the exit event.
    ///
    /// The drain-ordering fix originally set the flag as soon as the process
    /// was reaped, which is up to a whole grace period before the event was
    /// sent — so the run appeared to end without ever reporting a code.
    #[test]
    fn the_exit_event_is_already_queued_when_the_session_reports_it_finished() {
        let session = Session::spawn(&trivial_command("x"), 24, 80).expect("spawns");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut events = Vec::new();
        while std::time::Instant::now() < deadline {
            let running = session.is_running();
            events.extend(session.drain());
            if !running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            events.iter().any(|e| matches!(e, Event::Exited(_))),
            "no exit event was waiting when the session said it had finished: {events:?}"
        );
    }

    #[test]
    fn draining_an_idle_session_returns_nothing_and_does_not_block() {
        let session = Session::spawn(&trivial_command("x"), 24, 80).expect("spawns");
        run_to_completion(&session);
        // Everything already taken; must return promptly rather than waiting.
        let started = std::time::Instant::now();
        let _ = session.drain();
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn the_working_directory_and_label_are_remembered_for_link_resolution() {
        let config = trivial_command("x");
        let session = Session::spawn(&config, 24, 80).expect("spawns");
        assert_eq!(session.cwd(), config.cwd);
        assert_eq!(session.label(), "test");
        run_to_completion(&session);
    }
}
