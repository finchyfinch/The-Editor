//! The working tree, and what of it is going into the next commit.
//!
//! Three groups, in the order the work moves through them: conflicts first
//! because nothing else can happen until they are resolved, then what is
//! staged, then what is not. Untracked files sit at the bottom of the unstaged
//! group rather than in a fourth one — from the panel's point of view a new
//! file and an edited file are the same decision, and separating them buys a
//! heading nobody needs.
//!
//! The panel decides nothing. It reports what was clicked and the application
//! carries it out, which is what keeps the destructive action — discard — from
//! being something a widget can do on its own.

use std::path::PathBuf;

use editor_vcs::status::{Entry, Status};
use eframe::egui;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Add these paths to the index.
    Stage(Vec<String>),
    /// Take these back out of it.
    Unstage(Vec<String>),
    /// Throw away the unstaged changes to these.
    ///
    /// The application must confirm before doing it. The panel deliberately
    /// cannot: a widget that can destroy work on a single click is one
    /// mis-click from doing it.
    Discard(Vec<String>),
    /// Open this file in an editor tab.
    Open(PathBuf),
    /// Show this file's changes.
    Diff(PathBuf),
    /// Ask git for the working tree's state again.
    Refresh,
    /// Commit what is staged.
    Commit { message: String, amend: bool },
    /// Show the history.
    ShowHistory,
    /// Show the branches.
    ShowBranches,
    /// Ask the remote what it has, without taking any of it.
    Fetch(String),
    /// Bring the current branch up to date, fast-forward only.
    Pull,
    /// Send the current branch to the remote.
    Push {
        remote: String,
        branch: String,
        set_upstream: bool,
    },
}

/// The Source Control tab.
#[derive(Debug, Default)]
pub(crate) struct GitPanel {
    /// Which row the pointer last acted on, so the diff and the file open
    /// against the same thing the buttons did.
    selected: Option<PathBuf>,
    /// The commit message being written. Kept here, not in egui's memory,
    /// because a message survives switching tabs and must survive a commit git
    /// refused — a hook that rejects the change must not also throw away the
    /// sentence explaining it.
    message: String,
    /// Whether the next commit rewrites the last one.
    amend: bool,
    /// Whether the box has been filled from the previous message. Once per
    /// tick of the box, so editing it and toggling twice does not overwrite
    /// what was typed.
    amend_prefilled: bool,
}

impl GitPanel {
    /// Empty the message box, after a commit that actually landed.
    pub(crate) fn committed(&mut self) {
        self.message.clear();
        self.amend = false;
        self.amend_prefilled = false;
    }
}

