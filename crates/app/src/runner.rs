//! Owning the running process and its output.
//!
//! One run at a time. Starting a second run stops the first, rather than
//! leaving orphans holding terminals open — which is what happens if you only
//! drop the old session and hope.

use std::path::{Path, PathBuf};

use editor_proc::ansi::AnsiSink;
use editor_proc::pty::{Event, Session};
use editor_proc::run_config::RunConfig;
use editor_widgets::console::Console;

/// The run panel's state.
pub(crate) struct Runner {
    session: Option<Session>,
    output: AnsiSink,
    pub(crate) console: Console,
    /// Kept so Restart can re-run the same thing after the process has gone.
    last: Option<RunConfig>,
    /// Where the last run happened, for resolving links after it exits.
    cwd: PathBuf,
    label: String,
    /// Set when a run finishes, so the app can surface the exit code once.
    finished: Option<Option<i32>>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("running", &self.is_running())
            .field("label", &self.label)
            .finish()
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            session: None,
            output: AnsiSink::new(DEFAULT_SCROLLBACK),
            console: Console::default(),
            last: None,
            cwd: PathBuf::from("."),
            label: "No process".to_owned(),
            finished: None,
        }
    }
}

/// Lines of output kept. A runaway loop printing forever must not exhaust
/// memory; M8 makes this a setting.
const DEFAULT_SCROLLBACK: usize = 50_000;

