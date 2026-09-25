//! The frame around the editor: menus, toolbar, status bar, the editor pane
//! itself, the welcome screen, toasts, the small windows, and appearance.

use super::*;

impl EditorApp {
    pub(super) fn set_theme(&mut self, pref: ThemePreference) {
        self.settings.set_theme(pref);
        self.persist_settings();
        // Force a re-apply on the next frame even if the resolved theme is
        // unchanged, so switching Dark -> System with a dark OS still records
        // the new preference.
        self.applied_theme = None;
    }

    pub(super) fn nudge_scale(&mut self, delta: f32) {
        let next = stepped_scale(self.settings.ui_scale(), delta);
        self.settings.set_ui_scale(next);
        self.persist_settings();
    }

    pub(super) fn persist_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.error(format!("Could not save settings: {e:#}"));
        }
    }

    /// Apply the theme, interface font size and zoom if any has changed since
    /// the last frame.
    pub(super) fn sync_appearance(&mut self, ctx: &egui::Context) {
        let resolved = ui_theme::resolve(ctx, self.settings.theme());
        let scale = self.settings.ui_scale();
        let font_size = self.settings.ui_font_size();

        // Compared against the context's own zoom rather than only against
        // what was last applied, so the setting stays the authority on the
        // number even if something else moves it. Tracking "what did I last
        // push" alone is what let the two come apart in the first place: a
        // zoom factor changed behind this function's back is a difference it
        // could not see, and so never corrected.
        let changed = self.applied_theme != Some(resolved)
            || (self.applied_scale - scale).abs() > f32::EPSILON
            || (ctx.zoom_factor() - scale).abs() > f32::EPSILON
            || (self.applied_ui_font - font_size).abs() > f32::EPSILON;

        if changed {
            ui_theme::apply(ctx, resolved, scale, font_size);
            // The code pane follows the UI theme. PLAN.md §3.11 allows pinning
            // them apart; the setting for that arrives with the settings UI.
            self.syntax_theme = SyntaxTheme::for_ui(resolved);
            self.applied_theme = Some(resolved);
            self.applied_scale = scale;
            self.applied_ui_font = font_size;
        }
    }

    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;
        // Cloned so the closure below does not borrow `self` while the menu is
        // being drawn. Fifteen paths is nothing.
        let recent = self.recent.clone();
        let mut picked_recent = None;
        let mut clear_recent = false;

        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (menu, ids) in MENUS {
                    ui.menu_button(*menu, |ui| {
                        for id in *ids {
                            match id {
                                MenuEntry::Separator => {
                                    ui.separator();
                                }
                                MenuEntry::Item(id) => {
                                    if menu_item(ui, *id).clicked() {
                                        invoked = Some(*id);
                                        ui.close();
                                    }
                                }
                                MenuEntry::Recent => {
                                    if let Some(path) = recent_menu(ui, &recent, &mut clear_recent)
                                    {
                                        picked_recent = Some(path);
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                }
            });
        });

        if clear_recent {
            self.recent.clear();
        }
        if picked_recent.is_some() {
            self.pending_recent = picked_recent;
        }
        invoked
    }

    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;

        // Panel heights follow the text size rather than being fixed, so
        // raising ui.font_size does not clip the toolbar or status bar.
        let row = ui.text_style_height(&egui::TextStyle::Body);

        // What is going on, so Run and Stop can say so. Debugging counts as
        // running: the program is live either way, and a Stop button that does
        // nothing during a debug session is a lie.
        let debugging = self.debug.is_some();
        let busy = self.runner.is_running() || self.runner.has_queued_work() || debugging;
        let what = if debugging { "debugging" } else { "running" };

        egui::Panel::top("toolbar")
            .exact_size(row * 2.2)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    for group in TOOLBAR {
                        for id in *group {
                            let cmd = commands::get(*id);
                            let tip = cmd.shortcut_text(ui.ctx()).map_or_else(
                                || cmd.title.to_owned(),
                                |sc| format!("{} ({sc})", cmd.title),
                            );
                            let glyph = editor_widgets::icon::pick(ui, toolbar_glyphs(*id));

                            match id {
                                // While something is live, Run is spent and
                                // Stop is the live one. Saying so with enabled
                                // state and colour rather than with a changing
                                // glyph, because a button whose icon changes
                                // under the pointer is harder to aim at than
                                // one that greys out.
                                CommandId::Run if busy => {
                                    ui.add_enabled(false, egui::Button::new(glyph))
                                        .on_disabled_hover_text(format!("Already {what}"));
                                    // An actual moving thing, so it is obvious
                                    // at a glance that this is not a stalled
                                    // window.
                                    ui.spinner();
                                }
                                CommandId::RunStop => {
                                    let button = egui::Button::new(
                                        egui::RichText::new(glyph).color(if busy {
                                            ui.visuals().error_fg_color
                                        } else {
                                            ui.visuals().weak_text_color()
                                        }),
                                    );
                                    if ui
                                        .add_enabled(busy, button)
                                        .on_hover_text(if debugging {
                                            "Stop debugging".to_owned()
                                        } else {
                                            tip.clone()
                                        })
                                        .on_disabled_hover_text("Nothing is running")
                                        .clicked()
                                    {
                                        invoked = Some(if debugging {
                                            CommandId::DebugStop
                                        } else {
                                            CommandId::RunStop
                                        });
                                    }
                                }
                                _ => {
                                    if ui.button(glyph).on_hover_text(tip).clicked() {
                                        invoked = Some(*id);
                                    }
                                }
                            }
                        }
                        ui.separator();
                    }
                });
            });

        invoked
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;

        // Read what the status bar needs before the closure borrows self.
        let summary = self.active_doc().map(|entry| {
            let (line, column) = entry.view.cursor_position(&entry.doc);
            StatusSummary {
                language: entry.language.display_name(),
                encoding: entry.doc.encoding().label(),
                eol: entry.doc.line_ending().label(),
                lines: entry.doc.line_count(),
                read_only: entry.doc.read_only().is_some(),
                line,
                column,
                selected: entry.view.selection_len(),
            }
        });
        // What will run this file, in place of the bare language name.
        let runtime = match self.active_doc().map(|entry| entry.language) {
            Some(LanguageId::Python) => Some(self.python_status()),
            Some(LanguageId::Rust) => Some(self.rust_status()),
            _ => None,
        };
        let theme_label = self.settings.theme().label();
        // As applied to this file, `.editorconfig` included, not just as set.
        let EditorOptions {
            tab_width,
            insert_spaces,
            ..
        } = self.editor_options();
        let running = self.runner.is_running() || self.runner.has_queued_work();
        let diagnostic_counts = self.lsp.diagnostics().total_counts();
        let checkers = self.checker_summary();
        let has_run = self.runner.output().line_count() > 1;
        let run_label = self.runner.label().to_owned();
        // The branch, and how it stands against its upstream once the listing
        // has come back. The name alone comes from a cheaper question, so it is
        // on screen from the first frame and the counts fill in behind it.
        let branch = self.git.branch().map(|name| {
            let track = self.git.current_branch().map_or_else(String::new, |b| {
                b.track_summary(
                    editor_widgets::glyphs::AHEAD,
                    editor_widgets::glyphs::BEHIND,
                )
            });
            if track.is_empty() {
                name.to_owned()
            } else {
                format!("{name}  {track}")
            }
        });

        let row = ui.text_style_height(&egui::TextStyle::Body);

        egui::Panel::bottom("status_bar")
            .exact_size(row * 1.6)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    match &summary {
                        Some(s) => {
                            ui.weak(format!("Ln {}, Col {}", s.line, s.column));
                            if s.selected > 0 {
                                ui.weak(format!("({} selected)", s.selected));
                            }
                            ui.separator();
                            match &runtime {
                                Some(runtime) => {
                                    if ui
                                        .button(&runtime.text)
                                        .on_hover_text(&runtime.hover)
                                        .clicked()
                                    {
                                        invoked = Some(runtime.command);
                                    }
                                }
                                None => {
                                    ui.weak(s.language);
                                }
                            }
                            ui.separator();
                            ui.weak(s.encoding);
                            ui.separator();
                            ui.weak(s.eol);
                            ui.separator();
                            ui.weak(format!(
                                "{}: {tab_width}",
                                if insert_spaces { "Spaces" } else { "Tabs" }
                            ));
                            ui.separator();
                            ui.weak(format!("{} lines", s.lines));
                            if s.read_only {
                                ui.separator();
                                ui.weak("Read-only");
                            }
                        }
                        None => {
                            ui.weak("Ready");
                        }
                    }

                    // Problem counts, and a way to the panel listing them.
                    //
                    // Shown even at zero. A blank space where the count should
                    // be is read as "nothing is wrong", which is the same thing
                    // "nothing is checking" looks like — and the hover is the
                    // only place that difference is stated.
                    if summary.is_some() {
                        ui.separator();
                        let text = if diagnostic_counts.is_empty() {
                            format!("{} No problems", editor_widgets::glyphs::OK)
                        } else {
                            format!(
                                "{} {}  {} {}",
                                editor_lsp::diagnostics::Severity::Error.glyph(),
                                diagnostic_counts.errors,
                                editor_lsp::diagnostics::Severity::Warning.glyph(),
                                diagnostic_counts.warnings
                            )
                        };
                        if ui.button(text).on_hover_text(&checkers).clicked() {
                            invoked = Some(CommandId::ShowProblems);
                        }
                    }

                    // What is running, and a way back to its output. Without
                    // this, a hidden output panel means a running process with
                    // nothing on screen to say so.
                    if running || has_run {
                        ui.separator();
                        let text = if running {
                            format!("\u{25b6} {run_label}")
                        } else {
                            format!("\u{25a0} {run_label}")
                        };
                        if ui
                            .button(text)
                            .on_hover_text("Show the output panel")
                            .clicked()
                        {
                            invoked = Some(CommandId::ShowOutput);
                        }
                    }

                    // The branch, when the project is a repository. Absent
                    // rather than empty when it is not: a blank space labelled
                    // "branch" invites the question of which one.
                    //
                    // Labelled rather than given a branch icon, because the
                    // bundled fonts have no glyph for one and an unlabelled
                    // name reads as another of the values beside it.
                    if let Some(branch) = &branch {
                        ui.separator();
                        ui.weak(format!("Branch: {branch}"))
                            .on_hover_text("The branch this project is on");
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The theme indicator is a control, not a label — one
                        // of the three ways PLAN.md §3.11 requires it to be
                        // reachable.
                        if ui
                            .button(format!("{} {theme_label}", editor_widgets::glyphs::THEME))
                            .on_hover_text("Change theme")
                            .clicked()
                        {
                            invoked = Some(next_theme_command(theme_label));
                        }
                        ui.separator();
                        ui.weak(format!("v{}", BUILD.version));
                    });
                });
            });

        invoked
    }

    pub(super) fn editor_pane(&mut self, ui: &mut egui::Ui) -> Option<tab_bar::Action> {
        let mut tab_action = None;

        egui::CentralPanel::default().show(ui, |ui| {
            if self.docs.is_empty() {
                if let Some(path) = self.welcome(ui) {
                    self.pending_recent = Some(path);
                }
                return;
            }

            let tabs: Vec<tab_bar::TabInfo> = self
                .docs
                .iter()
                .map(|entry| tab_bar::TabInfo {
                    title: entry.doc.display_name(),
                    tooltip: entry
                        .doc
                        .path()
                        .map_or_else(|| "Unsaved".to_owned(), |p| p.display().to_string()),
                    dirty: entry.doc.is_dirty(),
                    preview: entry.preview,
                })
                .collect();

            let action = tab_bar::ui(ui, &tabs, self.active);
            if action != tab_bar::Action::None {
                tab_action = Some(action);
            }
            ui.separator();

            // Directly under the tabs, above everything else in the pane:
            // whatever happened to the file on disk outranks a search bar.
            // This also has to come before the borrows below, which hold
            // `self` for the rest of the closure.
            if let Some(index) = self.active {
                self.disk_bar(ui, index);
            }

            let mut opts = self.editor_options();
            let syntax = &self.syntax_theme;
            let underline_level = self.settings.underline_diagnostics();
            // Read before the mutable borrow below. Breakpoints are stored
            // one-based, as the gutter and the protocol both count them, and
            // converted here for painting.
            let active_path = self
                .active
                .and_then(|i| self.docs.get(i))
                .and_then(|e| e.doc.path())
                .map(Path::to_path_buf);
            let breakpoints_here: Vec<(usize, bool)> = active_path
                .as_ref()
                .map(|p| {
                    self.breakpoints
                        .for_file(p)
                        .into_iter()
                        .map(|line| (line.saturating_sub(1), true))
                        .collect()
                })
                .unwrap_or_default();
            let paused_line = self.debug_view.location().and_then(|(path, line)| {
                (Some(&path) == active_path.as_ref()).then(|| line.saturating_sub(1))
            });
            let diagnostics = self.lsp.diagnostics();

            // How the active buffer differs from the committed version. Read
            // here, before `entry` is borrowed mutably, because the tracker and
            // the documents are separate fields and the closure below would
            // otherwise borrow both at once.
            //
            // Only the active tab: the marks are only ever drawn for the file
            // on screen, and diffing the others would be work for nobody.
            let changes = active_path
                .as_ref()
                .zip(self.active.and_then(|i| self.docs.get(i)))
                .map(|(path, entry)| {
                    self.git
                        .marks(
                            path,
                            entry.doc.version(),
                            entry.doc.text().len_bytes(),
                            || entry.doc.text().to_string(),
                        )
                        .to_vec()
                })
                .unwrap_or_default();

            // Who last touched each line, when the annotations are switched on.
            // Read from the file *on disk*, so unsaved edits shift it — which
            // is what the message on switching it on says, and why saving asks
            // for it again.
            let blame: Vec<(usize, String)> = if self.show_blame {
                active_path
                    .as_ref()
                    .and_then(|path| self.git.blame(path))
                    .map(|lines| {
                        lines
                            .iter()
                            .map(|line| (line.number.saturating_sub(1), line.origin.label()))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) {
                opts.language = entry.language;
                // The parse tree was brought up to date by `sync_highlighters`
                // earlier this frame, so it is safe to paint from here.
                if entry.find.is_open() {
                    // The *start* of the selection, not its head: Find opened
                    // on a selected word should land on that word, and the
                    // head sits at its far end, one character past the only
                    // match the user cared about.
                    let caret = entry.view.selection.start();
                    let mut action = entry.find.ui(ui, &entry.doc, caret);
                    // A keyboard Find Next arrives as a pending step rather
                    // than a click, so it takes the same path.
                    if let Some(direction) = entry.pending_find_step.take()
                        && let Some(range) = entry.find.step_from(caret, direction)
                    {
                        action = find_bar::Action::Reveal(range);
                    }
                    apply_find_action(entry, action);
                    ui.separator();
                }
                entry
                    .view
                    .set_search_matches(entry.find.matches(), entry.find.current_match());
                entry.view.set_diagnostics(Self::underlines_for(
                    entry,
                    diagnostics,
                    underline_level,
                ));
                entry
                    .view
                    .set_debug_state(breakpoints_here.clone(), paused_line);
                entry.view.set_changes(changes);
                entry.view.set_blame(blame);

                // Editing a preview tab promotes it: the file is being worked
                // on, so it must not be replaced by the next explorer click.
                if entry
                    .view
                    .ui(ui, &mut entry.doc, entry.highlighter.as_mut(), syntax, opts)
                {
                    entry.preview = false;
                }
            }
        });

        tab_action
    }

    /// The empty state. Returns a recent file if one was clicked.
    ///
    /// The recent list is repeated here as well as in the menu because this is
    /// the screen you are looking at when you want it: an editor opened with
    /// nothing in it is almost always about to reopen something.
    pub(super) fn welcome(&self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut picked = None;
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("The Editor");
            ui.label(format!("Version {}", BUILD.version));
            ui.add_space(24.0);

            egui::Grid::new("welcome_hints")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    for id in [
                        CommandId::OpenFolder,
                        CommandId::OpenFile,
                        CommandId::NewFile,
                        CommandId::CommandPalette,
                    ] {
                        let cmd = commands::get(id);
                        ui.weak(cmd.title);
                        ui.weak(cmd.shortcut_text(ui.ctx()).unwrap_or_default());
                        ui.end_row();
                    }
                });

            if self.recent.is_empty() {
                return;
            }
            ui.add_space(28.0);
            ui.weak("Recent");
            ui.add_space(4.0);
            for path in self.recent.iter().take(8) {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let response = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(name).color(ui.visuals().hyperlink_color),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(path.display().to_string())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    picked = Some(path.clone());
                }
            }
        });
        picked
    }

    pub(super) fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.born.elapsed() < TOAST_LIFETIME);
        if self.toasts.is_empty() {
            return;
        }
        // Keep repainting while any toast is on screen so it expires on time
        // rather than when the next input arrives.
        ctx.request_repaint_after(Duration::from_millis(250));

        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -40.0))
            .interactable(false)
            .show(ctx, |ui| {
                for toast in &self.toasts {
                    let colour = if toast.error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().text_color()
                    };
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.colored_label(colour, &toast.text);
                    });
                }
            });
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About The Editor")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.heading("The Editor");
                ui.label(format!("Version {}", BUILD.version));
                ui.add_space(6.0);
                ui.label("An IDE for Python and Rust.");
                ui.add_space(6.0);
                ui.label(format!(
                    "Copyright \u{a9} 2026 {}",
                    editor_config::APP_AUTHOR
                ));
                ui.label("Licensed under the MIT licence.");
                ui.add_space(10.0);
                ui.separator();
                egui::Grid::new("about_build")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Build");
                        ui.label(format!("{} ({})", BUILD.commit, BUILD.date));
                        ui.end_row();
                        ui.label("Compiler");
                        ui.label(BUILD.rustc);
                        ui.end_row();
                        ui.label("Config");
                        ui.label(self.paths.config_dir().display().to_string());
                        ui.end_row();
                        ui.label("Logs");
                        ui.label(&self.log_dir);
                        ui.end_row();
                        if self.paths.is_portable() {
                            ui.label("Mode");
                            ui.label("Portable");
                            ui.end_row();
                        }
                    });
            });
        self.show_about = open;
    }

    /// Draw the Settings window and act on what it asked for.
    ///
    /// The window mutates `self.settings` directly, so the only work here is
    /// persisting the change and routing the two buttons that need the
    /// application's help. Nothing needs to be re-applied: `sync_appearance`
    /// already re-reads the theme, zoom and font size every frame, and the
    /// editor options are read fresh each time the editor is drawn.
    pub(super) fn settings_form_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_form.is_open() {
            return;
        }

        let detected = self.detected_interpreter();
        let running = self.lsp.running();
        let action = self
            .settings_form
            .ui(ctx, &mut self.settings, detected.as_deref(), &running);

        match action {
            settings_window::Action::None => {}
            settings_window::Action::Changed => self.persist_settings(),
            settings_window::Action::OpenFile => self.run_command(CommandId::OpenSettingsFile, ctx),
            settings_window::Action::PickInterpreter => {
                self.run_command(CommandId::SelectInterpreter, ctx);
                // The picker writes the setting; refresh the field behind it so
                // the window is not still showing the old path.
                self.settings_form.open(&self.settings);
            }
            settings_window::Action::CheckToolchains => {
                self.run_command(CommandId::CheckToolchains, ctx);
            }
        }
    }

    /// What optional tooling is installed, and what each missing piece would
    /// buy.
    ///
    /// The Editor works with none of it — PLAN.md §3.6 — but "works without"
    /// must not shade into "silently does less than you think". Someone whose
    /// broken Python shows only a syntax error needs a way to find out that no
    /// type checker is installed, and what to install.
    pub(super) fn toolchains_window(&mut self, ctx: &egui::Context) {
        let Some(found) = self.toolchains.as_ref() else {
            return;
        };
        let found = found.clone();

        let mut open = true;
        egui::Window::new("Check Toolchains")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    "The Editor checks syntax on its own. Everything below is optional \
                     and adds deeper analysis.",
                );
                ui.add_space(8.0);

                egui::Grid::new("toolchains")
                    .num_columns(4)
                    .striped(true)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.strong("Tool");
                        ui.strong("Status");
                        ui.strong("Provides");
                        ui.strong("Install with");
                        ui.end_row();

                        for spec in editor_lsp::registry::ALL {
                            let installed = found.iter().find(|f| f.spec.id == spec.id);
                            ui.label(spec.name);
                            match installed {
                                Some(f) => {
                                    ui.colored_label(
                                        severity_colour(
                                            ui.visuals(),
                                            editor_lsp::diagnostics::Severity::Hint,
                                        ),
                                        "Installed",
                                    )
                                    .on_hover_text(f.program.display().to_string());
                                    ui.weak(spec.provides);
                                    // Nothing to do, so nothing to copy.
                                    ui.weak("\u{2014}");
                                }
                                None => {
                                    ui.weak("Not found");
                                    ui.weak(spec.provides);
                                    ui.horizontal(|ui| {
                                        ui.code(spec.install);
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(spec.install.to_owned());
                                        }
                                    });
                                }
                            }
                            ui.end_row();
                        }
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.small(
                    "Paste the command into a terminal. `pip` installs into whichever Python \
                     is on your PATH; to lint a project with its own virtual environment, \
                     activate it first and The Editor will prefer the tools it finds there.",
                );

                ui.add_space(8.0);
                if ui.button("Re-check").clicked() {
                    self.toolchains =
                        Some(editor_lsp::registry::find_all(&self.tool_search_path()));
                }
            });

        if !open {
            self.toolchains = None;
        }
    }

    /// Generated from the registry, so it cannot describe a binding that does
    /// not exist. PLAN.md §3.10.
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_shortcuts;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .show(ui, |ui| {
                        egui::Grid::new("shortcut_list")
                            .num_columns(2)
                            .striped(true)
                            .spacing([24.0, 4.0])
                            .show(ui, |ui| {
                                for cmd in commands::registry() {
                                    ui.label(cmd.palette_label());
                                    match cmd.shortcut_text(ui.ctx()) {
                                        Some(sc) => ui.label(sc),
                                        None => ui.weak("\u{2014}"),
                                    };
                                    ui.end_row();
                                }
                            });
                    });
            });
        self.show_shortcuts = open;
    }
}