impl GitPanel {
    /// Draw the panel. `root` is the repository's top level, needed to turn
    /// git's relative paths back into ones the editor can open.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        state: State<'_>,
        root: Option<&std::path::Path>,
    ) -> Action {
        let mut action = Action::None;

        match state {
            State::NoRepository => {
                ui.add_space(8.0);
                ui.weak("This project is not a git repository.");
                return action;
            }
            State::Waiting => {
                ui.add_space(8.0);
                ui.weak("Reading the working tree\u{2026}");
                return action;
            }
            State::Ready {
                status,
                error,
                last_message,
                branch,
                remotes,
                busy,
                remote_said,
            } => {
                if let Some(error) = error {
                    // Git's own words, in full. A tidy summary of a git error
                    // throws away the part that says what to do about it.
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    });
                    ui.separator();
                }
                action = Self::branch_bar(ui, branch, remotes, busy, remote_said);
                ui.separator();
                let below = self.commit_box(ui, status, last_message);
                if action == Action::None {
                    action = below;
                }
                ui.separator();
                let below = self.body(ui, status, root);
                if action == Action::None {
                    action = below;
                }
            }
        }

        action
    }

    /// Which branch, how it stands against its upstream, and the three remote
    /// operations.
    fn branch_bar(
        ui: &mut egui::Ui,
        branch: Option<&editor_vcs::branch::Branch>,
        remotes: &[String],
        busy: Option<&str>,
        remote_said: Option<&str>,
    ) -> Action {
        let mut action = Action::None;

        ui.horizontal(|ui| {
            match branch {
                Some(branch) => {
                    ui.strong(&branch.name);
                    let track = branch.track_summary(
                        editor_widgets::glyphs::AHEAD,
                        editor_widgets::glyphs::BEHIND,
                    );
                    if !track.is_empty() {
                        ui.weak(&track);
                    }
                    match &branch.upstream {
                        Some(upstream) => {
                            ui.weak(format!("tracks {upstream}"));
                        }
                        None => {
                            ui.weak("no upstream");
                        }
                    }
                }
                None => {
                    ui.weak("Reading the branch\u{2026}");
                }
            }

            if ui.button("Branches\u{2026}").clicked() {
                action = Action::ShowBranches;
            }
            if ui.button("History\u{2026}").clicked() {
                action = Action::ShowHistory;
            }

            // The remote operations. Disabled outright when there is nowhere to
            // send anything, which is the honest state of a repository with no
            // remote rather than three buttons that all fail the same way.
            //
            // Everything is disabled while one is running: the worker does one
            // thing at a time, and queueing a second push behind a first is a
            // way to be surprised.
            let remote = remotes.first();
            let can = remote.is_some() && busy.is_none();

            ui.separator();
            if ui
                .add_enabled(can, egui::Button::new("Fetch"))
                .on_hover_text("Ask the remote what it has. Changes nothing here.")
                .on_disabled_hover_text(disabled_reason(remote.is_some(), busy))
                .clicked()
                && let Some(remote) = remote
            {
                action = Action::Fetch(remote.clone());
            }
            if ui
                .add_enabled(can, egui::Button::new("Pull"))
                .on_hover_text(
                    "Bring this branch up to date. Fast-forward only \u{2014} if the histories \
                     have diverged this refuses rather than merging.",
                )
                .on_disabled_hover_text(disabled_reason(remote.is_some(), busy))
                .clicked()
            {
                action = Action::Pull;
            }
            if ui
                .add_enabled(can && branch.is_some(), egui::Button::new("Push"))
                .on_hover_text("Send this branch to the remote. Never forced.")
                .on_disabled_hover_text(disabled_reason(remote.is_some(), busy))
                .clicked()
                && let (Some(remote), Some(branch)) = (remote, branch)
            {
                action = Action::Push {
                    remote: remote.clone(),
                    branch: branch.name.clone(),
                    // A branch that has never been pushed needs telling what it
                    // tracks, or git refuses with advice about `--set-upstream`.
                    set_upstream: branch.upstream.is_none(),
                };
            }

            if let Some(busy) = busy {
                ui.weak(busy);
            }
        });

        if let Some(said) = remote_said {
            // Git's own summary. "Everything up-to-date" is the whole point of
            // pressing the button when nothing needed sending.
            ui.horizontal_wrapped(|ui| {
                ui.weak(said);
            });
        }

        action
    }

    /// The message box and the button that uses it.
    fn commit_box(
        &mut self,
        ui: &mut egui::Ui,
        status: &Status,
        last_message: Option<&str>,
    ) -> Action {
        let mut action = Action::None;
        let staged = status.staged().count();
        let conflicts = status.conflicted().count();

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.message)
                    .desired_rows(2)
                    .desired_width(ui.available_width() - 260.0)
                    .hint_text("Message for the next commit"),
            );

            ui.vertical(|ui| {
                // Amending is only possible where there is something to amend,
                // and only sensible before the work has been shared. The second
                // is the user's judgement; the first is a fact.
                let can_amend = last_message.is_some();
                let amend = ui.add_enabled(
                    can_amend,
                    egui::Checkbox::new(&mut self.amend, "Amend the last commit"),
                );
                if amend.changed() {
                    if self.amend && self.message.trim().is_empty() && !self.amend_prefilled {
                        // Start from the message being replaced. Offering a
                        // blank box invites replacing a considered message with
                        // a hurried one.
                        self.message = last_message.unwrap_or_default().to_owned();
                        self.amend_prefilled = true;
                    } else if !self.amend && self.amend_prefilled {
                        // It was filled in on our account, so take it back out
                        // rather than leaving the old message to be committed
                        // again as a new one.
                        self.message.clear();
                        self.amend_prefilled = false;
                    }
                }
                amend.on_hover_text(if can_amend {
                    "Rewrite the last commit instead of adding one. Safe until it has been shared."
                } else {
                    "There is no commit to amend yet."
                });

                // Amending can commit nothing new — fixing a message is a
                // perfectly good reason to amend — so it does not need
                // anything staged. An ordinary commit does.
                let has_something = staged > 0 || self.amend;
                let ready = has_something && !self.message.trim().is_empty() && conflicts == 0;
                let label = if self.amend {
                    "Amend".to_owned()
                } else if staged > 0 {
                    format!("Commit {staged} file{}", if staged == 1 { "" } else { "s" })
                } else {
                    "Commit".to_owned()
                };
                let button = ui.add_enabled(ready, egui::Button::new(label));
                if button.clicked() {
                    action = Action::Commit {
                        message: self.message.clone(),
                        amend: self.amend,
                    };
                }
                // Say *why* it is disabled. A greyed button with no explanation
                // is the most annoying thing an interface can do.
                button.on_disabled_hover_text(if conflicts > 0 {
                    "Resolve the conflicts first."
                } else if !has_something {
                    "Stage something first."
                } else {
                    "Write a message first."
                });
            });
        });

        action
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        status: &Status,
        root: Option<&std::path::Path>,
    ) -> Action {
        let mut action = Action::None;

        let conflicted: Vec<&Entry> = status.conflicted().collect();
        let staged: Vec<&Entry> = status.staged().collect();
        let unstaged: Vec<&Entry> = status.unstaged().collect();
        let untracked: Vec<&Entry> = status.untracked().collect();

        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                action = Action::Refresh;
            }
            ui.separator();
            let all_unstaged: Vec<String> = unstaged
                .iter()
                .chain(&untracked)
                .map(|e| path_of(e))
                .collect();
            if ui
                .add_enabled(
                    !all_unstaged.is_empty(),
                    egui::Button::new("Stage everything"),
                )
                .clicked()
            {
                action = Action::Stage(all_unstaged);
            }
            let all_staged: Vec<String> = staged.iter().map(|e| path_of(e)).collect();
            if ui
                .add_enabled(!all_staged.is_empty(), egui::Button::new("Unstage all"))
                .clicked()
            {
                action = Action::Unstage(all_staged);
            }
        });
        ui.separator();

        if status.is_clean() {
            ui.add_space(8.0);
            ui.weak("Nothing has changed since the last commit.");
            return action;
        }

        egui::ScrollArea::vertical()
            .id_salt("git_panel_body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Conflicts first: nothing else in this panel can be finished
                // while one is outstanding.
                if !conflicted.is_empty() {
                    self.group(
                        ui,
                        "Conflicts",
                        &conflicted,
                        root,
                        Buttons::None,
                        &mut action,
                    );
                }
                if !staged.is_empty() {
                    self.group(
                        ui,
                        "Staged — will be committed",
                        &staged,
                        root,
                        Buttons::Unstage,
                        &mut action,
                    );
                }
                if !unstaged.is_empty() || !untracked.is_empty() {
                    let rest: Vec<&Entry> = unstaged.iter().chain(&untracked).copied().collect();
                    self.group(
                        ui,
                        "Changed — will not be committed",
                        &rest,
                        root,
                        Buttons::Stage,
                        &mut action,
                    );
                }
            });

        action
    }

    /// One heading and its rows.
    fn group(
        &mut self,
        ui: &mut egui::Ui,
        heading: &str,
        entries: &[&Entry],
        root: Option<&std::path::Path>,
        buttons: Buttons,
        action: &mut Action,
    ) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong(heading);
            ui.weak(format!("({})", entries.len()));
        });

        for entry in entries {
            let full = root.map(|root| root.join(&entry.path));
            let selected = full
                .as_ref()
                .is_some_and(|p| Some(p) == self.selected.as_ref());

            ui.horizontal(|ui| {
                // The status letters, in a fixed-width column so the names
                // below each other start in the same place.
                ui.label(
                    egui::RichText::new(format!(
                        "{}{}",
                        entry.index.letter(),
                        entry.worktree.letter()
                    ))
                    .monospace()
                    .weak(),
                );

                let label = ui
                    .selectable_label(selected, entry.name())
                    .on_hover_text(describe(entry));
                if label.clicked()
                    && let Some(full) = full.clone()
                {
                    self.selected = Some(full.clone());
                    *action = Action::Open(full);
                }
                if label.double_clicked()
                    && let Some(full) = full.clone()
                {
                    *action = Action::Diff(full);
                }

                if let Some(folder) = entry.folder() {
                    ui.weak(folder);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match buttons {
                        Buttons::Stage => {
                            // Discard first in the source, last on screen: this
                            // is a right-to-left layout, and the destructive
                            // button belongs furthest from the pointer's
                            // resting place.
                            if ui
                                .small_button("Discard")
                                .on_hover_text("Throw away these changes. This cannot be undone.")
                                .clicked()
                            {
                                *action = Action::Discard(vec![path_of(entry)]);
                            }
                            if ui.small_button("Stage").clicked() {
                                *action = Action::Stage(vec![path_of(entry)]);
                            }
                        }
                        Buttons::Unstage => {
                            if ui.small_button("Unstage").clicked() {
                                *action = Action::Unstage(vec![path_of(entry)]);
                            }
                        }
                        // A conflict is not staged or discarded from here. It
                        // is resolved by editing the file, which is what
                        // clicking its name does.
                        Buttons::None => {}
                    }
                });
            });
        }
    }
}

