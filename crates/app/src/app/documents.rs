//! Open documents: tabs, saving, and what happens when a file changes on
//! disk behind them.

use super::*;

impl EditorApp {
    /// Open a file in a tab, or focus the tab it is already in.
    ///
    /// `preview` opens it in the reusable preview tab (single click in the
    /// explorer); otherwise it gets a permanent tab.
    pub(super) fn open_path(&mut self, path: &Path, preview: bool) {
        if let Some(existing) = self.docs.iter().position(|d| d.doc.path() == Some(path)) {
            self.active = Some(existing);
            if !preview {
                self.docs[existing].preview = false;
            }
            self.focus_active();
            return;
        }

        let doc = match Document::open(path) {
            Ok(doc) => doc,
            Err(e) => {
                self.error(format!("{e:#}"));
                return;
            }
        };
        // Only files that actually opened go on the recent list. Offering one
        // that failed a moment ago is a trap.
        Session::push_recent(&mut self.recent, path);

        if doc.is_large() {
            self.info(format!(
                "{} is large; opened read-only without highlighting",
                doc.display_name()
            ));
        }

        let language = path
            .extension()
            .and_then(|e| e.to_str())
            .map_or(LanguageId::PlainText, LanguageId::from_extension);

        let entry = OpenDoc {
            recovery_id: self.claim_recovery_id(),
            highlighter: new_highlighter(language, &doc),
            doc,
            view: EditorView::default(),
            language,
            find: FindBar::default(),
            pending_find_step: None,
            preview,
            syntax_version: None,
            syntax_due: None,
            disk: DiskState::Unchanged,
        };

        // A preview tab replaces the existing one rather than adding to it.
        if preview && let Some(slot) = self.docs.iter().position(|d| d.preview) {
            self.docs[slot] = entry;
            self.active = Some(slot);
        } else {
            self.docs.push(entry);
            self.active = Some(self.docs.len() - 1);
        }
        self.sync_watched_files();
        self.focus_active();
    }

    /// Close a tab, asking first if it has unsaved changes.
    pub(super) fn close_tab(&mut self, index: usize) {
        if self.docs.get(index).is_some_and(|d| d.doc.is_dirty()) {
            self.pending = Some(Pending::CloseTab(index));
            return;
        }
        self.force_close_tab(index);
    }

