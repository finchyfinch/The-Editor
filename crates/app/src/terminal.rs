//! An integrated shell.
//!
//! The run console already drives a program under a pseudo-terminal and renders
//! its ANSI output; a terminal is the same machinery pointed at a shell instead
//! of at the file you are editing. Almost all of this is choosing the shell and
//! keeping a scrollback.
//!
//! Worth having because without it every `pip install`, `git commit` and
//! `pytest -k` means leaving the editor — and the project's virtual environment
//! is already known here, so the shell can start inside it.
//!
//! One session, not many. Tabs of terminals are a feature of a terminal
//! emulator; what an editor needs is a place to run a command in the project.

use std::path::{Path, PathBuf};

use editor_proc::ansi::AnsiSink;
use editor_proc::pty::{Event, Session};
use editor_proc::run_config::RunConfig;
use eframe::egui;

/// Lines of scrollback kept. A build that prints for a minute must not grow
/// until the editor runs out of memory.
const SCROLLBACK: usize = 5_000;

/// The shell to start, and what to call it.
///
/// PowerShell before `cmd` on Windows because it is what anyone doing anything
/// beyond `dir` is already using; `$SHELL` elsewhere because a user who changed
/// it meant it.
#[must_use]
fn shell() -> (PathBuf, Vec<String>, String) {
    if cfg!(windows) {
        if let Some(pwsh) = editor_proc::interpreter::which("pwsh") {
            return (pwsh, vec!["-NoLogo".to_owned()], "PowerShell".to_owned());
        }
        if let Some(ps) = editor_proc::interpreter::which("powershell") {
            return (ps, vec!["-NoLogo".to_owned()], "PowerShell".to_owned());
        }
        return (
            PathBuf::from("cmd"),
            Vec::new(),
            "Command Prompt".to_owned(),
        );
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let name = Path::new(&shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shell".to_owned());
    (PathBuf::from(shell), Vec::new(), name)
}

pub(crate) struct Terminal {
    session: Option<Session>,
    output: AnsiSink,
    input: String,
    label: String,
    /// Set when the panel opens, so typing can start without clicking first.
    focus_input: bool,
    follow: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            session: None,
            // A sink has no meaningful default size, so it is stated here
            // rather than derived.
            output: AnsiSink::new(SCROLLBACK),
            input: String::new(),
            label: String::new(),
            focus_input: false,
            follow: true,
        }
    }
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("running", &self.is_running())
            .finish()
    }
}

