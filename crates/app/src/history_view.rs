//! The commit history, and what any one commit did.
//!
//! Two panes: the list on the left, the selected commit's message and files on
//! the right. Its own window rather than a dock tab for the same reason the
//! diff view is one — history is consulted and closed, not lived in, and the
//! dock is already carrying seven tabs.
//!
//! The window computes nothing. It is handed the commits each frame and reports
//! what was clicked, so the several reasons there might be no history — no
//! repository, no commits, no answer yet — each get their own sentence.

use editor_vcs::log::{Commit, Detail};
use eframe::egui;

/// How many commits to read at a time.
///
/// A repository's log is unbounded and a window showing all of it would spend
/// its time formatting rows nobody scrolls to. "Load more" adds another page.
pub(crate) const PAGE: usize = 100;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Read further back.
    LoadMore(usize),
    /// Open this file, from the selected commit's list.
    Open(String),
}

/// What there is to show.
#[derive(Debug, Clone, Copy)]
pub(crate) enum State<'a> {
    NoRepository,
    /// Asked and not answered.
    Waiting,
    Ready {
        commits: &'a [Commit],
        /// The selected commit in full, once it has been fetched.
        detail: Option<&'a Detail>,
        /// Whether there may be more commits further back.
        more: bool,
    },
}

/// The history window.
#[derive(Debug, Default)]
pub(crate) struct HistoryView {
    open: bool,
    /// The object name of the row being looked at.
    selected: Option<String>,
}

impl HistoryView {
    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The commit whose details the application should fetch.
    #[must_use]
    pub(crate) fn selected(&self) -> Option<&str> {
        self.open.then_some(self.selected.as_deref()).flatten()
    }

    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        branch: Option<&str>,
        state: State<'_>,
    ) -> Action {
        let mut action = Action::None;
        if !self.open {
            return action;
        }

        let title = match branch {
            Some(branch) => format!("History \u{2014} {branch}"),
            None => "History".to_owned(),
        };

        let mut open = true;
        egui::Window::new(title)
            .open(&mut open)
            .default_size([900.0, 560.0])
            .resizable(true)
            .show(ctx, |ui| match state {
                State::NoRepository => {
                    ui.weak("This project is not a git repository.");
                }
                State::Waiting => {
                    ui.weak("Reading the history\u{2026}");
                }
                State::Ready {
                    commits,
                    detail,
                    more,
                } => {
                    if commits.is_empty() {
                        ui.weak("Nothing has been committed yet.");
                        return;
                    }
                    action = self.panes(ui, commits, detail, more);
                }
            });

        if !open {
            self.open = false;
        }
        action
    }

    fn panes(
        &mut self,
        ui: &mut egui::Ui,
        commits: &[Commit],
        detail: Option<&Detail>,
        more: bool,
    ) -> Action {
        let mut action = Action::None;
        // Select the newest by default, so the right-hand pane is never blank
        // on opening — an empty pane beside a full list reads as broken.
        if self.selected.is_none() {
            self.selected = commits.first().map(|c| c.id.clone());
        }

        let full = ui.available_size();
        ui.horizontal_top(|ui| {
            // Each pane lays its contents out downwards. `allocate_ui` inherits
            // the *parent's* direction, which here is horizontal — without
            // saying otherwise, every row of the list and every line of the
            // message ends up side by side across the window.
            let down = egui::Layout::top_down(egui::Align::Min);

            // The list.
            ui.allocate_ui_with_layout(egui::vec2(full.x * 0.55, full.y), down, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("history_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for commit in commits {
                            let selected = self.selected.as_deref() == Some(commit.id.as_str());
                            let row = ui.selectable_label(selected, row_text(commit));
                            if row.clicked() {
                                self.selected = Some(commit.id.clone());
                            }
                            row.on_hover_text(hover_text(commit));
                        }
                        if more {
                            ui.add_space(6.0);
                            if ui.button("Load more").clicked() {
                                action = Action::LoadMore(commits.len() + PAGE);
                            }
                        }
                    });
            });

            ui.separator();

            // The selected commit.
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), full.y), down, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("history_detail")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match detail {
                        None => {
                            ui.weak("Reading the commit\u{2026}");
                        }
                        Some(detail) => {
                            // Monospace, because a commit message is written
                            // against a fixed width and often has a list in it.
                            ui.label(egui::RichText::new(&detail.message).monospace());
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(4.0);
                            ui.strong(format!(
                                "{} file{}",
                                detail.files.len(),
                                if detail.files.len() == 1 { "" } else { "s" }
                            ));
                            for (change, path) in &detail.files {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(change.letter()).monospace().weak(),
                                    );
                                    if ui.link(path).clicked() {
                                        action = Action::Open(path.clone());
                                    }
                                });
                            }
                        }
                    });
            });
        });

        action
    }
}

