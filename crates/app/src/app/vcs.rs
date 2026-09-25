//! Version control: the diff, history and branches windows, and the questions
//! asked before anything git cannot undo.

use super::*;

impl EditorApp {
    /// The diff window, and the comparison it needs to draw.
    ///
    /// The sections are rebuilt only when the buffer's version moves, because
    /// the window stays open while you edit and a diff per frame of a large
    /// file is the one cost this feature could easily have.
    pub(super) fn diff_window(&mut self, ctx: &egui::Context) {
        if !self.diff_view.is_open() {
            self.diff_cache = None;
            return;
        }
        let Some(path) = self.diff_view.path().map(Path::to_path_buf) else {
            return;
        };

        // The diff is of a *buffer*. Closing the tab takes away the thing being
        // compared, so the window goes with it.
        let Some(version) = self
            .docs
            .iter()
            .find(|entry| entry.doc.path() == Some(path.as_path()))
            .map(|entry| entry.doc.version())
        else {
            self.diff_view.close();
            self.diff_cache = None;
            return;
        };

        let fresh = self
            .diff_cache
            .as_ref()
            .is_some_and(|(cached, at, _)| cached == &path && *at == version);

        let known = self.git.baseline_state(&path);
        if !fresh && known == editor_vcs::tracker::Known::Ready {
            let text = self
                .docs
                .iter()
                .find(|entry| entry.doc.path() == Some(path.as_path()))
                .map(|entry| entry.doc.text().to_string())
                .unwrap_or_default();
            let committed = self.git.baseline(&path).unwrap_or_default().to_owned();
            let sections = editor_vcs::unified::unified(
                &editor_vcs::diff::lines(&committed),
                &editor_vcs::diff::lines(&text),
                editor_vcs::unified::DEFAULT_CONTEXT,
            );
            self.diff_cache = Some((path.clone(), version, sections));
        }

        let state = if !self.git.has_repo() {
            diff_view::State::NoRepository
        } else {
            match known {
                editor_vcs::tracker::Known::Waiting => diff_view::State::Waiting,
                editor_vcs::tracker::Known::Absent => diff_view::State::NotTracked,
                editor_vcs::tracker::Known::Ready => match &self.diff_cache {
                    Some((_, _, sections)) if !sections.is_empty() => {
                        diff_view::State::Changed(sections)
                    }
                    _ => diff_view::State::Unchanged,
                },
            }
        };

        let branch = self.git.branch().map(str::to_owned);
        self.diff_view.ui(ctx, branch.as_deref(), state);
    }

    /// Carry out what the Source Control panel asked for.
    ///
    /// Everything except discarding happens immediately: staging and unstaging
    /// move the index around and git can put either back. Discarding cannot be
    /// put back by anything, so it goes through [`Self::discard_prompt`].
    pub(super) fn apply_git_action(&mut self, action: git_panel::Action) {
        match action {
            git_panel::Action::None => {}
            git_panel::Action::Refresh => self.git.refresh_status(),
            git_panel::Action::Stage(paths) => {
                self.git.act(editor_vcs::tracker::Action::Stage(paths));
            }
            git_panel::Action::Unstage(paths) => {
                self.git.act(editor_vcs::tracker::Action::Unstage(paths));
            }
            git_panel::Action::Discard(paths) => self.pending_discard = Some(paths),
            git_panel::Action::Open(path) => self.open_path(&path, true),
            git_panel::Action::Diff(path) => self.diff_view.open(path),
            git_panel::Action::Commit { message, amend } => {
                self.git
                    .act(editor_vcs::tracker::Action::Commit { message, amend });
            }
            git_panel::Action::ShowHistory => {
                self.history.open();
                if !self.git.log_known() {
                    self.git.refresh_log(history_view::PAGE);
                }
            }
            git_panel::Action::ShowBranches => {
                self.branches.open();
                self.git.refresh_branches();
            }
            git_panel::Action::Fetch(remote) => {
                self.git.act(editor_vcs::tracker::Action::Fetch(remote));
            }
            git_panel::Action::Pull => self.git.act(editor_vcs::tracker::Action::Pull),
            git_panel::Action::Push {
                remote,
                branch,
                set_upstream,
            } => self.git.act(editor_vcs::tracker::Action::Push {
                remote,
                branch,
                set_upstream,
            }),
        }
    }