impl Terminal {
    pub(crate) fn is_running(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_running)
    }

    /// Start a shell in `cwd`, with `extra_path` ahead of `PATH`.
    ///
    /// `extra_path` is how the project's virtual environment gets in: a
    /// terminal that does not have the venv's `python` on its path is a
    /// terminal you have to activate the venv in before it is useful.
    pub(crate) fn start(&mut self, cwd: &Path, extra_path: &[PathBuf]) {
        if self.is_running() {
            return;
        }
        let (program, args, name) = shell();

        let mut env = Vec::new();
        if !extra_path.is_empty() {
            let existing = std::env::var("PATH").unwrap_or_default();
            let separator = if cfg!(windows) { ";" } else { ":" };
            let prefix = extra_path
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(separator);
            env.push(("PATH".to_owned(), format!("{prefix}{separator}{existing}")));
        }

        let config = RunConfig {
            label: name.clone(),
            program,
            args,
            cwd: cwd.to_path_buf(),
            env,
        };

        self.output = AnsiSink::new(SCROLLBACK);
        self.follow = true;
        self.focus_input = true;
        self.label = name;

        match Session::spawn(&config, 30, 120) {
            Ok(session) => self.session = Some(session),
            Err(e) => {
                self.output
                    .push_line(&format!("[Could not start a shell: {e:#}]"));
                self.session = None;
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            session.stop();
        }
        self.output.push_line("[Shell closed]");
    }

    /// Drain the shell's output. Call once per frame.
    ///
    /// Returns true if anything arrived, so the caller knows to keep the frame
    /// loop turning.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let events = session.drain();
        let busy = !events.is_empty();
        for event in events {
            match event {
                Event::Output(bytes) => self.output.feed(&bytes),
                Event::Exited(_) => {
                    self.output.push_line("[Shell exited]");
                    self.session = None;
                }
                Event::Failed(message) => self.output.push_line(&format!("[{message}]")),
            }
        }
        busy
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, cwd: Option<&Path>) -> bool {
        let mut wants_start = false;

        ui.horizontal(|ui| {
            if self.is_running() {
                ui.label(&self.label);
                if ui.button("Close").clicked() {
                    self.stop();
                }
            } else {
                ui.weak("No shell running");
                if ui.button("Start").clicked() {
                    wants_start = true;
                }
            }
            if ui.button("Clear").clicked() {
                self.output = AnsiSink::new(SCROLLBACK);
            }
            if let Some(cwd) = cwd {
                ui.weak(cwd.display().to_string());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.follow, "Follow");
            });
        });
        ui.separator();

        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row = ui.fonts_mut(|f| f.row_height(&font));

        // Bottom-up, so the prompt claims its height *first* and the scrollback
        // fills whatever is left. Laying out top-down and capping the scroll
        // area at "available height minus a guess" is what clipped the input
        // behind the status bar: the guess has to be exactly right, and it
        // stops being right the moment the interface font size changes.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if self.is_running() {
                ui.horizontal(|ui| {
                    ui.weak("\u{203a}");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.input)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("Type a command"),
                    );
                    if std::mem::take(&mut self.focus_input) {
                        field.request_focus();
                    }
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let line = std::mem::take(&mut self.input);
                        if let Some(session) = self.session.as_ref() {
                            // Carriage return alone. A terminal sends CR for
                            // Enter, and the LF is a second key press: PSReadLine
                            // reads it as "insert a newline", drops to its `>>`
                            // continuation prompt, and treats the next command
                            // as a second line of the same statement.
                            let _ = session.send_input(&format!("{line}\r"));
                        }
                        // Keep focus, so a run of commands can be typed without
                        // reaching for the mouse between each one.
                        self.focus_input = true;
                        self.follow = true;
                    }
                });
                ui.separator();
            }

            // Whatever height is left after the prompt has taken its own.
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                egui::ScrollArea::both()
                    .id_salt("terminal_output")
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.follow)
                    .show_rows(ui, row, self.output.line_count(), |ui, rows| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for line in self.output.lines().skip(rows.start).take(rows.len()) {
                            let text = line.plain();
                            ui.label(
                                egui::RichText::new(if text.is_empty() { " " } else { &text })
                                    .font(font.clone()),
                            );
                        }
                    });
            });
        });

        wants_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_is_chosen_for_this_platform() {
        let (program, _, name) = shell();
        assert!(!name.is_empty());
        assert!(!program.as_os_str().is_empty());
        if cfg!(windows) {
            // PowerShell where it exists, `cmd` as the floor -- every Windows
            // install has one of them.
            assert!(
                name.contains("PowerShell") || name.contains("Command Prompt"),
                "got {name}"
            );
        }
    }

    #[test]
    fn a_fresh_terminal_is_not_running() {
        let terminal = Terminal::default();
        assert!(!terminal.is_running());
    }

    #[test]
    fn polling_with_no_session_reports_nothing_rather_than_panicking() {
        let mut terminal = Terminal::default();
        assert!(!terminal.poll());
    }

    /// The project's virtual environment has to reach the shell, or the first
    /// thing anyone types is a command to activate it.
    #[test]
    fn the_extra_path_is_prepended_rather_than_replacing_the_existing_one() {
        let separator = if cfg!(windows) { ";" } else { ":" };
        let existing = "/usr/bin";
        let extra = [PathBuf::from("/project/.venv/bin")];

        let prefix = extra
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(separator);
        let combined = format!("{prefix}{separator}{existing}");

        assert!(combined.starts_with("/project/.venv/bin"), "the venv wins");
        assert!(combined.ends_with(existing), "and the rest survives");
    }

    #[test]
    fn stopping_a_terminal_that_never_started_still_says_so() {
        // So the panel does not sit looking live after a failed start.
        let mut terminal = Terminal::default();
        terminal.stop();
        let text: String = terminal
            .output
            .lines()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("closed"), "got {text:?}");
    }
}