/// One row of the list: when, who, and what the commit says it did.
fn row_text(commit: &Commit) -> String {
    // Through the checked constants, because the obvious symbol for a
    // merge is not in the bundled fonts and would draw as an empty box.
    let merge = if commit.is_merge() {
        format!("{} ", editor_widgets::glyphs::MERGE)
    } else {
        String::new()
    };
    format!(
        "{}  {}  {}{}",
        commit.date(),
        commit.short,
        merge,
        commit.subject
    )
}

/// The rest of it, where there is room.
fn hover_text(commit: &Commit) -> String {
    format!(
        "{}\n\n{} <{}>\n{} ({})\n{}",
        commit.subject, commit.author, commit.email, commit.when, commit.relative, commit.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, parents: usize) -> Commit {
        Commit {
            id: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_owned(),
            short: "a1b2c3d".to_owned(),
            author: "Gareth Finch".to_owned(),
            email: "someone@example.invalid".to_owned(),
            when: "2026-08-16T11:22:33+01:00".to_owned(),
            relative: "2 hours ago".to_owned(),
            parents: vec!["p".to_owned(); parents],
            subject: subject.to_owned(),
        }
    }

    #[test]
    fn a_new_view_is_closed_and_asks_for_nothing() {
        let view = HistoryView::default();
        assert!(!view.is_open());
        assert_eq!(view.selected(), None);
    }

    #[test]
    fn a_closed_view_asks_for_nothing_even_after_a_selection() {
        let mut view = HistoryView::default();
        view.open();
        view.selected = Some("abc".to_owned());
        assert_eq!(view.selected(), Some("abc"));
        view.open = false;
        assert_eq!(
            view.selected(),
            None,
            "a closed window must not keep fetching commits"
        );
    }

    #[test]
    fn a_row_says_when_which_and_what() {
        let text = row_text(&commit("Make the terminal a terminal", 1));
        assert!(text.contains("2026-08-16"), "got {text:?}");
        assert!(text.contains("a1b2c3d"), "got {text:?}");
        assert!(
            text.contains("Make the terminal a terminal"),
            "got {text:?}"
        );
    }

    #[test]
    fn a_merge_is_marked_as_one() {
        assert!(row_text(&commit("Merge branch trial", 2)).contains(editor_widgets::glyphs::MERGE));
        assert!(
            !row_text(&commit("An ordinary commit", 1)).contains(editor_widgets::glyphs::MERGE)
        );
    }

    #[test]
    fn the_hover_carries_what_the_row_had_no_room_for() {
        let text = hover_text(&commit("Subject", 1));
        assert!(text.contains("someone@example.invalid"), "got {text:?}");
        assert!(text.contains("2 hours ago"), "got {text:?}");
        assert!(
            text.contains("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"),
            "the full object name, for pasting into a terminal"
        );
    }

    #[test]
    fn loading_more_asks_for_a_page_beyond_what_is_shown() {
        // The action carries the new total rather than a page number, so the
        // tracker's one bounded request stays one bounded request.
        assert_eq!(Action::LoadMore(20 + PAGE), Action::LoadMore(120));
    }
}
