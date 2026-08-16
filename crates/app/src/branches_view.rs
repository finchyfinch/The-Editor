//! The branches, and moving between them.
//!
//! Local branches first, then the remote-tracking ones, each with what it knows
//! about its upstream. Switching is a click; creating takes a name; deleting
//! asks first, and asks harder when the branch has commits on it that nothing
//! else does.
//!
//! Like the other windows here, this one decides nothing. It reports what was
//! clicked and the application carries it out — which is what keeps the two
//! destructive possibilities, a forced delete and a switch that would lose
//! work, out of a widget's hands.

use editor_vcs::branch::{self, Branch};
use eframe::egui;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Move onto this branch.
    Checkout(String),
    /// Start a branch here, and move onto it.
    Create { name: String, switch: bool },
    /// Delete this branch. `force` drops commits nothing else can reach, and
    /// the application must confirm before passing it on.
    Delete { name: String, force: bool },
    /// Read the branches again.
    Refresh,
}

/// What there is to show.
#[derive(Debug, Clone, Copy)]
pub(crate) enum State<'a> {
    NoRepository,
    /// Asked and not answered.
    Waiting,
    Ready {
        branches: &'a [Branch],
        /// A remote operation is running, so the list is a moment out of date.
        busy: Option<&'a str>,
    },
}

/// The branches window.
#[derive(Debug, Default)]
pub(crate) struct BranchesView {
    open: bool,
    /// The name being typed into the "new branch" box.
    new_name: String,
    /// What is wrong with it, if anything, worked out as it is typed rather
    /// than after the button is pressed.
    complaint: Option<&'static str>,
}

impl BranchesView {
    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Empty the name box, after a branch that was actually created.
    pub(crate) fn created(&mut self) {
        self.new_name.clear();
        self.complaint = None;
    }

    pub(crate) fn ui(&mut self, ctx: &egui::Context, state: State<'_>) -> Action {
        let mut action = Action::None;
        if !self.open {
            return action;
        }

        let mut open = true;
        egui::Window::new("Branches")
            .open(&mut open)
            .default_size([560.0, 460.0])
            .resizable(true)
            .show(ctx, |ui| match state {
                State::NoRepository => {
                    ui.weak("This project is not a git repository.");
                }
                State::Waiting => {
                    ui.weak("Reading the branches\u{2026}");
                }
                State::Ready { branches, busy } => {
                    action = self.body(ui, branches, busy);
                }
            });

        if !open {
            self.open = false;
        }
        action
    }

