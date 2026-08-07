//! The run output console.
//!
//! Shows what the running program printed, lets you type into it, and turns
//! file references into links. Scrolling follows the output unless the user has
//! scrolled up to read something — output that yanks you back to the bottom
//! every time a line arrives is unusable.

use std::path::PathBuf;

use editor_proc::ansi::{AnsiSink, Colour, Line};
use editor_proc::links;
use eframe::egui;

/// What the console wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    None,
    /// Open a file the user clicked in the output.
    OpenLocation {
        path: PathBuf,
        line: usize,
        column: Option<usize>,
    },
    /// Send this text, plus a newline, to the program's stdin.
    SendInput(String),
    Stop,
    Restart,
    Clear,
}

/// Console state that outlives a single run.
#[derive(Debug, Default)]
pub struct Console {
    input: String,
    /// False once the user scrolls up, restored when they scroll back down.
    follow_output: bool,
    focus_input: bool,
}

/// What the console needs to know about the current run.
#[derive(Debug, Clone, Copy)]
pub struct RunState<'a> {
    pub running: bool,
    /// Shown in the header, e.g. `Python (.venv)`.
    pub label: &'a str,
    /// Where the program is running, for resolving relative paths in links.
    pub cwd: &'a std::path::Path,
}

impl Console {
    /// Called when a run starts, so the view returns to following output.
    pub fn on_run_started(&mut self) {
        self.follow_output = true;
        self.focus_input = false;
    }

    /// Draw the console.
    pub fn ui(&mut self, ui: &mut egui::Ui, output: &AnsiSink, state: RunState<'_>) -> Action {
        let mut action = Action::None;

        ui.horizontal(|ui| {
            if state.running {
                ui.spinner();
                ui.label(state.label);
                if ui
                    .button("Stop")
                    .on_hover_text("Terminate the process")
                    .clicked()
                {
                    action = Action::Stop;
                }
            } else {
                ui.weak(state.label);
                if ui.button("Restart").clicked() {
                    action = Action::Restart;
                }
            }
            if ui.button("Clear").clicked() {
                action = Action::Clear;
            }
            if output.was_trimmed() {
                ui.weak("(older output dropped)");
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.follow_output, "Follow")
                    .on_hover_text("Scroll to new output as it arrives");
            });
        });
        ui.separator();

        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_height = ui.fonts_mut(|f| f.row_height(&font));

        let scroll = egui::ScrollArea::both()
            .id_salt("console")
            .auto_shrink([false, false])
            .stick_to_bottom(self.follow_output)
            .show_rows(ui, row_height, output.line_count(), |ui, rows| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for line in output.lines().skip(rows.start).take(rows.len()) {
                    if let Some(clicked) = self.line_ui(ui, line, &font, state.cwd) {
                        action = clicked;
                    }
                }
            });

        // If the user scrolls away from the bottom, stop following; if they
        // scroll back, resume. Doing this from the scroll offset rather than
        // from a drag event catches the mouse wheel too.
        let at_bottom =
            scroll.state.offset.y >= scroll.content_size.y - scroll.inner_rect.height() - 4.0;
        if !at_bottom && self.follow_output {
            self.follow_output = false;
        }

        if state.running {
            ui.separator();
            ui.horizontal(|ui| {
                ui.weak("\u{203a}");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .hint_text("Type here to send input to the program")
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                if std::mem::take(&mut self.focus_input) {
                    field.request_focus();
                }
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = Action::SendInput(std::mem::take(&mut self.input));
                    // Keep focus so a sequence of prompts can be answered
                    // without reaching for the mouse between each one.
                    self.focus_input = true;
                    self.follow_output = true;
                }
            });
        }

        action
    }

    /// Draw one output line, with its file references as links.
    fn line_ui(
        &self,
        ui: &mut egui::Ui,
        line: &Line,
        font: &egui::FontId,
        cwd: &std::path::Path,
    ) -> Option<Action> {
        let plain = line.plain();
        let found = links::find(&plain);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            if found.is_empty() {
                for run in &line.runs {
                    if run.text.is_empty() {
                        continue;
                    }
                    ui.label(styled(ui, &run.text, run.style, font));
                }
                // Keep empty lines the right height instead of collapsing.
                if line.is_empty() {
                    ui.label(egui::RichText::new(" ").font(font.clone()));
                }
                return None;
            }

            // With links present, style is dropped in favour of the link
            // segmentation — a compiler diagnostic is more useful clickable
            // than coloured, and combining the two would mean splitting runs
            // against link boundaries for little gain.
            let mut action = None;
            let mut cursor = 0usize;
            for link in &found {
                if let Some(before) = plain.get(cursor..link.range.start)
                    && !before.is_empty()
                {
                    ui.label(egui::RichText::new(before).font(font.clone()));
                }
                if let Some(text) = plain.get(link.range.clone()) {
                    let response = ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .font(font.clone())
                                .color(ui.visuals().hyperlink_color)
                                .underline(),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        action = Some(Action::OpenLocation {
                            path: link.resolve(cwd),
                            line: link.line,
                            column: link.column,
                        });
                    }
                }
                cursor = link.range.end;
            }
            if let Some(rest) = plain.get(cursor..)
                && !rest.is_empty()
            {
                ui.label(egui::RichText::new(rest).font(font.clone()));
            }
            action
        })
        .inner
    }
}

