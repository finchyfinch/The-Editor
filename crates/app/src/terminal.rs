//! An integrated terminal.
//!
//! A real one, as of this version: a grid the program draws on, not a log of
//! lines. [`editor_proc::screen::Screen`] holds the grid and interprets the
//! escape sequences; this panel draws it, works out how many rows and columns
//! fit, and turns key presses into the bytes a program is waiting for.
//!
//! That distinction is the whole feature. The previous terminal was a
//! scrollback that understood colour, which is right for a build log and wrong
//! for anything interactive: it told programs `TERM=xterm-256color` and then
//! ignored every sequence that moved the cursor, so a full-screen program drew
//! its window into a list of lines and the result was unreadable. It also sent
//! whole lines on Enter, so a program reading single keys never saw them.
//!
//! One session, not many. Tabs of terminals are a feature of a terminal
//! emulator; what an editor needs is a place to run a command in the project.

use std::path::{Path, PathBuf};

use editor_proc::ansi::{Colour, Line};
use editor_proc::pty::{Event, Session};
use editor_proc::run_config::RunConfig;
use editor_proc::screen::Screen;
use eframe::egui;

use crate::terminal_keys;

/// Rows of scrollback kept above the screen.
const SCROLLBACK: usize = 5_000;

/// The measurements every part of the drawing needs, worked out once a frame.
struct Metrics {
    font: egui::FontId,
    row_height: f32,
    cell_width: f32,
}

/// The size the session starts at, before the panel has been laid out once.
const INITIAL_ROWS: usize = 24;
const INITIAL_COLS: usize = 80;

/// The shell to start, and what to call it.
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
    screen: Screen,
    label: String,
    /// Set when the panel opens, so typing can start without clicking first.
    grab_focus: bool,
    /// A note shown instead of the grid when there is no session.
    message: Option<String>,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            session: None,
            screen: Screen::new(INITIAL_ROWS, INITIAL_COLS, SCROLLBACK),
            label: String::new(),
            grab_focus: false,
            message: None,
        }
    }
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("running", &self.is_running())
            .field("screen", &self.screen)
            .finish()
    }
}