impl Runner {
    #[must_use]
    pub(crate) fn is_running(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_running)
    }

    /// The output buffer. Used by tests and by anything that wants to copy the
    /// transcript; the console reads it through [`Self::draw`].
    #[must_use]
    pub(crate) fn output(&self) -> &AnsiSink {
        &self.output
    }

    /// Where the last run happened, for the status bar.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// What is running, for the status bar.
    #[must_use]
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    /// Start a command, replacing anything already running.
    ///
    /// # Errors
    /// If the process cannot be started.
    pub(crate) fn start(&mut self, config: RunConfig, clear_first: bool) -> anyhow::Result<()> {
        self.stop();

        if clear_first {
            self.output.clear();
        }
        // Echo the exact command before running it, so there is never any
        // question about what was executed or with which interpreter.
        self.output
            .push_line(&format!("> {}", config.command_line()));
        self.output
            .push_line(&format!("  in {}", config.cwd.display()));

        let session = Session::spawn(&config, 24, 120)?;
        self.cwd = config.cwd.clone();
        self.label = config.label.clone();
        self.last = Some(config);
        self.session = Some(session);
        self.finished = None;
        self.console.on_run_started();
        Ok(())
    }

    /// Re-run the last command.
    ///
    /// # Errors
    /// If there is nothing to re-run, or it cannot be started.
    pub(crate) fn restart(&mut self) -> anyhow::Result<()> {
        let config = self
            .last
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Nothing has been run yet"))?;
        self.start(config, true)
    }

    pub(crate) fn stop(&mut self) {
        if let Some(session) = &self.session
            && session.is_running()
        {
            session.stop();
        }
    }

    pub(crate) fn clear(&mut self) {
        self.output.clear();
    }

    /// Send a line to the program's stdin.
    pub(crate) fn send_input(&mut self, text: &str) {
        let Some(session) = &self.session else {
            return;
        };
        // Echo what was typed: a PTY in this configuration does not, and
        // without it the transcript reads as if the program answered its own
        // prompt.
        self.output.feed(text.as_bytes());
        if let Err(e) = session.send_input(&format!("{text}\n")) {
            self.output.push_line(&format!("[input failed: {e}]"));
        }
    }

    /// Drain everything the process has produced. Returns true if anything
    /// arrived, so the caller knows to request a repaint.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let events = session.drain();
        if events.is_empty() {
            return false;
        }

        for event in events {
            match event {
                Event::Output(bytes) => self.output.feed(&bytes),
                Event::Exited(code) => {
                    match code {
                        Some(0) => self.output.push_line("[Finished]"),
                        Some(code) => {
                            self.output.push_line(&format!("[Exited with code {code}]"));
                        }
                        None => self.output.push_line("[Terminated]"),
                    }
                    self.finished = Some(code);
                }
                Event::Failed(message) => {
                    self.output.push_line(&format!("[Error: {message}]"));
                }
            }
        }
        true
    }

    /// Take the exit code of a run that has just finished, once.
    pub(crate) fn take_finished(&mut self) -> Option<Option<i32>> {
        self.finished.take()
    }

    /// Draw the console.
    ///
    /// Here rather than in the app so the borrow of the console can be split
    /// from the borrow of the output buffer it reads, which the caller cannot
    /// do through a method call.
    pub(crate) fn draw(&mut self, ui: &mut eframe::egui::Ui) -> editor_widgets::console::Action {
        let Self {
            console,
            output,
            session,
            cwd,
            label,
            ..
        } = self;
        let state = editor_widgets::console::RunState {
            running: session.as_ref().is_some_and(Session::is_running),
            label,
            cwd,
        };
        console.ui(ui, output, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trivial(output: &str) -> RunConfig {
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

    fn run_and_wait(runner: &mut Runner) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            runner.poll();
            if !runner.is_running() {
                runner.poll();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn transcript(runner: &Runner) -> String {
        runner
            .output()
            .lines()
            .map(editor_proc::ansi::Line::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_command_is_echoed_before_it_runs() {
        let mut runner = Runner::default();
        runner.start(trivial("hi"), true).expect("starts");
        let text = transcript(&runner);
        assert!(
            text.contains("> ") && text.contains("echo hi"),
            "the exact command should be visible: {text}"
        );
        assert!(
            text.contains("in "),
            "the working directory should be visible too: {text}"
        );
        runner.stop();
    }

    #[test]
    fn output_arrives_and_the_exit_is_reported() {
        let mut runner = Runner::default();
        runner.start(trivial("marker_text"), true).expect("starts");
        run_and_wait(&mut runner);

        let text = transcript(&runner);
        assert!(text.contains("marker_text"), "got: {text}");
        assert!(text.contains("[Finished]"), "got: {text}");
        assert_eq!(runner.take_finished(), Some(Some(0)));
        assert_eq!(runner.take_finished(), None, "reported exactly once");
    }

    #[test]
    fn a_nonzero_exit_code_is_shown() {
        let mut runner = Runner::default();
        let config = RunConfig {
            args: if cfg!(windows) {
                vec!["/C".to_owned(), "exit 7".to_owned()]
            } else {
                vec!["-c".to_owned(), "exit 7".to_owned()]
            },
            ..trivial("")
        };
        runner.start(config, true).expect("starts");
        run_and_wait(&mut runner);

        assert!(
            transcript(&runner).contains("code 7"),
            "got: {}",
            transcript(&runner)
        );
    }

    #[test]
    fn starting_a_second_run_replaces_the_first() {
        let mut runner = Runner::default();
        runner.start(trivial("first"), true).expect("starts");
        runner.start(trivial("second"), true).expect("starts again");
        run_and_wait(&mut runner);

        let text = transcript(&runner);
        assert!(text.contains("second"), "got: {text}");
        assert!(
            !text.contains("first"),
            "clearing should have removed the previous run: {text}"
        );
    }

    #[test]
    fn restart_reruns_the_last_command() {
        let mut runner = Runner::default();
        assert!(
            runner.restart().is_err(),
            "there is nothing to restart before the first run"
        );

        runner.start(trivial("again"), true).expect("starts");
        run_and_wait(&mut runner);
        runner.restart().expect("restarts");
        run_and_wait(&mut runner);

        assert!(transcript(&runner).contains("again"));
    }

    #[test]
    fn a_failed_start_is_an_error_not_a_panic() {
        let mut runner = Runner::default();
        let config = RunConfig {
            program: PathBuf::from("definitely-not-a-real-program-xyzzy"),
            args: Vec::new(),
            ..trivial("")
        };
        assert!(runner.start(config, true).is_err());
        assert!(!runner.is_running());
    }

    /// End-to-end: resolve an interpreter, run a real Python file, and check
    /// that its output and traceback come back and that the traceback produces
    /// a working link. This is the headline feature; unit tests of the pieces
    /// would not catch them being wired together wrongly.
    ///
    /// Skipped when no Python is installed, so the suite still passes on a
    /// machine without it.
    #[test]
    fn a_real_python_file_runs_and_its_traceback_is_clickable() {
        let Some(interpreter) = editor_proc::interpreter::resolve("", None) else {
            eprintln!("skipping: no Python interpreter available");
            return;
        };

        let dir = std::env::temp_dir().join("the-editor-run-e2e");
        std::fs::create_dir_all(&dir).expect("create dir");
        let script = dir.join("boom.py");
        std::fs::write(
            &script,
            "print('marker output')\nraise ValueError('deliberate')\n",
        )
        .expect("write script");

        let config = editor_proc::run_config::python(&script, Some(&interpreter), Some(&dir), &[])
            .expect("builds a config");

        let mut runner = Runner::default();
        runner.start(config, true).expect("starts");
        run_and_wait(&mut runner);

        let text = transcript(&runner);
        assert!(text.contains("marker output"), "stdout missing: {text}");
        assert!(text.contains("ValueError"), "traceback missing: {text}");
        assert!(
            text.contains("[Exited with code 1]"),
            "a raising script exits non-zero: {text}"
        );

        // The traceback frame must yield a link that resolves to the file.
        let frame = text
            .lines()
            .find(|l| l.contains("boom.py") && l.contains("line"))
            .unwrap_or_else(|| panic!("no traceback frame in: {text}"));
        let links = editor_proc::links::find(frame);
        let link = links
            .first()
            .unwrap_or_else(|| panic!("no link in {frame:?}"));
        assert_eq!(
            link.resolve(runner.cwd()).canonicalize().ok(),
            script.canonicalize().ok(),
            "the link should point at the script that raised"
        );
        assert_eq!(link.line, 2, "the raise is on line 2");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn polling_with_no_session_is_harmless() {
        let mut runner = Runner::default();
        assert!(!runner.poll());
        assert!(!runner.is_running());
        runner.stop();
        runner.clear();
    }
}