    /// The branches window, and what it needs to draw.
    pub(super) fn branches_window(&mut self, ctx: &egui::Context) {
        if !self.branches.is_open() {
            return;
        }

        let state = if self.git.has_repo() {
            if self.git.branches_known() {
                branches_view::State::Ready {
                    branches: self.git.branches(),
                    busy: self.git.busy(),
                }
            } else {
                branches_view::State::Waiting
            }
        } else {
            branches_view::State::NoRepository
        };

        match self.branches.ui(ctx, state) {
            branches_view::Action::None => {}
            branches_view::Action::Refresh => self.git.refresh_branches(),
            branches_view::Action::Checkout(name) => {
                self.git.act(editor_vcs::tracker::Action::Checkout(name));
            }
            branches_view::Action::Create { name, switch } => {
                self.git
                    .act(editor_vcs::tracker::Action::CreateBranch { name, switch });
                // Emptied optimistically rather than on success, unlike the
                // commit message: a rejected name is rejected *before* git sees
                // it, by the box itself, so anything that gets this far is one
                // git will take or refuse for a reason that retyping will not
                // fix.
                self.branches.created();
            }
            branches_view::Action::Delete { name, force: false } => {
                // Unforced first. Git refuses when the branch has commits
                // nothing else can reach, and that refusal is the prompt: it
                // says which branch and why, better than a dialog written in
                // advance could.
                self.git
                    .act(editor_vcs::tracker::Action::DeleteBranch { name, force: false });
            }
            branches_view::Action::Delete { name, force: true } => {
                self.pending_force_delete = Some(name);
            }
        }
    }

    /// Confirm before deleting a branch whose commits nothing else can reach.
    ///
    /// Milder than the discard prompt, and deliberately: the commits survive in
    /// the reflog for a while, so this is recoverable by someone who knows how,
    /// where discarded changes are recoverable by nobody.
    pub(super) fn force_delete_prompt(&mut self, ctx: &egui::Context) {
        let Some(name) = self.pending_force_delete.clone() else {
            return;
        };
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_force_delete")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Delete this branch and its commits?");
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong(&name);
                ui.label("has commits that are not merged anywhere else.");
            });
            ui.add_space(6.0);
            ui.label(
                "Deleting it makes them unreachable. Git's reflog can still find them for a \
                 while, but nothing in The Editor can.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Delete anyway").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => self.pending_force_delete = None,
            Some(true) => {
                let name = self.pending_force_delete.take().unwrap_or_default();
                self.git
                    .act(editor_vcs::tracker::Action::DeleteBranch { name, force: true });
            }
        }
    }

    /// The history window, and what it needs to draw.
    pub(super) fn history_window(&mut self, ctx: &egui::Context) {
        if !self.history.is_open() {
            return;
        }
        // Asked for here rather than when the row was clicked: the selection
        // can also change by the list reloading under it, and this covers both.
        let selected = self.history.selected().map(str::to_owned);
        if let Some(id) = &selected {
            self.git.detail(id);
        }

        let state = if self.git.has_repo() {
            if self.git.log_known() {
                history_view::State::Ready {
                    commits: self.git.log(),
                    detail: selected.as_deref().and_then(|id| self.git.known_detail(id)),
                    // Exactly as many as were asked for means there are
                    // probably more; fewer means the history ran out.
                    more: self.git.log().len() >= self.git.log_limit(),
                }
            } else {
                history_view::State::Waiting
            }
        } else {
            history_view::State::NoRepository
        };

        let branch = self.git.branch().map(str::to_owned);
        let action = self.history.ui(ctx, branch.as_deref(), state);

        match action {
            history_view::Action::None => {}
            history_view::Action::LoadMore(limit) => self.git.refresh_log(limit),
            history_view::Action::Open(relative) => {
                if let Some(root) = self.git.root().map(Path::to_path_buf) {
                    self.open_path(&root.join(relative), true);
                }
            }
        }
    }

    /// Confirm before throwing work away.
    ///
    /// Worded more bluntly than the delete prompt on purpose. A deleted file is
    /// in the recycle bin; discarded changes are nowhere. The default is to
    /// cancel, and Escape takes it.
    pub(super) fn discard_prompt(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.pending_discard.clone() else {
            return;
        };
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_discard")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(if paths.len() == 1 {
                "Discard changes to this file?"
            } else {
                "Discard changes to these files?"
            });
            ui.add_space(8.0);
            ui.label("The changes are thrown away. This cannot be undone \u{2014} not by Undo, and not by git.");
            ui.add_space(8.0);

            // Every path, up to a point. A list that scrolls off the modal is
            // a list nobody read before clicking.
            const SHOWN: usize = 12;
            for path in paths.iter().take(SHOWN) {
                ui.weak(path);
            }
            if paths.len() > SHOWN {
                ui.weak(format!("\u{2026} and {} more", paths.len() - SHOWN));
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Discard").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => self.pending_discard = None,
            Some(true) => {
                let paths = self.pending_discard.take().unwrap_or_default();
                self.git.act(editor_vcs::tracker::Action::Discard(paths));
            }
        }
    }
}