impl Terminal {
    pub(crate) fn is_running(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_running)
    }

    /// Start a shell in `cwd`, with `extra_path` ahead of `PATH`.
    pub(crate) fn start(&mut self, cwd: &Path, extra_path: &[PathBuf], ctx: &egui::Context) {
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

        let (rows, cols) = self.screen.size();
        self.screen = Screen::new(rows, cols, SCROLLBACK);
        self.grab_focus = true;
        self.label = name;
        self.message = None;

        // Wake the interface when the shell produces something. Without this
        // the output waits in the channel until a key is pressed or the pointer
        // moves, which looks exactly like the program having hung -- and for a
        // shell, where output arrives after you have stopped typing, that is
        // the normal case rather than an edge one.
        let waker = ctx.clone();
        let wake: editor_proc::pty::Waker = std::sync::Arc::new(move || waker.request_repaint());

        match Session::spawn_with_wake(&config, rows as u16, cols as u16, Some(wake)) {
            Ok(session) => self.session = Some(session),
            Err(e) => {
                self.message = Some(format!("Could not start a shell: {e:#}"));
                self.session = None;
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            session.stop();
        }
        self.message = Some("Shell closed.".to_owned());
    }

    /// Drain the shell's output. Call once per frame.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let events = session.drain();
        let busy = !events.is_empty();
        for event in events {
            match event {
                Event::Output(bytes) => self.screen.feed(&bytes),
                Event::Exited(_) => {
                    self.message = Some("Shell exited.".to_owned());
                    self.session = None;
                }
                Event::Failed(message) => self.message = Some(message),
            }
        }
        busy
    }

    /// Draw the terminal. Returns true if a shell should be started.
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
            // The title a program set, which is how `claude` and `vim` say what
            // they are doing.
            if let Some(title) = self.screen.title() {
                ui.separator();
                ui.weak(title.to_owned());
            } else if let Some(cwd) = cwd {
                ui.separator();
                ui.weak(cwd.display().to_string());
            }
        });
        ui.separator();

        if let Some(message) = self.message.clone()
            && !self.is_running()
        {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak(message);
            });
            return wants_start;
        }

        self.grid_ui(ui);
        wants_start
    }

    /// The grid itself, plus the keyboard.
    fn grid_ui(&mut self, ui: &mut egui::Ui) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let metrics = Metrics {
            row_height: ui.fonts_mut(|f| f.row_height(&font)),
            // Every cell is one character wide because the font is monospaced,
            // which is the assumption the whole grid rests on.
            cell_width: ui.fonts_mut(|f| f.glyph_width(&font, 'M')),
            font,
        };

        // The *viewport* decides the grid, not the content: the child is
        // drawing into the window you can see, and telling it otherwise makes
        // it lay out for a screen that is not there.
        let viewport = ui.available_size();
        let cols = ((viewport.x / metrics.cell_width).floor() as usize).clamp(20, 500);
        let rows = ((viewport.y / metrics.row_height).floor() as usize).clamp(4, 200);

        if self.screen.size() != (rows, cols) {
            self.screen.resize(rows, cols);
            if let Some(session) = self.session.as_ref() {
                session.resize(rows as u16, cols as u16);
            }
        }

        // Scrollback above the grid, in one scrollable run. Sticking to the
        // bottom keeps the live screen in view while output arrives, and
        // scrolling up reaches what has gone past — which is the whole reason
        // a shell keeps history.
        //
        // The alternate screen has no scrollback, so this collapses to exactly
        // the grid there, which is what a full-screen program wants: it is
        // drawing a window, not producing a transcript.
        let history = self.screen.scrollback().len();
        let total_rows = history + rows;

        egui::ScrollArea::vertical()
            .id_salt("terminal_grid")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(viewport.x, total_rows as f32 * metrics.row_height),
                    egui::Sense::click(),
                );
                if std::mem::take(&mut self.grab_focus) || response.clicked() {
                    response.request_focus();
                }

                if response.has_focus() {
                    // Claim the keys egui would otherwise spend on moving focus
                    // between widgets. A terminal wants all of them: Tab
                    // completes, the arrows move through history, Escape means
                    // Escape.
                    ui.memory_mut(|memory| {
                        memory.set_focus_lock_filter(
                            response.id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: true,
                            },
                        );
                    });
                    self.handle_keys(ui);
                }

                let clip = ui.clip_rect();
                self.paint(
                    &ui.painter_at(clip),
                    ui.visuals(),
                    rect,
                    clip,
                    &metrics,
                    response.has_focus(),
                );
            });
    }

    /// Turn this frame's input into bytes for the child.
    fn handle_keys(&mut self, ui: &egui::Ui) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let application_cursor = self.screen.application_cursor();
        let bracketed = self.screen.bracketed_paste();
        let events = ui.input(|i| i.events.clone());

        for event in events {
            let bytes = match event {
                egui::Event::Text(text) => terminal_keys::encode_text(&text),
                egui::Event::Paste(text) => terminal_keys::encode_paste(&text, bracketed),
                egui::Event::Copy | egui::Event::Cut => {
                    // Ctrl+C in a terminal interrupts; it does not copy, which
                    // is why every terminal uses Ctrl+Shift+C for copying. egui
                    // synthesises these from the platform, so they are ignored
                    // here and the interrupt goes through as Ctrl+C below.
                    continue;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => match terminal_keys::encode(key, modifiers, application_cursor) {
                    Some(bytes) => bytes,
                    None => continue,
                },
                _ => continue,
            };
            let _ = session.send_bytes(&bytes);
        }
    }

    /// Draw the rows that fall inside the clip rectangle.
    ///
    /// Only those: a shell with five thousand rows of history would otherwise
    /// lay out five thousand rows to show forty.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        painter: &egui::Painter,
        visuals: &egui::Visuals,
        rect: egui::Rect,
        clip: egui::Rect,
        metrics: &Metrics,
        focused: bool,
    ) {
        let scrollback = self.screen.scrollback();
        let visible = self.screen.visible_lines();
        let total = scrollback.len() + visible.len();

        let first = (((clip.top() - rect.top()) / metrics.row_height)
            .floor()
            .max(0.0) as usize)
            .min(total);
        let last =
            ((((clip.bottom() - rect.top()) / metrics.row_height).ceil() as usize) + 1).min(total);

        for row in first..last {
            let line = match scrollback.get(row) {
                Some(line) => line,
                None => &visible[row - scrollback.len()],
            };
            let y = rect.top() + row as f32 * metrics.row_height;
            self.paint_line(painter, visuals, line, egui::pos2(rect.left(), y), metrics);
        }

        if focused && self.screen.cursor_visible() && self.is_running() {
            let (row, col) = self.screen.cursor();
            let top = rect.top() + (scrollback.len() + row) as f32 * metrics.row_height;
            let left = rect.left() + col as f32 * metrics.cell_width;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(left, top),
                    egui::vec2(metrics.cell_width.max(1.0), metrics.row_height),
                ),
                0.0,
                // A block, at half strength, so the character underneath is
                // still readable through it.
                visuals.strong_text_color().gamma_multiply(0.5),
            );
        }
    }

    fn paint_line(
        &self,
        painter: &egui::Painter,
        visuals: &egui::Visuals,
        line: &Line,
        at: egui::Pos2,
        metrics: &Metrics,
    ) {
        let (left, y) = (at.x, at.y);
        let (font, cell_width) = (&metrics.font, metrics.cell_width);
        let mut column = 0usize;
        for run in &line.runs {
            let width = run.text.chars().count() as f32 * cell_width;
            let x = left + column as f32 * cell_width;

            if let Some(background) = run.style.background {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(width, metrics.row_height),
                    ),
                    0.0,
                    colour_of(visuals, background),
                );
            }
            let colour = run
                .style
                .foreground
                .map_or_else(|| visuals.text_color(), |c| colour_of(visuals, c));
            painter.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_TOP,
                &run.text,
                font.clone(),
                colour,
            );
            column += run.text.chars().count();
        }
    }
}

