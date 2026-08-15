//! The buffer, against what was committed.
//!
//! A unified diff rather than two panes side by side. Side-by-side is prettier
//! on a wide screen and worse everywhere else: it halves the width available to
//! code that was written to fill it, and it needs a second scrollbar kept in
//! step with the first. Unified is what `git diff` shows, what a review shows,
//! and what fits.
//!
//! The window computes nothing. It is handed a [`State`] each frame and draws
//! it, so the several different reasons there might be no diff to show — no
//! repository, a file git has never seen, an answer that has not arrived yet —
//! each get their own sentence instead of one blank panel meaning all of them.

use std::path::{Path, PathBuf};

use editor_vcs::unified::{Mark, Section, totals};
use eframe::egui;

/// What there is to show.
#[derive(Debug, Clone, Copy)]
pub(crate) enum State<'a> {
    /// The project is not a git repository.
    NoRepository,
    /// Git has never seen this file: it is new, or ignored.
    NotTracked,
    /// The committed version has been asked for and has not arrived.
    Waiting,
    /// The buffer matches what was committed.
    Unchanged,
    /// The buffer differs, and here is how.
    Changed(&'a [Section]),
}

/// The diff window.
#[derive(Debug, Default)]
pub(crate) struct DiffView {
    open: bool,
    /// Which file is being shown. The window follows the tab it was opened
    /// from rather than the active one: opening a diff and then looking at
    /// another file to check something should not silently change the subject.
    path: Option<PathBuf>,
}

impl DiffView {
    /// Show the changes to `path`, replacing whatever was shown before.
    pub(crate) fn open(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.open = true;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The file being shown, so the application knows what to diff.
    #[must_use]
    pub(crate) fn path(&self) -> Option<&Path> {
        self.open.then_some(self.path.as_deref()).flatten()
    }

    /// Draw the window. `branch` names what the buffer is being compared with.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, branch: Option<&str>, state: State<'_>) {
        if !self.open {
            return;
        }

        let title = self.path.as_ref().map_or_else(
            || "Changes".to_owned(),
            |path| {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                format!("Changes — {name}")
            },
        );

        let mut open = true;
        egui::Window::new(title)
            .open(&mut open)
            .default_size([760.0, 520.0])
            .resizable(true)
            .show(ctx, |ui| {
                Self::header(ui, branch, state);
                ui.separator();
                egui::ScrollArea::both()
                    .id_salt("diff_body")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match state {
                        State::NoRepository => {
                            ui.weak("This project is not a git repository, so there is nothing to compare against.");
                        }
                        State::NotTracked => {
                            ui.weak("Git has no record of this file — it is new, or ignored. Every line of it is new.");
                        }
                        State::Waiting => {
                            ui.weak("Reading the committed version\u{2026}");
                        }
                        State::Unchanged => {
                            ui.weak("No changes: this file matches what was committed.");
                        }
                        State::Changed(sections) => Self::sections(ui, sections),
                    });
            });

        // The window's own close button.
        if !open {
            self.open = false;
        }
    }

    /// The line that says what is being compared with what.
    fn header(ui: &mut egui::Ui, branch: Option<&str>, state: State<'_>) {
        ui.horizontal(|ui| {
            match branch {
                Some(branch) => ui.weak(format!("Working copy against HEAD on {branch}")),
                None => ui.weak("Working copy against HEAD"),
            };
            if let State::Changed(sections) = state {
                let (added, removed) = totals(sections);
                ui.separator();
                ui.colored_label(added_colour(ui.visuals()), format!("+{added}"));
                ui.colored_label(removed_colour(ui.visuals()), format!("-{removed}"));
            }
        });
    }

    /// The diff itself.
    fn sections(ui: &mut egui::Ui, sections: &[Section]) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        // Wide enough for the numbers in a file of any size anyone edits, and
        // fixed, so the code column starts in the same place on every row —
        // a gutter that shifts when the line count passes a power of ten is
        // harder to read than one that is slightly too wide.
        let digits = sections
            .iter()
            .flat_map(|s| &s.lines)
            .filter_map(|l| l.old.or(l.new))
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(3);

        for section in sections {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&section.header).monospace().weak());

            for line in &section.lines {
                let (sign, colour) = match line.mark {
                    Mark::Context => (' ', ui.visuals().text_color()),
                    Mark::Added => ('+', added_colour(ui.visuals())),
                    Mark::Removed => ('-', removed_colour(ui.visuals())),
                };
                // One label per row, laid out as text rather than as a grid:
                // a grid would re-measure every cell of a thousand-line diff
                // on every frame, and the numbers are already fixed width.
                let number = |n: Option<usize>| match n {
                    Some(n) => format!("{n:>digits$}"),
                    None => " ".repeat(digits),
                };
                let row = format!(
                    "{} {}  {sign}{}",
                    number(line.old),
                    number(line.new),
                    line.text
                );
                ui.label(egui::RichText::new(row).font(font.clone()).color(colour));
            }
        }
    }
}

/// The colours the gutter uses, so the two agree about what green means.
fn added_colour(visuals: &egui::Visuals) -> egui::Color32 {
    editor_widgets::editor_view::change_colour(visuals, editor_vcs::diff::LineStatus::Added)
}

fn removed_colour(visuals: &egui::Visuals) -> egui::Color32 {
    editor_widgets::editor_view::change_colour(visuals, editor_vcs::diff::LineStatus::DeletedAbove)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_view_is_closed_and_showing_nothing() {
        let view = DiffView::default();
        assert!(!view.is_open());
        assert_eq!(view.path(), None);
    }

    #[test]
    fn opening_names_the_file_it_is_showing() {
        let mut view = DiffView::default();
        view.open(PathBuf::from("/project/main.py"));
        assert!(view.is_open());
        assert_eq!(view.path(), Some(Path::new("/project/main.py")));
    }

    /// A closed window has no subject, so nothing goes looking for a diff of a
    /// file that is not on screen.
    #[test]
    fn closing_stops_it_asking_for_anything() {
        let mut view = DiffView::default();
        view.open(PathBuf::from("/project/main.py"));
        view.close();
        assert!(!view.is_open());
        assert_eq!(view.path(), None);
    }

    #[test]
    fn opening_a_second_file_replaces_the_first() {
        let mut view = DiffView::default();
        view.open(PathBuf::from("/project/one.py"));
        view.open(PathBuf::from("/project/two.py"));
        assert_eq!(view.path(), Some(Path::new("/project/two.py")));
    }
}
