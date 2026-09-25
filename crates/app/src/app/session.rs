//! The session and crash recovery: what was open, where the window was, and
//! the unsaved work a crash left behind.

use super::*;

impl EditorApp {
    /// Reopen what was open last time.
    ///
    /// Runs on the first frame rather than in `new`, because the window has to
    /// exist before its remembered geometry can be checked against the
    /// monitors that are actually attached.
    pub(super) fn restore_session(&mut self, session: &Session, ctx: &egui::Context) {
        if let Some(geometry) = session.window {
            let monitor = ctx.input(|i| i.viewport().monitor_size);
            let visible = monitor.is_none_or(|size| geometry.is_on_screen(size.x, size.y));

            if visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    geometry.x, geometry.y,
                )));
            } else {
                // The monitor it was on is gone. Keep the size, drop the
                // position, and let the window manager place it.
                tracing::info!("remembered window position is off-screen; ignoring it");
            }

            // The size to come back to when un-maximized, which is only the
            // remembered one if the remembered one is a real window rather
            // than the screen written down by an older build — see
            // `fills_the_screen`. Dropping it leaves the window at the size it
            // was built with, which is at least a window.
            let stale = geometry.maximized
                && monitor.is_some_and(|size| geometry.fills_the_screen(size.x, size.y));
            let normal = (!stale).then_some(geometry);
            if let Some(normal) = normal {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    normal.width,
                    normal.height,
                )));
            }
            self.restored_window = normal.map(|g| WindowGeometry {
                maximized: false,
                ..g
            });
            self.awaiting_window = self.restored_window;

            if geometry.maximized {
                // After the size, so the window has somewhere to go back to.
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }

        // Restored first, so that reopening the previous tabs below pushes
        // them to the front of a list that already has the older history in it
        // rather than replacing it.
        self.recent.clone_from(&session.recent_files);
        for (path, lines) in &session.breakpoints {
            self.breakpoints.set_file(path.clone(), lines.clone());
        }

        if let Some(folder) = &session.folder {
            self.open_folder(folder.clone());
        }
        for file in &session.open_files {
            self.open_path(&file.path, false);
            if let Some(entry) = self.active_mut() {
                let caret = file.caret.min(entry.doc.len_chars());
                entry.view.set_caret(caret);
            }
        }
        if let Some(active) = session.active.filter(|i| *i < self.docs.len()) {
            self.active = Some(active);
            self.focus_active();
        }
        self.show_output = session.show_output;
    }

    /// Note where the window is, if it is a normal window right now.
    ///
    /// Runs every frame. A maximized window is skipped rather than recorded:
    /// its size and position are the screen's, and the point of remembering
    /// geometry is to give the user back the window *they* sized.
    pub(super) fn track_window(&mut self, ctx: &egui::Context) {
        let Some((geometry, maximized)) = ctx.input(|i| {
            let viewport = i.viewport();
            // Position comes from the outer rect — that is where the window
            // actually is — but the size comes from the *inner* rect, because
            // that is what `InnerSize` sets when restoring. Saving the outer
            // size and restoring it as the inner one makes the window grow by
            // the height of its own title bar on every launch.
            let outer = viewport.outer_rect?;
            let inner = viewport.inner_rect?;
            Some((
                WindowGeometry {
                    x: outer.min.x,
                    y: outer.min.y,
                    width: inner.width(),
                    height: inner.height(),
                    maximized: false,
                },
                viewport.maximized.unwrap_or(false),
            ))
        }) else {
            return;
        };

        self.window_maximized = maximized;
        if maximized {
            return;
        }
        // Wait for the restored geometry to arrive before believing what the
        // window says about itself; see `awaiting_window`. A maximized restore
        // clears this on the first un-maximize, which is the first moment the
        // remembered size is on screen to be seen.
        if let Some(wanted) = self.awaiting_window {
            let close = |a: f32, b: f32| (a - b).abs() <= 2.0;
            if !close(geometry.width, wanted.width) || !close(geometry.height, wanted.height) {
                return;
            }
            self.awaiting_window = None;
        }
        self.restored_window = Some(geometry);
    }

    /// Gather the current state for writing out.
    pub(super) fn current_session(&self) -> Session {
        // The *normal* geometry, with whether the window is maximized on top
        // of it — the two are independent, and a maximized window still has a
        // size to come back to. Both come from `track_window` rather than from
        // the viewport here, because by the time the window is closing it is
        // too late to ask what size it used to be.
        let window = self.restored_window.map(|geometry| WindowGeometry {
            maximized: self.window_maximized,
            ..geometry
        });

        Session {
            breakpoints: self.breakpoints.flatten(),
            recent_files: self.recent.clone(),
            folder: self.tree.root().map(Path::to_path_buf),
            open_files: self
                .docs
                .iter()
                .filter_map(|entry| {
                    Some(OpenFile {
                        path: entry.doc.path()?.to_path_buf(),
                        caret: entry.view.selection.head,
                    })
                })
                .collect(),
            active: self.active,
            window: window.filter(|w| w.is_plausible()),
            show_output: self.show_output,
        }
    }

    pub(super) fn save_session(&mut self) {
        if self.session_saved || !self.settings.restore_session() {
            return;
        }
        let session = self.current_session();
        if let Err(e) = session.save(&self.paths.session_file()) {
            tracing::warn!("could not save the session: {e}");
        }
        self.session_saved = true;
    }

    pub(super) fn claim_recovery_id(&mut self) -> u64 {
        let id = self.next_recovery_id;
        self.next_recovery_id += 1;
        id
    }

    /// Copy unsaved buffers aside, so a crash cannot take them.
    ///
    /// Watches the set of `(id, version)` pairs for dirty documents rather than
    /// being called from each edit path. Every way text can change — typing,
    /// paste, undo, a project-wide replace, a rename applied from the language
    /// server — bumps the version, so nothing can be added later that forgets
    /// to schedule a write.
    pub(super) fn autosave(&mut self, ctx: &egui::Context) {
        self.recovery.beat();

        let now: Vec<(u64, u64)> = self
            .docs
            .iter()
            .filter(|d| d.doc.is_dirty())
            .map(|d| (d.recovery_id, d.doc.version()))
            .collect();

        if now != self.dirty_seen {
            // Anything that stopped being dirty was saved, closed, or undone
            // back to what is on disk. Either way its copy is now a lie.
            for (id, _) in &self.dirty_seen {
                if !now.iter().any(|(other, _)| other == id) {
                    self.recovery.discard(*id);
                }
            }
            self.dirty_seen = now;
            self.recovery.mark_dirty();
        }

        if self.recovery.is_due() {
            for entry in &self.docs {
                if !entry.doc.is_dirty() {
                    continue;
                }
                self.recovery.store(
                    entry.recovery_id,
                    entry.doc.version(),
                    entry.doc.path(),
                    &entry.doc.display_name(),
                    &entry.doc.text().to_string(),
                );
            }
            self.recovery.settle();
        }

        // Without this the write never happens on an idle editor: type a
        // sentence, walk away, and the copy is made only when you come back.
        if let Some(wait) = self.recovery.next_wake() {
            ctx.request_repaint_after(wait);
        }
    }

    /// Log how long it took to get to the first frame, once.
    ///
    /// PLAN.md §M9 asks for under 500 ms cold to interactive. A number nobody
    /// measures is a number that drifts, and startup is the one figure a user
    /// notices every single time without ever being able to say what it was.
    pub(super) fn report_startup(&mut self) {
        let Some(started) = self.started.take() else {
            return;
        };
        let took = started.elapsed();
        tracing::info!(
            millis = took.as_millis(),
            budget_millis = 500u64,
            within_budget = took <= Duration::from_millis(500),
            "first frame"
        );
    }

    /// Shown once, the first time The Editor is run.
    ///
    /// Three things and then out of the way. A first-run wizard that walks
    /// through every setting is a wizard people click through without reading;
    /// what actually helps is knowing that a *folder* is the unit of work, that
    /// the language tools are separate and checkable, and that there is a
    /// manual.
    pub(super) fn first_run_ui(&mut self, ctx: &egui::Context) {
        if !self.first_run {
            return;
        }
        let mut dismiss = false;
        let mut then = None;

        egui::Modal::new(egui::Id::new("first_run")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Welcome to The Editor");
            ui.add_space(4.0);
            ui.weak(format!("Version {}", BUILD.version));
            ui.add_space(12.0);

            ui.label(
                "An IDE for Python and Rust. No account, no installer, and \
                 nothing sent anywhere.",
            );
            ui.add_space(12.0);

            ui.label(egui::RichText::new("Open a folder, not a file").strong());
            ui.label(
                "The folder is the project: it is what the explorer shows, what \
                 search searches, and where The Editor looks for a virtual \
                 environment.",
            );
            ui.add_space(10.0);

            ui.label(egui::RichText::new("Check the toolchains").strong());
            ui.label(
                "Highlighting and syntax errors work on their own. Completion, \
                 go to definition and debugging need tools you may not have \
                 yet; this says which, and how to install them.",
            );
            ui.add_space(10.0);

            ui.label(egui::RichText::new("The manual is in the Help menu").strong());
            ui.label("Along with every keyboard shortcut, as the application actually has them.");

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Open a folder\u{2026}").clicked() {
                    then = Some(CommandId::OpenFolder);
                    dismiss = true;
                }
                if ui.button("Check toolchains").clicked() {
                    then = Some(CommandId::CheckToolchains);
                    dismiss = true;
                }
                if ui.button("User manual").clicked() {
                    then = Some(CommandId::UserManual);
                    dismiss = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Start editing").clicked() {
                        dismiss = true;
                    }
                });
            });
        });

        if dismiss {
            self.first_run = false;
            if let Some(id) = then {
                self.run_command(id, ctx);
            }
        }
    }

    /// Offer back the unsaved work of a session that did not shut down.
    ///
    /// Modal, unlike the disk-change bar. This one is about work that exists
    /// nowhere else, the files are deleted once dismissed, and it happens at
    /// most once per crash — all the reasons the other case is non-modal point
    /// the other way here.
    pub(super) fn recovery_prompt(&mut self, ctx: &egui::Context) {
        if self.recovered.is_empty() {
            return;
        }
        let mut restore = false;
        let mut discard = false;

        egui::Modal::new(egui::Id::new("crash_recovery")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Unsaved work was recovered");
            ui.add_space(6.0);
            ui.label(
                "The Editor did not shut down cleanly. These buffers had \
                      changes that were never saved:",
            );
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .id_salt("recovery_list")
                .max_height(200.0)
                .show(ui, |ui| {
                    for item in &self.recovered {
                        ui.horizontal(|ui| {
                            ui.monospace(&item.name);
                            match item.path.as_ref() {
                                Some(path) => {
                                    ui.weak(shorten_middle(&path.display().to_string(), 52))
                                }
                                None => ui.weak("never saved"),
                            };
                        });
                    }
                });

            ui.add_space(10.0);
            ui.small(
                "Restoring opens each one in a tab with its changes, unsaved. \
                 Undo goes back to what is on disk.",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Restore").clicked() {
                    restore = true;
                }
                if ui
                    .button("Discard")
                    .on_hover_text("Delete the recovered copies. This cannot be undone.")
                    .clicked()
                {
                    discard = true;
                }
            });
        });

        if !restore && !discard {
            return;
        }
        for item in std::mem::take(&mut self.recovered) {
            if restore {
                self.restore_one(&item);
            }
            recovery::dispose(&item.source);
        }
    }

    /// Put one recovered buffer back into a tab.
    pub(super) fn restore_one(&mut self, item: &recovery::Recovered) {
        // Where the file still exists, open it properly first and then replace
        // the text. That keeps its encoding and line endings, and leaves one
        // undo step between the recovered version and what is on disk — which
        // is the comparison anyone will want to make.
        let doc = match item.path.as_ref().map(|p| (p, Document::open(p))) {
            Some((_, Ok(mut doc))) => {
                let end = doc.len_chars();
                let before = editor_core::selection::Selection::at(0);
                doc.apply(
                    &editor_core::edit::Transaction::new(vec![editor_core::edit::Edit::replace(
                        0..end,
                        item.text.clone(),
                    )]),
                    before,
                    before,
                );
                doc.break_undo_run();
                doc
            }
            _ => Document::recovered(item.path.clone(), &item.text),
        };

        let language = item
            .path
            .as_ref()
            .and_then(|p| p.extension())
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
            preview: false,
            syntax_version: None,
            syntax_due: None,
            disk: DiskState::Unchanged,
        };

        // The session restore ran earlier in this same frame and has already
        // reopened the tabs that were open when the crash happened -- and the
        // file being recovered is very likely one of them, read back from disk
        // without the changes that are about to be put back. Taking over that
        // tab rather than adding a second one is the difference between
        // recovering a file and appearing to open it twice, with the same name
        // on two tabs and the newer text on whichever one you happen to click.
        //
        // Only a buffer with a path can be matched, which is the right rule
        // rather than a limitation: a buffer that was never saved is not in
        // the session file either, so there is nothing for it to collide with.
        let slot = match tab_showing(&self.docs, entry.doc.path()) {
            Some(slot) => {
                // The id the reopened tab was given is going out with it. It
                // has nothing written against it yet -- the tab was opened
                // clean this session -- but discarding it keeps the store's
                // bookkeeping honest rather than relying on that.
                self.recovery.discard(self.docs[slot].recovery_id);
                self.docs[slot] = entry;
                slot
            }
            None => {
                self.docs.push(entry);
                self.docs.len() - 1
            }
        };
        self.active = Some(slot);
        self.sync_watched_files();
    }
}