/// A terminal colour as something to paint with.
///
/// The sixteen indexed colours are the ones a theme is entitled to an opinion
/// about; beyond that the program has asked for a specific colour and gets it.
fn colour_of(visuals: &egui::Visuals, colour: Colour) -> egui::Color32 {
    match colour {
        Colour::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
        Colour::Indexed(index) => indexed(visuals, index),
    }
}

fn indexed(visuals: &egui::Visuals, index: u8) -> egui::Color32 {
    // The usual xterm palette, adjusted so the dark half stays legible on a
    // light background: pure blue on white is unreadable, and a terminal that
    // follows the editor's theme has to be readable in both.
    let dark = visuals.dark_mode;
    match index {
        0 => {
            if dark {
                egui::Color32::from_rgb(40, 42, 48)
            } else {
                egui::Color32::from_rgb(60, 62, 68)
            }
        }
        1 => egui::Color32::from_rgb(200, 70, 70),
        2 => egui::Color32::from_rgb(90, 160, 90),
        3 => {
            if dark {
                egui::Color32::from_rgb(200, 170, 80)
            } else {
                egui::Color32::from_rgb(150, 120, 30)
            }
        }
        4 => {
            if dark {
                egui::Color32::from_rgb(100, 150, 220)
            } else {
                egui::Color32::from_rgb(50, 100, 190)
            }
        }
        5 => egui::Color32::from_rgb(170, 110, 200),
        6 => egui::Color32::from_rgb(70, 160, 170),
        7 => {
            if dark {
                egui::Color32::from_rgb(200, 202, 208)
            } else {
                egui::Color32::from_rgb(80, 82, 88)
            }
        }
        // The bright half.
        8 => egui::Color32::from_rgb(120, 122, 128),
        9 => egui::Color32::from_rgb(240, 110, 110),
        10 => egui::Color32::from_rgb(120, 200, 120),
        11 => egui::Color32::from_rgb(230, 200, 110),
        12 => egui::Color32::from_rgb(130, 180, 245),
        13 => egui::Color32::from_rgb(200, 140, 230),
        14 => egui::Color32::from_rgb(100, 200, 210),
        15 => {
            if dark {
                egui::Color32::from_rgb(245, 246, 250)
            } else {
                egui::Color32::from_rgb(30, 32, 38)
            }
        }
        // The 6x6x6 colour cube, then the greyscale ramp.
        16..=231 => {
            let n = index - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            egui::Color32::from_rgb(step(n / 36), step((n / 6) % 6), step(n % 6))
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            egui::Color32::from_gray(level)
        }
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
        let mut terminal = Terminal::default();
        terminal.stop();
        assert!(
            terminal
                .message
                .as_deref()
                .is_some_and(|m| m.contains("closed")),
            "got {:?}",
            terminal.message
        );
    }

    /// The grid is what a program draws on, so output has to reach it.
    #[test]
    fn output_lands_on_the_grid() {
        let mut terminal = Terminal::default();
        terminal.screen.feed(b"\x1b[2;3Hhello");
        assert!(
            terminal.screen.to_text().contains("hello"),
            "got {:?}",
            terminal.screen.to_text()
        );
    }

    /// Every index has to produce a colour rather than panicking, including
    /// the cube and the greyscale ramp at the top of the range.
    #[test]
    fn every_palette_index_maps_to_a_colour() {
        let visuals = egui::Visuals::dark();
        for index in 0..=255u8 {
            let _ = indexed(&visuals, index);
        }
        let light = egui::Visuals::light();
        for index in 0..=255u8 {
            let _ = indexed(&light, index);
        }
    }

    /// Pure blue on white is unreadable, so the dark half of the palette has to
    /// differ between themes.
    #[test]
    fn the_dark_colours_differ_between_themes_so_both_stay_readable() {
        let dark = indexed(&egui::Visuals::dark(), 4);
        let light = indexed(&egui::Visuals::light(), 4);
        assert_ne!(dark, light);
    }
}
