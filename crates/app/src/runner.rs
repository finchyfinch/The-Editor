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
    /// The interface to wake when a running program produces output.
    ///
    /// Set once, after the window exists. `None` only in tests, which drive
    /// the runner directly and have nothing to wake.
    wake: Option<eframe::egui::Context>,
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
    /// Commands still to run, for multi-step operations like creating a
    /// virtual environment. A non-zero exit abandons the rest, because
    /// installing requirements into an environment that failed to be created
    /// only produces a second, more confusing error.
    queue: std::collections::VecDeque<RunConfig>,
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
            wake: None,
            session: None,
            output: AnsiSink::new(DEFAULT_SCROLLBACK),
            console: Console::default(),
            last: None,
            cwd: PathBuf::from("."),
            label: "No process".to_owned(),
            finished: None,
            queue: std::collections::VecDeque::new(),
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

    /// Put a banner line into the console, as a finished run does.
    ///
    /// The debugger reports its own start and end; without this it shared the
    /// console with the runner but never said when it had stopped, so a program
    /// that had run to completion looked identical to one still paused.
    pub(crate) fn push_banner(&mut self, text: &str) {
        self.output.push_line(text);
    }

    /// Feed text straight into the console.
    ///
    /// Used by the debugger, whose output arrives as protocol events rather
    /// than through a terminal, but which belongs in the same place the user
    /// already looks for a program's output.
    pub(crate) fn push_output(&mut self, text: &str) {
        self.output.feed(text.as_bytes());
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

    /// Run several commands in order, stopping at the first failure.
    ///
    /// # Errors
    /// If the first command cannot be started. Later failures surface in the
    /// console rather than as a `Result`, since by then the caller has moved on.
    pub(crate) fn start_sequence(&mut self, mut commands: Vec<RunConfig>) -> anyhow::Result<()> {
        let first = commands
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("nothing to run"))?;
        commands.remove(0);
        self.start(first, true)?;
        self.queue = commands.into();
        Ok(())
    }

    /// True while a multi-step operation still has commands to run.
    #[must_use]
    pub(crate) fn has_queued_work(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Start a command, replacing anything already running.
    ///
    /// # Errors
    /// If the process cannot be started.
    /// Give the runner something to wake when output arrives.
    pub(crate) fn set_context(&mut self, ctx: &eframe::egui::Context) {
        if self.wake.is_none() {
            self.wake = Some(ctx.clone());
        }
    }

    pub(crate) fn start(&mut self, config: RunConfig, clear_first: bool) -> anyhow::Result<()> {
        self.stop();
        // A new run started by hand abandons any sequence in progress.
        self.queue.clear();

        if clear_first {
            self.output.clear();
        }
        // Echo the exact command before running it, so there is never any
        // question about what was executed or with which interpreter.
        self.output
            .push_line(&format!("> {}", config.command_line()));
        self.output
            .push_line(&format!("  in {}", config.cwd.display()));

        // Same reason as the terminal: without a wake the output sits in the
        // channel until something else causes a frame, so a long compile looks
        // stalled between bursts.
        let wake = self.wake.clone().map(|ctx| {
            let waker: editor_proc::pty::Waker = std::sync::Arc::new(move || ctx.request_repaint());
            waker
        });
        let session = Session::spawn_with_wake(&config, 24, 120, wake)?;
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

        let mut next = None;
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
                    if code == Some(0) {
                        next = self.queue.pop_front();
                    } else if !self.queue.is_empty() {
                        let abandoned = self.queue.len();
                        self.queue.clear();
                        self.output
                            .push_line(&format!("[Stopped: {abandoned} step(s) not run]"));
                    }
                    // Only report completion once the whole sequence is done,
                    // so a three-step venv creation is one outcome, not three.
                    if next.is_none() {
                        self.finished = Some(code);
                    }
                }
                Event::Failed(message) => {
                    self.output.push_line(&format!("[Error: {message}]"));
                }
            }
        }

        // Chain to the next step without clearing what came before, so the
        // whole sequence reads as one transcript.
        if let Some(config) = next {
            let queued = std::mem::take(&mut self.queue);
            if let Err(e) = self.start(config, false) {
                self.output
                    .push_line(&format!("[Could not continue: {e:#}]"));
                self.finished = Some(Some(-1));
            } else {
                self.queue = queued;
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
    fn a_sequence_runs_every_step_in_order() {
        let mut runner = Runner::default();
        runner
            .start_sequence(vec![
                trivial("step_one"),
                trivial("step_two"),
                trivial("step_three"),
            ])
            .expect("starts");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            runner.poll();
            if !runner.is_running() && !runner.has_queued_work() {
                runner.poll();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let text = transcript(&runner);
        for marker in ["step_one", "step_two", "step_three"] {
            assert!(text.contains(marker), "{marker} missing from: {text}");
        }
        let positions: Vec<usize> = ["step_one", "step_two", "step_three"]
            .iter()
            .filter_map(|m| text.find(m))
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "steps ran out of order: {text}"
        );
        assert_eq!(
            runner.take_finished(),
            Some(Some(0)),
            "completion is reported once, at the end of the sequence"
        );
    }

    #[test]
    fn a_failing_step_abandons_the_rest_of_the_sequence() {
        // Installing requirements into an environment that failed to be
        // created only produces a second, more confusing error.
        let failing = RunConfig {
            args: if cfg!(windows) {
                vec!["/C".to_owned(), "exit 4".to_owned()]
            } else {
                vec!["-c".to_owned(), "exit 4".to_owned()]
            },
            ..trivial("")
        };

        let mut runner = Runner::default();
        runner
            .start_sequence(vec![failing, trivial("should_not_run")])
            .expect("starts");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            runner.poll();
            if !runner.is_running() && !runner.has_queued_work() {
                runner.poll();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let text = transcript(&runner);
        assert!(text.contains("code 4"), "got: {text}");
        assert!(
            !text.contains("should_not_run"),
            "the remaining step must not run: {text}"
        );
        assert!(text.contains("not run"), "the user should be told: {text}");
        assert!(!runner.has_queued_work());
    }

    /// End-to-end: really create a virtual environment through the runner and
    /// check the interpreter it produces works. The dialog's job is to build
    /// these commands; this proves the commands are right.
    ///
    /// Skipped when no Python is installed.
    #[test]
    fn a_real_virtual_environment_is_created_and_its_python_runs() {
        let Some(base) = editor_proc::venv::discover().into_iter().next() else {
            eprintln!("skipping: no Python installation found");
            return;
        };

        let root = std::env::temp_dir().join("the-editor-venv-e2e");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).expect("create project");
        let target = root.join(".venv");

        let commands = editor_proc::venv::create_commands(&editor_proc::venv::CreateOptions {
            base: base.path.clone(),
            target: target.clone(),
            // Skip pip work: it needs the network and is not what is under test.
            upgrade_pip: false,
            requirements: None,
            system_site_packages: false,
        })
        .expect("builds commands");

        let mut runner = Runner::default();
        runner.start_sequence(commands).expect("starts");

        // Creating an environment copies a Python installation, so allow more
        // time than a trivial command needs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            runner.poll();
            if !runner.is_running() && !runner.has_queued_work() {
                runner.poll();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let text = transcript(&runner);
        assert_eq!(
            runner.take_finished(),
            Some(Some(0)),
            "venv creation did not succeed: {text}"
        );

        let python = editor_proc::interpreter::venv_python(&target);
        assert!(
            python.is_file(),
            "no interpreter at {}: {text}",
            python.display()
        );
        let version = editor_proc::interpreter::version_of(&python)
            .unwrap_or_else(|e| panic!("the new interpreter does not run: {e}"));
        assert!(version.starts_with('3'), "got version {version:?}");

        // ...and the project would now find it automatically.
        let found = editor_proc::interpreter::find_venv(&root).expect("venv is discoverable");
        assert_eq!(found.path, python);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_sequence_is_an_error_rather_than_a_silent_no_op() {
        let mut runner = Runner::default();
        assert!(runner.start_sequence(Vec::new()).is_err());
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