    /// Close a tab unconditionally. Only call once unsaved work is resolved.
    pub(super) fn force_close_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        let closed = self.docs.remove(index);
        // The committed copy and the diff were held for a buffer that is gone.
        if let Some(path) = closed.doc.path() {
            self.git.forget(path);
        }

        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.docs.len() - 1)),
            None => None,
        };
        self.sync_watched_files();
    }

    /// Indices of every document with unsaved changes.
    pub(super) fn dirty_indices(&self) -> Vec<usize> {
        self.docs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.doc.is_dirty())
            .map(|(i, _)| i)
            .collect()
    }

    /// Which documents a pending action would discard.
    pub(super) fn at_risk(&self, pending: Pending) -> Vec<usize> {
        match pending {
            Pending::CloseTab(i) => self
                .docs
                .get(i)
                .filter(|d| d.doc.is_dirty())
                .map(|_| vec![i])
                .unwrap_or_default(),
            Pending::CloseOthers(keep) => self
                .dirty_indices()
                .into_iter()
                .filter(|i| *i != keep)
                .collect(),
            Pending::CloseAll | Pending::Quit => self.dirty_indices(),
        }
    }

    /// Save the given documents. Returns false if any could not be saved, in
    /// which case the destructive action must not proceed.
    pub(super) fn save_indices(&mut self, indices: &[usize]) -> bool {
        let mut failures = Vec::new();
        for &i in indices {
            let Some(entry) = self.docs.get_mut(i) else {
                continue;
            };
            if entry.doc.path().is_none() {
                // An untitled buffer needs a destination. Rather than opening a
                // file chooser from inside a modal, refuse and let the user do
                // Save As deliberately.
                failures.push(format!("{} has never been saved", entry.doc.display_name()));
                continue;
            }
            if let Err(e) = entry.doc.save() {
                failures.push(format!("{}: {e:#}", entry.doc.display_name()));
            }
        }
        if failures.is_empty() {
            true
        } else {
            self.error(format!("Not closed \u{2014} {}", failures.join("; ")));
            false
        }
    }

    /// Carry out a pending action now that unsaved work has been dealt with.
    pub(super) fn commit_pending(&mut self, pending: Pending, ctx: &egui::Context) {
        match pending {
            Pending::CloseTab(i) => self.force_close_tab(i),
            Pending::CloseOthers(keep) => {
                if keep < self.docs.len() {
                    let kept = self.docs.remove(keep);
                    self.docs.clear();
                    self.docs.push(kept);
                    self.active = Some(0);
                }
            }
            Pending::CloseAll => {
                self.docs.clear();
                self.active = None;
            }
            Pending::Quit => {
                self.confirm_quit();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Agree to the window closing, and take this session's recovery copies
    /// with it.
    ///
    /// The clearing is part of confirming rather than something each caller
    /// remembers, because there are two ways out of the application and only
    /// one of them used to do it. Quitting with nothing unsaved cleared the
    /// store; quitting *after choosing not to save* set the flag by hand and
    /// returned, and the flag is what stops the close-requested branch running
    /// a second time — so the clearing it does was skipped, and the copies
    /// survived. The next start then offered back, as unsaved work rescued
    /// from a crash, precisely the changes the user had just told it to throw
    /// away.
    pub(super) fn confirm_quit(&mut self) {
        self.quit_confirmed = true;
        self.recovery.clear();
    }

    /// The Save / Don't Save / Cancel prompt.
    ///
    /// Deliberately not a plain "are you sure": the third option has to be
    /// *save*, or the only way out of the dialog is to lose the work.
    pub(super) fn unsaved_prompt(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending else {
            return;
        };
        let at_risk = self.at_risk(pending);

        // Nothing actually unsaved — proceed without bothering the user.
        if at_risk.is_empty() {
            self.pending = None;
            self.commit_pending(pending, ctx);
            return;
        }

        let names: Vec<String> = at_risk
            .iter()
            .filter_map(|i| self.docs.get(*i))
            .map(|d| d.doc.display_name())
            .collect();

        let mut decision = None;

        egui::Modal::new(egui::Id::new("unsaved_changes")).show(ctx, |ui| {
            ui.set_width(420.0);
            if names.len() == 1 {
                ui.heading(format!("Save changes to {}?", names[0]));
            } else {
                ui.heading(format!("Save changes to {} files?", names.len()));
            }
            ui.add_space(6.0);
            ui.label("Your changes will be lost if you don't save them.");

            if names.len() > 1 {
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for name in &names {
                            ui.weak(format!("\u{2022} {name}"));
                        }
                    });
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save = if names.len() == 1 { "Save" } else { "Save All" };
                    if ui.button(save).clicked() {
                        decision = Some(Decision::Save);
                    }
                    if ui.button("Don't Save").clicked() {
                        decision = Some(Decision::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(Decision::Cancel);
                    }
                });
            });
        });

        // Escape is Cancel — the safe option, never the destructive one.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(Decision::Cancel);
        }

        match decision {
            Some(Decision::Save) => {
                self.pending = None;
                if self.save_indices(&at_risk) {
                    self.commit_pending(pending, ctx);
                }
            }
            Some(Decision::Discard) => {
                self.pending = None;
                self.commit_pending(pending, ctx);
            }
            Some(Decision::Cancel) => self.pending = None,
            None => {}
        }
    }

    pub(super) fn save_active(&mut self, ask_for_path: bool) {
        let Some(index) = self.active else {
            return;
        };
        self.tidy_before_saving(index);
        let needs_path = ask_for_path || self.docs[index].doc.path().is_none();

        if needs_path {
            let start = self
                .tree
                .root()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let chosen = rfd::FileDialog::new()
                .set_directory(start)
                .set_file_name(self.docs[index].doc.display_name())
                .save_file();
            let Some(path) = chosen else { return };
            self.save_to(index, Some(path));
        } else {
            self.save_to(index, None);
        }
    }

    /// Write one document, to `path` if given or to its own path otherwise,
    /// and bring everything that describes the disk up to date.
    ///
    /// A document whose encoding cannot hold its text is not written; the
    /// question of what to do instead goes to [`Self::encoding_prompt`].
    pub(super) fn save_to(&mut self, index: usize, path: Option<PathBuf>) {
        let result = match &path {
            Some(path) => self.docs[index].doc.save_as(path),
            None => self.docs[index].doc.save(),
        };

        match result {
            Ok(()) => {
                let name = self.docs[index].doc.display_name();
                self.docs[index].preview = false;
                self.tree.refresh();
                // Save As gives the document a new path, and possibly one in a
                // directory nothing is watching yet.
                self.sync_watched_files();
                // The working tree just changed on disk, and the panel is
                // describing the state from before the save.
                self.git.refresh_status();
                // Blame is of the file on disk, so this is the one moment it
                // goes stale without HEAD having moved.
                if let Some(path) = self.docs[index].doc.path().map(Path::to_path_buf) {
                    self.git.refresh_blame(&path);
                }
                self.info(format!("Saved {name}"));
            }
            Err(e) => match e.downcast_ref::<Unrepresentable>() {
                Some(refused) => {
                    self.pending_encoding = Some((self.docs[index].recovery_id, *refused, path));
                }
                None => self.error(format!("Save failed: {e:#}")),
            },
        }
    }

    /// Ask what to do with a file whose encoding cannot store what was typed.
    ///
    /// UTF-8 is the only offer, because it is the only encoding that is
    /// certain to work and that every tool reading the file will understand.
    /// The alternative — writing the character as something else — is what
    /// used to happen silently, and is why this prompt exists.
    pub(super) fn encoding_prompt(&mut self, ctx: &egui::Context) {
        let Some((id, refused, target)) = self.pending_encoding.clone() else {
            return;
        };
        let Some(index) = self.docs.iter().position(|d| d.recovery_id == id) else {
            self.pending_encoding = None;
            return;
        };
        let name = self.docs[index].doc.display_name();
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_encoding")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Save as UTF-8?");
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong(&name);
                ui.label(format!(
                    "is saved as {}, which cannot store {:?} (line {}, column {}).",
                    refused.encoding.label(),
                    refused.character,
                    refused.line,
                    refused.column
                ));
            });
            ui.add_space(4.0);
            ui.label(
                "Saving it as UTF-8 keeps every character. Programs that expect \
                 the old encoding may show accented letters differently.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Save as UTF-8").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => {
                self.pending_encoding = None;
                self.info(format!("{name} was not saved"));
            }
            Some(true) => {
                self.pending_encoding = None;
                self.docs[index].doc.set_encoding(Encoding::Utf8);
                self.save_to(index, target);
            }
        }
    }

    /// Write a file created by the New File dialog and open it.
    pub(super) fn create_file(&mut self, request: new_file::NewFile) {
        let new_file::NewFile {
            path,
            language,
            contents,
            cursor,
        } = request;

        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error(format!("Could not create {}: {e}", parent.display()));
            return;
        }

        // Normalise to the platform's line endings on the way out; Document
        // will detect and preserve them from here on.
        let eol = editor_core::document::LineEnding::platform_default();
        let text = if eol == editor_core::document::LineEnding::Crlf {
            contents.replace('\n', "\r\n")
        } else {
            contents
        };

        if let Err(e) = std::fs::write(&path, text.as_bytes()) {
            self.error(format!("Could not write {}: {e}", path.display()));
            return;
        }

        tracing::info!(path = %path.display(), "created file");
        self.tree.refresh();
        self.open_path(&path, false);

        if let Some(entry) = self.active_mut() {
            // The dialog's explicit choice wins over extension sniffing, so a
            // Python file named `build.cfg` is still treated as Python.
            entry.language = language;
            entry.highlighter = new_highlighter(language, &entry.doc);
            // Place the caret where the template asked.
            entry.view.set_caret(cursor);
        }
    }

    /// Run an editing operation on the active document.
    ///
    /// Line operations are commands so they reach the menu and the palette, but
    /// they act on the view, and every one needs the same two lookups and the
    /// same "is there a document" guard.
    pub(super) fn on_view(&mut self, f: impl FnOnce(&mut EditorView, &mut Document) -> bool) {
        if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i))
            && f(&mut entry.view, &mut entry.doc)
        {
            // Editing a preview tab promotes it, as typing does.
            entry.preview = false;
        }
    }

    /// Open Go to File over a fresh listing of the project.
    pub(super) fn open_file_picker(&mut self) {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first: Go to File searches the project");
            return;
        };
        let listing = editor_search::files::list(&root);
        if listing.files.is_empty() {
            self.info("No files found in this folder");
            return;
        }
        self.file_picker.open(listing);
    }

    /// Select `range` in the active document and scroll it into view.
    pub(super) fn reveal_in_active(&mut self, range: std::ops::Range<usize>) {
        if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) {
            let end = range.end.min(entry.doc.len_chars());
            entry.view.select_range(range.start.min(end), end);
            entry.view.focus();
        }
    }

    /// Move a tab, keeping the selection and the recent list pointing at the
    /// same documents rather than at the same positions.
    pub(super) fn reorder_tab(&mut self, from: usize, to: usize) {
        if from >= self.docs.len() || to >= self.docs.len() || from == to {
            return;
        }
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);

        // Every index at or after the smaller of the two has moved.
        let remap = |i: usize| {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        };
        self.active = self.active.map(remap);
        for index in &mut self.mru {
            *index = remap(*index);
        }
    }

    /// Ctrl+Tab: step through tabs in the order they were last looked at.
    ///
    /// Most-recently-used rather than left-to-right, because the tab you want
    /// next is nearly always the one you were just in — and pressing it twice
    /// should return you there, not walk the strip.
    pub(super) fn cycle_tab(&mut self, backwards: bool) {
        if self.docs.len() < 2 {
            return;
        }
        self.refresh_mru();
        let here = self
            .active
            .and_then(|a| self.mru.iter().position(|i| *i == a))
            .unwrap_or(0);
        let step = if backwards { -1isize } else { 1 };
        let next = (here as isize + step).rem_euclid(self.mru.len() as isize) as usize;

        self.cycling = true;
        self.active = self.mru.get(next).copied();
        self.focus_active();
    }

    /// Keep the recent list holding exactly the open tabs, once each.
    pub(super) fn refresh_mru(&mut self) {
        self.mru.retain(|i| *i < self.docs.len());
        self.mru.dedup();
        for i in 0..self.docs.len() {
            if !self.mru.contains(&i) {
                self.mru.push(i);
            }
        }
    }

    /// Record the active tab as the most recent, once Ctrl is let go.
    ///
    /// Doing it on every switch would move the tab being left to the front
    /// mid-cycle, so a second Ctrl+Tab would bounce back rather than going on.
    pub(super) fn settle_mru(&mut self, ctx: &egui::Context) {
        if self.cycling && !ctx.input(|i| i.modifiers.ctrl || i.modifiers.command) {
            self.cycling = false;
        }
        if self.cycling {
            return;
        }
        let Some(active) = self.active else { return };
        if self.mru.first() == Some(&active) {
            return;
        }
        self.mru.retain(|i| *i != active);
        self.mru.insert(0, active);
        self.refresh_mru();
    }

    /// Apply the on-save whitespace policies before writing.
    ///
    /// As a transaction, so it lands in the undo history: saving and pressing
    /// undo gets the whitespace back, which is what someone who put it there
    /// deliberately would expect.
    pub(super) fn tidy_before_saving(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let policy = self.save_policy(entry.doc.path());
        if policy.is_noop() {
            return;
        }
        let Some(tx) = editor_core::whitespace::tidy(entry.doc.text(), policy) else {
            return;
        };

        let Some(entry) = self.docs.get_mut(index) else {
            return;
        };
        let before = entry.view.selection;
        let after = before.clamped(entry.doc.len_chars());
        entry.doc.break_undo_run();
        entry.doc.apply(&tx, before, after);
        entry.doc.break_undo_run();
        // The edits may have deleted the text the caret was sitting in.
        entry
            .view
            .set_caret(entry.view.selection.head.min(entry.doc.len_chars()));
    }

    /// What to tidy on save, with a project's `.editorconfig` taking priority.
    pub(super) fn save_policy(&self, path: Option<&Path>) -> editor_core::whitespace::OnSave {
        let mut policy = editor_core::whitespace::OnSave {
            trim_trailing_whitespace: self.settings.trim_trailing_whitespace(),
            ensure_final_newline: self.settings.insert_final_newline(),
        };
        if let Some(path) = path.filter(|_| self.settings.use_editorconfig()) {
            let style = editor_config::editorconfig::style_for(path);
            if let Some(trim) = style.trim_trailing_whitespace {
                policy.trim_trailing_whitespace = trim;
            }
            if let Some(newline) = style.insert_final_newline {
                policy.ensure_final_newline = newline;
            }
        }
        policy
    }

    /// Open a file and put the caret at a zero-based line and column.
    pub(super) fn open_at(&mut self, path: &Path, line: usize, column: usize) {
        self.open_path(path, false);
        if let Some(entry) = self.active_mut() {
            let offset = entry.doc.offset_at(line, column);
            entry.view.set_caret(offset);
            entry.view.focus();
        }
    }

    /// React to files changing outside The Editor.
    pub(super) fn poll_watcher(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let changes = watcher.drain();
        if changes.is_empty() {
            return;
        }
        self.environment
            .changed(&changes.touched, changes.structural);
        if changes.structural {
            self.tree.refresh();
        }

        for path in &changes.touched {
            if let Some(index) = self.docs.iter().position(|d| d.doc.path() == Some(path)) {
                self.reconcile_with_disk(index);
            }
        }
    }

    /// Open whatever the command line named, once, after the session restore.
    ///
    /// After, not instead: restoring the previous session and then opening the
    /// file you asked for leaves you where you were with the new file in front,
    /// which is what every editor does and what you want when the invocation
    /// came from a `git commit` hook or an "open in editor" button.
    ///
    /// A folder argument becomes the project. Several folders and the first
    /// wins, because there is one explorer pane.
    pub(super) fn open_from_command_line(&mut self) {
        if self.from_command_line.is_empty() {
            return;
        }
        let mut folder_taken = false;
        for path in std::mem::take(&mut self.from_command_line) {
            // Relative paths are relative to the shell's directory, and stay
            // usable only until something else changes ours.
            let path = std::fs::canonicalize(&path).map_or(path, plain_path);
            if path.is_dir() {
                if !folder_taken {
                    folder_taken = true;
                    self.open_folder(path);
                }
                continue;
            }
            self.open_path(&path, false);
        }
    }

    /// Re-check every open file when the window regains focus.
    ///
    /// The watcher only covers the open project folder, so a file opened from
    /// anywhere else — and every file when no folder is open at all — would
    /// otherwise never be checked. Coming back to the window is also exactly
    /// when you have been off editing the thing somewhere else, which is the
    /// case this is for.
    ///
    /// One `stat` per open document, on a transition rather than every frame.
    pub(super) fn check_disk_on_focus(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let regained = focused && !self.was_focused;
        self.was_focused = focused;
        if !regained {
            return;
        }
        for index in 0..self.docs.len() {
            self.reconcile_with_disk(index);
        }
    }

    /// Work out what happened to one document's file, and react.
    ///
    /// A clean buffer is reloaded without asking: there is nothing to lose, and
    /// prompting for it is the kind of dialogue people learn to dismiss without
    /// reading. A dirty one raises the bar in `disk_bar` instead, because
    /// saving over a file that `git checkout` has rewritten is how work gets
    /// lost, and that decision is not the editor's to make.
    pub(super) fn reconcile_with_disk(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        match disk_response(entry.doc.disk_state(), entry.doc.is_dirty()) {
            // Our own save, or a change already reckoned with. Leave any bar
            // that is up alone; only a fresh change should raise one.
            DiskResponse::Ignore => {}
            DiskResponse::Reload => self.reload_document(index),
            DiskResponse::Ask(state) => self.docs[index].disk = state,
        }
    }

    /// The bar above the editor when the file has changed underneath it.
    ///
    /// Non-modal on purpose. A modal here interrupts whatever you were typing
    /// to ask about something you may not care about yet, and the honest answer
    /// is often "let me look at what I have first".
    pub(super) fn disk_bar(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let state = entry.disk;
        if state == DiskState::Unchanged {
            return;
        }
        let name = entry.doc.display_name();

        let mut reload = false;
        let mut keep = false;
        let mut save_back = false;

        egui::Frame::default()
            .fill(ui.visuals().warn_fg_color.gamma_multiply(0.15))
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| match state {
                    DiskState::Modified => {
                        ui.label(format!(
                            "{name} changed on disk, and this tab has unsaved edits."
                        ));
                        reload = ui
                            .button("Reload")
                            .on_hover_text("Throw away the edits in this tab and re-read the file")
                            .clicked();
                        keep = ui
                            .button("Keep mine")
                            .on_hover_text(
                                "Keep what is in this tab; saving will overwrite the file",
                            )
                            .clicked();
                    }
                    DiskState::Deleted => {
                        ui.label(format!(
                            "{name} was deleted on disk. This tab is the only copy."
                        ));
                        save_back = ui.button("Save it back").clicked();
                        keep = ui.button("Dismiss").clicked();
                    }
                    DiskState::Unchanged => {}
                });
            });
        ui.separator();

        if reload {
            self.reload_document(index);
        } else if keep {
            // Adopt what is on disk as the baseline so the same change is not
            // reported again on the next filesystem event.
            if let Some(entry) = self.docs.get_mut(index) {
                entry.doc.accept_disk_state();
                entry.disk = DiskState::Unchanged;
            }
        } else if save_back {
            match self.docs[index].doc.save() {
                Ok(()) => {
                    self.docs[index].disk = DiskState::Unchanged;
                    self.tree.refresh();
                    self.info(format!("Wrote {name} back to disk"));
                }
                Err(e) => self.error(format!("Could not write it back: {e:#}")),
            }
        }
    }

    /// Re-read a document from disk, keeping the caret where it was.
    pub(super) fn reload_document(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            return;
        };
        let caret = entry.view.selection.head;

        match Document::open(&path) {
            Ok(doc) => {
                let entry = &mut self.docs[index];
                entry.highlighter = new_highlighter(entry.language, &doc);
                entry.doc = doc;
                entry.view.set_caret(caret.min(entry.doc.len_chars()));
                // The freshly opened document carries the file's current mtime,
                // so the question the bar was asking has now been answered.
                entry.disk = DiskState::Unchanged;
                tracing::info!(path = %path.display(), "reloaded after an external change");
            }
            Err(e) => self.error(format!("Could not reload {}: {e:#}", path.display())),
        }
    }
}