/// Which buttons a group's rows get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Buttons {
    Stage,
    Unstage,
    None,
}

/// What the panel has to draw.
#[derive(Debug, Clone, Copy)]
pub(crate) enum State<'a> {
    /// The project is not a git repository.
    NoRepository,
    /// Git has been asked and has not answered.
    Waiting,
    Ready {
        status: &'a Status,
        error: Option<&'a str>,
        /// The message of the commit HEAD is on, which is what an amend starts
        /// from. `None` before the first commit, when there is nothing to amend.
        last_message: Option<&'a str>,
        /// The branch HEAD is on, once the listing has come back.
        branch: Option<&'a editor_vcs::branch::Branch>,
        /// The configured remotes. Empty means there is nowhere to push.
        remotes: &'a [String],
        /// A remote operation is running, and the worker is busy with it.
        busy: Option<&'a str>,
        /// What a remote operation last reported, in git's own words.
        remote_said: Option<&'a str>,
    },
}

/// Why a remote button is greyed out. A disabled button with no explanation is
/// the most annoying thing an interface can do.
fn disabled_reason(has_remote: bool, busy: Option<&str>) -> &'static str {
    if !has_remote {
        "This repository has no remote configured."
    } else if busy.is_some() {
        "Waiting for the last remote operation to finish."
    } else {
        "Not available."
    }
}