    fn body(&mut self, ui: &mut egui::Ui, branches: &[Branch], busy: Option<&str>) -> Action {
        let mut action = Action::None;

        ui.horizontal(|ui| {
            let box_ = ui.add(
                egui::TextEdit::singleline(&mut self.new_name)
                    .desired_width(240.0)
                    .hint_text("New branch name"),
            );
            if box_.changed() {
                // Checked as it is typed, so the reason a name is refused
                // appears beside the box rather than after pressing a button.
                self.complaint = (!self.new_name.trim().is_empty())
                    .then(|| branch::is_valid_name(&self.new_name))
                    .flatten();
            }
            let ready = !self.new_name.trim().is_empty() && self.complaint.is_none();
            if ui
                .add_enabled(ready, egui::Button::new("Create and switch"))
                .clicked()
            {
                action = Action::Create {
                    name: self.new_name.trim().to_owned(),
                    switch: true,
                };
            }
            if ui
                .add_enabled(ready, egui::Button::new("Create only"))
                .clicked()
            {
                action = Action::Create {
                    name: self.new_name.trim().to_owned(),
                    switch: false,
                };
            }
            if ui.button("Refresh").clicked() {
                action = Action::Refresh;
            }
        });
        if let Some(complaint) = self.complaint {
            ui.colored_label(ui.visuals().error_fg_color, complaint);
        }
        if let Some(busy) = busy {
            ui.weak(busy);
        }
        ui.separator();

        let local: Vec<&Branch> = branches.iter().filter(|b| !b.is_remote()).collect();
        let remote: Vec<&Branch> = branches.iter().filter(|b| b.is_remote()).collect();

        egui::ScrollArea::vertical()
            .id_salt("branches_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !local.is_empty() {
                    ui.strong("On this machine");
                    for branch in &local {
                        Self::row(ui, branch, &mut action);
                    }
                }
                if !remote.is_empty() {
                    ui.add_space(8.0);
                    ui.strong("On a remote");
                    // No buttons: checking one of these out lands on a detached
                    // HEAD, which is not what clicking a branch should ever do.
                    // They are here to show what the remote has.
                    for branch in &remote {
                        ui.horizontal(|ui| {
                            ui.weak(&branch.name);
                            ui.weak(egui::RichText::new(&branch.short).monospace());
                        });
                    }
                }
            });

        action
    }

    fn row(ui: &mut egui::Ui, branch: &Branch, action: &mut Action) {
        ui.horizontal(|ui| {
            // The current branch is named plainly and has no Switch button:
            // switching to where you already are is a button that does nothing.
            if branch.is_head {
                ui.strong(&branch.name);
            } else {
                ui.label(&branch.name);
            }

            let track = branch.track_summary(
                editor_widgets::glyphs::AHEAD,
                editor_widgets::glyphs::BEHIND,
            );
            if !track.is_empty() {
                ui.weak(&track).on_hover_text(match &branch.upstream {
                    Some(upstream) if branch.upstream_gone => {
                        format!("{upstream} no longer exists on the remote")
                    }
                    Some(upstream) => format!(
                        "{} commit(s) here that {upstream} has not, {} the other way",
                        branch.ahead, branch.behind
                    ),
                    None => track.clone(),
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Deleting the branch you are standing on is something git
                // refuses anyway; not offering it is clearer than offering it
                // and relaying the refusal.
                if !branch.is_head && ui.small_button("Delete").clicked() {
                    *action = Action::Delete {
                        name: branch.name.clone(),
                        force: false,
                    };
                }
                if !branch.is_head && ui.small_button("Switch").clicked() {
                    *action = Action::Checkout(branch.name.clone());
                }
                if let Some(upstream) = &branch.upstream {
                    // A word rather than an arrow: the obvious arrow for this
                    // is not in the bundled fonts either, and "tracks" is
                    // clearer than any of the ones that are.
                    ui.weak(format!("tracks {upstream}"));
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, head: bool) -> Branch {
        let record = format!(
            "{name}\u{1f}{}\u{1f}\u{1f}\u{1f}abc1234\u{1e}",
            if head { "*" } else { " " }
        );
        editor_vcs::branch::parse(&record, false)
            .pop()
            .expect("one branch")
    }

    #[test]
    fn a_new_view_is_closed_with_an_empty_box() {
        let view = BranchesView::default();
        assert!(!view.is_open());
        assert_eq!(view.new_name, "");
        assert_eq!(view.complaint, None);
    }

    #[test]
    fn creating_a_branch_empties_the_box() {
        let mut view = BranchesView {
            new_name: "feature/thing".to_owned(),
            complaint: Some("something"),
            ..BranchesView::default()
        };
        view.created();
        assert_eq!(view.new_name, "");
        assert_eq!(view.complaint, None);
    }

    /// The parse helper is what the rows are built from, so it is worth knowing
    /// it produces what the row code expects.
    #[test]
    fn the_current_branch_is_the_one_marked_by_git() {
        assert!(branch("trial", true).is_head);
        assert!(!branch("other", false).is_head);
    }

    #[test]
    fn a_name_with_a_space_is_refused_with_a_reason() {
        assert!(
            branch::is_valid_name("has space")
                .expect("refused")
                .contains("spaces")
        );
    }
}