/// Map an ANSI style onto the active theme.
///
/// The sixteen standard colours are resolved against the theme rather than
/// hard-coded, so red output stays readable in both light and dark.
fn styled(
    ui: &egui::Ui,
    text: &str,
    style: editor_proc::ansi::Style,
    font: &egui::FontId,
) -> egui::RichText {
    let mut rich = egui::RichText::new(text).font(font.clone());
    if let Some(colour) = style.foreground {
        rich = rich.color(resolve_colour(ui, colour));
    }
    if let Some(colour) = style.background {
        rich = rich.background_color(resolve_colour(ui, colour));
    }
    if style.italic {
        rich = rich.italics();
    }
    if style.underline {
        rich = rich.underline();
    }
    rich
}

fn resolve_colour(ui: &egui::Ui, colour: Colour) -> egui::Color32 {
    match colour {
        Colour::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
        Colour::Indexed(index) => indexed_colour(index, ui.visuals().dark_mode),
    }
}

/// The xterm palette, with the first sixteen adjusted for contrast against a
/// dark or light editor background.
///
/// Takes `dark` rather than a `Ui` so it is a pure function and can be tested
/// without standing up an egui context.
fn indexed_colour(index: u8, dark: bool) -> egui::Color32 {
    match index {
        0 => {
            if dark {
                egui::Color32::from_rgb(0x55, 0x59, 0x60)
            } else {
                egui::Color32::from_rgb(0x28, 0x2c, 0x34)
            }
        }
        1 => {
            if dark {
                egui::Color32::from_rgb(0xe8, 0x6b, 0x6b)
            } else {
                egui::Color32::from_rgb(0xb3, 0x1d, 0x1d)
            }
        }
        2 => {
            if dark {
                egui::Color32::from_rgb(0x8f, 0xc9, 0x6f)
            } else {
                egui::Color32::from_rgb(0x1d, 0x6b, 0x24)
            }
        }
        3 => {
            if dark {
                egui::Color32::from_rgb(0xe0, 0xb3, 0x5e)
            } else {
                egui::Color32::from_rgb(0x8a, 0x5a, 0x00)
            }
        }
        4 => {
            if dark {
                egui::Color32::from_rgb(0x6c, 0xb6, 0xff)
            } else {
                egui::Color32::from_rgb(0x0a, 0x50, 0xa0)
            }
        }
        5 => {
            if dark {
                egui::Color32::from_rgb(0xc7, 0x9b, 0xf0)
            } else {
                egui::Color32::from_rgb(0x7c, 0x3a, 0xa8)
            }
        }
        6 => {
            if dark {
                egui::Color32::from_rgb(0x6f, 0xd0, 0xd8)
            } else {
                egui::Color32::from_rgb(0x0d, 0x66, 0x6e)
            }
        }
        7 => {
            if dark {
                egui::Color32::from_rgb(0xd8, 0xd8, 0xdd)
            } else {
                egui::Color32::from_rgb(0x3d, 0x45, 0x50)
            }
        }
        8..=15 => {
            // Bright variants: the same hue, lifted.
            let base = indexed_colour(index - 8, dark);
            egui::Color32::from_rgb(
                base.r().saturating_add(0x22),
                base.g().saturating_add(0x22),
                base.b().saturating_add(0x22),
            )
        }
        // The 6x6x6 colour cube.
        16..=231 => {
            let n = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
            egui::Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        // The greyscale ramp.
        232..=255 => {
            let v = (index - 232) * 10 + 8;
            egui::Color32::from_rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_colour_cube_covers_its_corners() {
        // Index 16 is black, 231 is white; getting the arithmetic backwards
        // inverts every 256-colour program's output.
        assert_eq!(indexed_colour(16, true), egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(
            indexed_colour(231, true),
            egui::Color32::from_rgb(255, 255, 255)
        );
        // Index 100 is 84 into the cube: r=2, g=2, b=0.
        assert_eq!(
            indexed_colour(100, true),
            egui::Color32::from_rgb(135, 135, 0)
        );
    }

    #[test]
    fn the_greyscale_ramp_ascends_and_stays_neutral() {
        let first = indexed_colour(232, true);
        let last = indexed_colour(255, true);
        assert!(first.r() < last.r());
        assert_eq!(first.r(), first.g(), "greys must be neutral");
        assert_eq!(first.g(), first.b());
    }

    #[test]
    fn bright_variants_are_lighter_than_their_base() {
        let sum = |c: egui::Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
        for dark in [true, false] {
            for base in 0..8u8 {
                assert!(
                    sum(indexed_colour(base + 8, dark)) >= sum(indexed_colour(base, dark)),
                    "colour {base} is not brighter in its bright form (dark={dark})"
                );
            }
        }
    }

    #[test]
    fn the_first_eight_colours_differ_between_the_themes() {
        // Terminal red on white has to be darker than terminal red on black,
        // or half of every coloured program's output is unreadable in one of
        // them.
        for index in 0..8u8 {
            assert_ne!(
                indexed_colour(index, true),
                indexed_colour(index, false),
                "colour {index} is identical in both themes"
            );
        }
    }

    #[test]
    fn starting_a_run_scrolls_back_to_the_output() {
        // Simulate having scrolled up to read something during a previous run.
        let mut console = Console {
            follow_output: false,
            ..Console::default()
        };
        console.on_run_started();
        assert!(console.follow_output);
    }
}