/// The path git knows the file by, which is what an action must carry.
fn path_of(entry: &Entry) -> String {
    entry.path.to_string_lossy().replace('\\', "/")
}

/// A full sentence for the hover, where there is room for one.
fn describe(entry: &Entry) -> String {
    let mut text = format!("{} — {}", entry.path.display(), entry.label());
    if let Some(original) = &entry.original {
        text.push_str(&format!(", from {}", original.display()));
    }
    if entry.is_staged() && entry.is_unstaged() {
        text.push_str("\nStaged, and then changed again since.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_vcs::status::parse;

    fn status(records: &[&str]) -> Status {
        let mut text = String::new();
        for record in records {
            text.push_str(record);
            text.push('\0');
        }
        parse(&text)
    }

    #[test]
    fn a_path_is_handed_back_the_way_git_spells_it() {
        let status = status(&[" M crates/app/src/main.rs"]);
        assert_eq!(path_of(&status.entries[0]), "crates/app/src/main.rs");
    }

    #[test]
    fn the_hover_says_what_happened_and_where() {
        let status = status(&[" M src/main.rs"]);
        let text = describe(&status.entries[0]);
        assert!(text.contains("main.rs"), "got {text:?}");
        assert!(text.contains("modified"), "got {text:?}");
    }

    #[test]
    fn a_rename_says_where_it_came_from() {
        let status = status(&["R  new.rs", "old.rs"]);
        let text = describe(&status.entries[0]);
        assert!(text.contains("from old.rs"), "got {text:?}");
    }

    /// The state that needs saying out loud, because the file appears twice in
    /// the panel and that looks like a bug until it is explained.
    #[test]
    fn a_file_staged_and_then_edited_again_says_so() {
        let status = status(&["MM src/main.rs"]);
        let text = describe(&status.entries[0]);
        assert!(text.contains("changed again"), "got {text:?}");
    }

    #[test]
    fn a_new_panel_has_nothing_selected() {
        let panel = GitPanel::default();
        assert_eq!(panel.selected, None);
    }
}
