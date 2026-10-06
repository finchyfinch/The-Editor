//! What the editor knows about the code: language servers, diagnostics,
//! completion, hover, navigation and rename, with the parse-tree fallbacks
//! for when no server can answer.

use super::problems::{
    diagnostic_range, is_underlined, problem_report, problems_at, problems_at_caret,
    problems_on_line,
};
use super::*;

impl EditorApp {
    /// The Problems panel: every diagnostic, grouped by file.
    ///
    /// Returns the location to jump to when a row is clicked.
    pub(super) fn problems_ui(&mut self, ui: &mut egui::Ui) -> Option<(PathBuf, usize, usize)> {
        // Only the file being looked at, unless asked otherwise. A panel
        // listing every open tab's problems buries the ones belonging to the
        // line under the caret, which is what it is being consulted about.
        let here = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|d| d.doc.path())
            .map(Path::to_path_buf);
        let files: Vec<(PathBuf, Vec<editor_lsp::diagnostics::Diagnostic>)> = self
            .lsp
            .diagnostics()
            .all()
            .into_iter()
            .filter(|(path, _)| self.problems_all_files || here.as_ref() == Some(path))
            .collect();

        // The what-is-missing notice sits at the bottom and is drawn first, so
        // it keeps its place while the list above it scrolls. It shows whether
        // or not there are diagnostics: finding a syntax error does not mean a
        // type checker has stopped being missing.
        if self.lsp.running().is_empty() && self.missing_tools_ui(ui) {
            self.toolchains = Some(editor_lsp::registry::find_all(&self.tool_search_path()));
        }

        ui.horizontal(|ui| {
            let mut all = self.problems_all_files;
            if ui
                .checkbox(&mut all, "All open files")
                .on_hover_text("Off shows only the file you are looking at")
                .changed()
            {
                self.problems_all_files = all;
            }
        });
        ui.separator();

        if files.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.weak("No problems");
            });
            return None;
        }

        let mut clicked = None;
        let here_now = self.problem_at_caret.clone();
        // Scroll to it once per change, not every frame.
        let mut reveal = self.problem_at_caret != self.problem_revealed;
        self.problem_revealed = self.problem_at_caret.clone();

        egui::ScrollArea::both()
            .id_salt("problems")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (path, diagnostics) in files {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    ui.horizontal(|ui| {
                        ui.strong(name);
                        ui.weak(format!("({})", diagnostics.len()));
                    });

                    // A file that does not parse cannot be analysed further by
                    // anything. Ruff reports the syntax error and stops;
                    // Pyright does the same. Without saying so, a missing
                    // import that goes unreported below a syntax error looks
                    // like the linter failing rather than waiting.
                    if has_syntax_error(&diagnostics) {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.weak(
                                "\u{2139} This file does not parse, so nothing can check it \
                                 beyond its syntax. Fix the error above and the rest \
                                 will follow.",
                            );
                        });
                    }

                    for diagnostic in diagnostics {
                        // The one the caret is sitting on, so a long list can
                        // be searched from the editor rather than by eye.
                        let at_caret = here_now.as_ref().is_some_and(|(p, line, column)| {
                            *p == path && *line == diagnostic.line && *column == diagnostic.column
                        });
                        let row = ui
                            .horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.colored_label(
                                    severity_colour(ui.visuals(), diagnostic.severity),
                                    diagnostic.severity.glyph(),
                                );
                                ui.weak(format!(
                                    "{}:{}",
                                    diagnostic.line + 1,
                                    diagnostic.column + 1
                                ));
                                let summary = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(diagnostic.summary()).background_color(
                                            if at_caret {
                                                ui.visuals().selection.bg_fill
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            },
                                        ),
                                    )
                                    .sense(egui::Sense::click())
                                    .truncate(),
                                );
                                selection_margin(ui);
                                summary
                            })
                            .inner;

                        if at_caret && std::mem::take(&mut reveal) {
                            // Only when it changed, or the panel would fight
                            // the user for the scrollbar every frame.
                            row.scroll_to_me(Some(egui::Align::Center));
                        }
                        if row
                            .on_hover_text(&diagnostic.message)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            clicked = Some((
                                path.clone(),
                                diagnostic.line as usize,
                                diagnostic.column as usize,
                            ));
                        }
                    }
                    ui.add_space(4.0);
                }
            });
        clicked
    }

    /// The strip at the foot of the Problems panel saying what is not checking
    /// this file, and the exact command that would fix it.
    ///
    /// Returns true if the user asked for the full toolchain window.
    ///
    /// Naming three tools the user has not got and stopping there is not a
    /// report, it is a riddle. Each line carries the command that installs it,
    /// with a button that puts it on the clipboard, because the next thing
    /// anyone does with a command is paste it into a terminal.
    pub(super) fn missing_tools_ui(&self, ui: &mut egui::Ui) -> bool {
        let missing = self.lsp.missing();
        let mut open_toolchains = false;

        egui::Panel::bottom("problems_toolchains").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.small("Syntax is checked by The Editor itself. Nothing else is checking.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_toolchains = ui.small_button("Check Toolchains...").clicked();
                });
            });

            for spec in missing {
                ui.horizontal(|ui| {
                    ui.small(format!("{} \u{2014} {}:", spec.name, spec.provides));
                    ui.code(spec.install);
                    if ui
                        .small_button("Copy")
                        .on_hover_text("Copy the command to the clipboard")
                        .clicked()
                    {
                        ui.ctx().copy_text(spec.install.to_owned());
                    }
                });
            }
            ui.add_space(4.0);
        });

        open_toolchains
    }

    /// Find the diagnostic under the caret, for the Problems panel to reveal.
    ///
    /// Recomputed each frame from the caret rather than set when the user
    /// clicks, so arrowing onto a squiggle reveals it too — and so it clears
    /// itself the moment the caret moves off the line.
    pub(super) fn sync_problem_at_caret(&mut self) {
        self.problem_at_caret = self.diagnostic_under_caret();
    }

    pub(super) fn diagnostic_under_caret(&self) -> Option<(PathBuf, u32, u32)> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let path = entry.doc.path()?;
        let caret = entry.view.selection.head;
        let all = self.lsp.diagnostics().for_file(path);

        problems_at_caret(&entry.doc, &all, caret)
            .first()
            .map(|d| (path.to_path_buf(), d.line, d.column))
    }

    /// Right-click > Copy Problem: the problems at the caret, in full.
    ///
    /// The hover cannot be selected from, and the Problems panel shows only a
    /// first line; this is the way to get the whole message somewhere else.
    pub(super) fn copy_problem_at_caret(&mut self, ctx: &egui::Context) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(path) = entry.doc.path() else {
            return;
        };
        let all = self.lsp.diagnostics().for_file(path);
        let problems = problems_at_caret(&entry.doc, &all, entry.view.selection.head);
        if problems.is_empty() {
            self.info("No problem reported here");
            return;
        }
        let report = problems
            .iter()
            .map(|d| problem_report(path, d))
            .collect::<Vec<_>>()
            .join("\n\n");
        ctx.copy_text(report);
        self.info(match problems.len() {
            1 => "Copied the problem".to_owned(),
            n => format!("Copied {n} problems"),
        });
    }

    /// F2: ask for a new name for the symbol under the caret.
    pub(super) fn begin_rename(&mut self) {
        let Some(name) = self.symbol_under_caret() else {
            self.info("Put the caret on a name first");
            return;
        };
        self.rename = Some((name.clone(), name));
    }

    /// The rename prompt.
    pub(super) fn rename_ui(&mut self, ctx: &egui::Context) {
        let Some((original, draft)) = self.rename.as_mut() else {
            return;
        };
        let original = original.clone();
        let mut go = false;
        let mut cancel = false;

        egui::Modal::new(egui::Id::new("rename_symbol")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Rename");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.weak("Rename");
                ui.monospace(&original);
                ui.weak("to:");
            });
            let field = ui.add(
                egui::TextEdit::singleline(draft)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            field.request_focus();
            ui.add_space(4.0);
            ui.small("Every use across the project is changed. One undo step per file.");

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui.button("Rename").clicked() {
                    go = true;
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });

        let new_name = self
            .rename
            .as_ref()
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        if cancel {
            self.rename = None;
            return;
        }
        if !go {
            return;
        }
        self.rename = None;

        let new_name = new_name.trim().to_owned();
        if new_name.is_empty() || new_name == original {
            return;
        }
        self.request_rename(&new_name);
    }

    pub(super) fn request_rename(&mut self, new_name: &str) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            self.info("Save the file before renaming");
            return;
        };
        // Unsaved changes are fine: the servers are sent the buffer, not the
        // file, and the rename prompt is modal, so nothing is typed between
        // the last sync and the question.
        let (line, column) = entry.doc.line_col(entry.view.selection.head);
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if !self.lsp.rename(&path, line, column, new_name) {
            self.error(
                "No language server can rename here. Renaming by find-and-replace \
                 is how the wrong things get renamed, so there is no fallback.",
            );
        }
    }

    /// Apply a rename the server worked out, across every file it touches.
    ///
    /// Open documents go through the normal transaction path, so the change is
    /// undoable and reaches the highlighter. Closed ones are rewritten on disk:
    /// loading twenty files to change one line in each, only to close them
    /// again, is the worse trade.
    pub(super) fn apply_rename(&mut self, files: Vec<editor_lsp::session::FileEdit>) {
        if files.is_empty() {
            self.info("Nothing to rename here");
            return;
        }
        let count = files.len();
        let mut failures = Vec::new();

        for file in files {
            let open = self
                .docs
                .iter()
                .position(|d| d.doc.path() == Some(file.path.as_path()));
            match open {
                Some(index) => self.apply_rename_to_open(index, &file),
                None => {
                    if let Err(e) = apply_rename_to_disk(&file) {
                        failures.push(format!("{}: {e}", file.path.display()));
                    }
                }
            }
        }

        self.tree.refresh();
        if failures.is_empty() {
            self.info(format!("Renamed across {count} file(s)"));
        } else {
            self.error(format!("Rename partly failed: {}", failures.join("; ")));
        }
    }

    pub(super) fn apply_rename_to_open(
        &mut self,
        index: usize,
        file: &editor_lsp::session::FileEdit,
    ) {
        let Some(entry) = self.docs.get_mut(index) else {
            return;
        };
        // One transaction per file, so a rename is one undo step there rather
        // than one per occurrence.
        let mut edits = Vec::new();
        for edit in &file.edits {
            let start = entry
                .doc
                .offset_at(edit.start_line as usize, edit.start_column as usize);
            let end = entry
                .doc
                .offset_at(edit.end_line as usize, edit.end_column as usize);
            edits.push(editor_core::edit::Edit::replace(
                start..end.max(start),
                edit.text.clone(),
            ));
        }
        if edits.is_empty() {
            return;
        }
        let before = entry.view.selection;
        entry.doc.break_undo_run();
        entry
            .doc
            .apply(&editor_core::edit::Transaction::new(edits), before, before);
        entry.doc.break_undo_run();
        entry.view.set_caret(before.head.min(entry.doc.len_chars()));
    }

    /// Ctrl+Shift+O: list what this file declares.
    ///
    /// From the parse tree rather than from a language server, so it works with
    /// nothing installed — the same choice as Go to Definition and Find Uses.
    pub(super) fn open_symbol_picker(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) else {
            self.info("Open a file first");
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(|h| h.tree()) else {
            self.info("No outline: this file has no grammar");
            return;
        };
        let symbols = editor_syntax::symbols::outline(tree, entry.doc.text());
        if symbols.is_empty() {
            self.info("Nothing is declared in this file");
            return;
        }
        self.symbol_picker.open(symbols);
    }

    /// The word being typed at the caret: where it starts, and what it is.
    /// The word being typed at the caret: where it starts, and what it is.
    ///
    /// `None` when the caret is not immediately after an identifier character,
    /// which is how the popup knows to close: the moment you type a space or a
    /// bracket, the word you were completing has ended.
    pub(super) fn completion_prefix(&self) -> Option<(usize, String)> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let caret = entry.view.selection.head;
        if !entry.view.selection.is_empty() {
            // With a selection, typing replaces it; there is no prefix.
            return None;
        }
        let text = entry.doc.text();

        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = caret;
        while start > 0 && is_word(text.char(start - 1)) {
            start -= 1;
        }
        if start == caret {
            // No word yet, but a `.` just typed is the single most common
            // reason to want a list: `os.` and then everything the module has.
            // The prefix is empty and the suggestion replaces nothing.
            return (caret > 0 && text.char(caret - 1) == '.').then(|| (caret, String::new()));
        }
        // A name cannot begin with a digit, so `1234` is a number being typed,
        // not a prefix worth asking a server about.
        if text.char(start).is_ascii_digit() {
            return None;
        }
        Some((start, text.slice(start..caret).chars().collect()))
    }

    /// Keep the completion popup in step with the document.
    ///
    /// Called once per frame. Asks for suggestions when a new word starts,
    /// narrows the list locally while the same word grows, and closes when the
    /// word ends.
    pub(super) fn sync_completion(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            self.completion.close();
            return;
        };
        let version = entry.doc.version();
        if self.completion_version == Some(version) {
            return;
        }
        self.completion_version = Some(version);

        let Some((start, prefix)) = self.completion_prefix() else {
            self.completion.close();
            return;
        };

        // Below this the list is most of what the server knows, which is
        // thousands of entries and no help. An empty prefix is exempt: it means
        // a `.` was just typed, where the member list is exactly what is
        // wanted and is already narrowed by the thing before the dot.
        const MIN_PREFIX: usize = 2;

        if self.completion.is_open() {
            self.completion.refilter(&prefix);
            return;
        }
        let long_enough = prefix.is_empty() || prefix.chars().count() >= MIN_PREFIX;
        if self.completion.is_waiting() || !long_enough {
            return;
        }
        self.request_completions(start, &prefix, false);
    }

    /// Ask the servers about the word starting at `start`.
    pub(super) fn request_completions(&mut self, start: usize, prefix: &str, explicit: bool) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            return;
        };
        // Asked about the *start* of the word, not the caret: a server given
        // the position after `par` may filter to what it thinks matches, and
        // its idea of matching is not the one the popup then applies. After a
        // `.` the two are the same position anyway.
        let (line, column) = entry.doc.line_col(start);
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if self.lsp.complete(&path, line, column) {
            self.completion.requested(start, prefix);
            return;
        }
        // The word-list fallback is not offered unbidden. A list of names that
        // happen to appear elsewhere in the file is a reasonable answer to
        // "suggest something", and a poor reason to put a popup over the text
        // every time two letters are typed.
        if explicit {
            self.complete_locally(start, prefix);
        }
    }

    /// Suggest names already written in this file.
    ///
    /// What is available with no language server: a word list from the parse
    /// tree, marked with what defines each name. It cannot see another file, an
    /// import, or anything in the standard library, so the detail column says
    /// where the suggestion came from rather than letting it pass for a real
    /// completion.
    ///
    /// Not offered after a `.`: the members of an object have nothing to do
    /// with the names that happen to appear elsewhere in the file, and a list
    /// of them there would be actively misleading.
    pub(super) fn complete_locally(&mut self, start: usize, prefix: &str) {
        if prefix.is_empty() {
            return;
        }
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(Highlighter::tree) else {
            return;
        };

        let caret_word = editor_syntax::symbols::identifier_at(
            tree,
            entry.doc.text(),
            entry.view.selection.head,
        )
        .map(|s| s.range);

        let items: Vec<editor_lsp::session::Completion> =
            editor_syntax::symbols::identifiers(tree, entry.doc.text(), caret_word)
                .into_iter()
                .map(|symbol| editor_lsp::session::Completion {
                    kind: Some(lsp_kind(symbol.kind)),
                    detail: Some("in this file".to_owned()),
                    // No `sortText`: the walk already put definitions first,
                    // and the popup sorts by it when present.
                    sort_text: None,
                    insert: symbol.name.clone(),
                    label: symbol.name,
                })
                .collect();

        if items.is_empty() {
            return;
        }
        self.completion.requested(start, prefix);
        self.completion.answered(items, prefix);
    }

    /// Ctrl+Space: ask now, whatever the prefix length.
    ///
    /// The two-character floor exists so the popup does not appear unbidden
    /// over a single letter. Asking for it explicitly is a different matter.
    pub(super) fn trigger_completion(&mut self) {
        let Some((start, prefix)) = self.completion_prefix() else {
            self.info("Put the caret in or after a name first");
            return;
        };
        self.completion.close();
        self.request_completions(start, &prefix, true);
        if !self.completion.is_open() && !self.completion.is_waiting() {
            self.info("No suggestions here");
        }
    }

    /// Let the popup claim its keys, and apply an acceptance.
    ///
    /// Runs near the top of the frame, before the editor reads events.
    pub(super) fn completion_keys(&mut self, ctx: &egui::Context) {
        let Some((_, prefix)) = self.completion_prefix() else {
            return;
        };
        let action = self.completion.handle_keys(ctx, prefix.chars().count());
        self.apply_completion(action);
    }

    /// Draw the popup, and apply a choice made with the mouse.
    pub(super) fn completion_draw(&mut self, ctx: &egui::Context) {
        let Some((_, prefix)) = self.completion_prefix() else {
            return;
        };
        let Some(caret) = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|e| e.view.caret_screen_rect())
        else {
            return;
        };
        let action = self.completion.draw(ctx, caret, prefix.chars().count());
        self.apply_completion(action);
    }

    /// Notice what the pointer has settled on, and find out about it.
    ///
    /// A server is asked when one can answer; the parse tree answers meanwhile
    /// and answers alone when there is no server. The degradation ladder in
    /// PLAN.md §3.6: everything that can work without a server should.
    pub(super) fn sync_hover(&mut self, ctx: &egui::Context) {
        // A popup while a menu or a dialog is open is a popup in the way. A
        // context menu most of all: the hover popup is drawn at tooltip level,
        // above menus, so a right-click on a squiggle opened the menu
        // underneath the problem it was asking about.
        if self.palette.is_open() || self.completion.is_open() || egui::Popup::is_any_open(ctx) {
            self.hover = None;
            return;
        }
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            self.hover = None;
            return;
        };
        let Some(hovered) = entry.view.hovered() else {
            self.hover = None;
            return;
        };
        // Already asked about this character; the answer may still be coming.
        if self
            .hover
            .as_ref()
            .is_some_and(|h| h.offset == hovered.offset && h.gutter == hovered.gutter)
        {
            return;
        }

        // The problems here. Over the text, only the underlined ones: a popup
        // about a warning the setting chose not to draw would be about nothing
        // the reader can see. The gutter marker stands for everything on the
        // line, so it shows everything.
        let problems = entry.doc.path().map_or_else(Vec::new, |path| {
            let all = self.lsp.diagnostics().for_file(path);
            if hovered.gutter {
                problems_on_line(&entry.doc, &all, entry.doc.line_of(hovered.offset))
            } else {
                let level = self.settings.underline_diagnostics();
                problems_at(&entry.doc, &all, hovered.offset)
                    .into_iter()
                    .filter(|d| is_underlined(d, level))
                    .collect()
            }
        });
        // The marker asks about the line, which has no type to look up.
        if hovered.gutter {
            self.hover = Some(Hover {
                offset: hovered.offset,
                at: hovered.at,
                text: String::new(),
                from_file: false,
                waiting: false,
                problems,
                gutter: true,
            });
            return;
        }

        // What the file itself can say, which is available at once and is the
        // whole answer when nothing else can be asked.
        let local = entry
            .highlighter
            .as_ref()
            .and_then(|h| h.tree())
            .and_then(|tree| {
                editor_syntax::hover::local(tree, entry.doc.text(), hovered.offset, entry.language)
            });

        let path = entry.doc.path().map(Path::to_path_buf);
        // `line_col` is one-based for the status bar; the protocol is not.
        let (line, column) = entry.doc.line_col(hovered.offset);
        let (line, column) = (line as u32 - 1, column as u32 - 1);
        let asked = match path {
            Some(path) => self.lsp.hover(&path, line, column),
            None => false,
        };

        self.hover = Some(Hover {
            offset: hovered.offset,
            at: hovered.at,
            text: local
                .as_ref()
                .map(editor_syntax::hover::Local::text)
                .unwrap_or_default(),
            from_file: local.is_some(),
            waiting: asked,
            problems,
            gutter: false,
        });
    }

    /// Draw whatever is known about what the pointer is on.
    pub(super) fn hover_draw(&mut self, ctx: &egui::Context) {
        // Checked again here because the menu a right-click opens appears in
        // the frame it is clicked in, after `sync_hover` has run.
        if egui::Popup::is_any_open(ctx) {
            self.hover = None;
            return;
        }
        let Some(hover) = self.hover.clone() else {
            return;
        };
        // Nothing to say and nothing coming: no popup at all, rather than an
        // empty one that follows the pointer around.
        if hover.text.is_empty() && !hover.waiting && hover.problems.is_empty() {
            return;
        }

        egui::Area::new(egui::Id::new("hover_popup"))
            // Below and slightly right of the pointer, which is where a tooltip
            // goes and where it does not cover the word being asked about.
            .fixed_pos(hover.at + egui::vec2(12.0, 20.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(560.0);
                    for (i, problem) in hover.problems.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(6.0);
                        }
                        problem_ui(ui, problem);
                    }
                    if hover.text.is_empty() && !hover.waiting {
                        return;
                    }
                    if !hover.problems.is_empty() {
                        ui.separator();
                    }
                    if hover.text.is_empty() {
                        ui.weak("Looking\u{2026}");
                        return;
                    }
                    // Monospace: a signature is code, and proportional spacing
                    // makes `a: i32, b: i32` hard to read as a list.
                    ui.label(egui::RichText::new(&hover.text).monospace());
                    if hover.from_file {
                        // Say where this came from. "Declared on this line" and
                        // "this is its type" are different claims, and only one
                        // of them is being made.
                        ui.add_space(4.0);
                        ui.weak("From this file only \u{2014} no language server answered.");
                    }
                });
            });
    }

    /// Put an accepted suggestion into the document.
    pub(super) fn apply_completion(&mut self, action: completion::Action) {
        let completion::Action::Accept { insert, replacing } = action else {
            return;
        };
        let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) else {
            return;
        };
        let caret_offset = entry.view.selection.head;
        let start = caret_offset.saturating_sub(replacing);

        // One transaction, so accepting a suggestion is one undo step rather
        // than a delete and an insert. The run is broken either side so it does
        // not coalesce with the typing that led up to it.
        use editor_core::edit::{Edit, Transaction};
        let before = entry.view.selection;
        let after = editor_core::selection::Selection::at(start + insert.chars().count());
        entry.doc.break_undo_run();
        entry.doc.apply(
            &Transaction::new(vec![Edit::replace(start..caret_offset, insert)]),
            before,
            after,
        );
        entry.view.set_caret(after.head);
        entry.doc.break_undo_run();
        entry.view.focus();

        // The edit bumps the version. Recording it here stops the next frame
        // treating the freshly inserted word as a new prefix and asking again.
        self.completion_version = self
            .active
            .and_then(|i| self.docs.get(i))
            .map(|e| e.doc.version());
    }

    /// The name the caret is on, for messages about a server's answer.
    pub(super) fn symbol_under_caret(&self) -> Option<String> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let tree = entry.highlighter.as_ref().and_then(Highlighter::tree)?;
        editor_syntax::symbols::identifier_at(tree, entry.doc.text(), entry.view.selection.head)
            .map(|s| s.name)
    }

    /// Go to Definition / Find Uses, from the menu, F12 or the context menu.
    ///
    /// Asks the language server first and falls back to a parse-tree search of
    /// the open file. The fallback is genuinely weaker — one file, no scope, no
    /// imports — so it says so rather than letting a partial answer pass for a
    /// complete one.
    pub(super) fn ask_about_symbol(&mut self, query: editor_lsp::session::Query) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let caret = entry.view.selection.head;
        let (line, column) = entry.doc.line_col(caret);
        // `line_col` is one-based for the status bar; the protocol is not.
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if let Some(path) = entry.doc.path().map(Path::to_path_buf)
            && self.lsp.ask(query, &path, line, column)
        {
            // The answer arrives through `poll`, possibly several frames later.
            self.uses.pending = Some(query);
            return;
        }

        self.answer_locally(query, caret);
    }

    /// The no-language-server path: search the open file's parse tree.
    pub(super) fn answer_locally(&mut self, query: editor_lsp::session::Query, caret: usize) {
        use editor_lsp::session::Query;
        use editor_syntax::symbols;

        let Some(index) = self.active else { return };
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(Highlighter::tree) else {
            self.info("This file has no grammar, so there is nothing to search");
            return;
        };
        let text = entry.doc.text();

        let Some(symbol) = symbols::identifier_at(tree, text, caret) else {
            self.info("Put the caret on a name first");
            return;
        };

        let ranges: Vec<std::ops::Range<usize>> = match query {
            Query::Definition => symbols::definitions(tree, text, &symbol.name),
            Query::References => symbols::occurrences(tree, text, &symbol.name)
                .into_iter()
                .map(|o| o.range)
                .collect(),
        };

        if ranges.is_empty() {
            // Nothing in this file. For a definition that is the normal case —
            // the function is in another module — so the project is searched
            // before giving up. Uses are left at this file: a name used in
            // fifty places across a project is a list nobody wants from a
            // search that cannot tell one `parse` from another.
            if query == Query::Definition {
                let language = entry.language;
                let name = symbol.name.clone();
                if self.find_definition_in_project(&name, language) {
                    return;
                }
            }
            self.info(format!(
                "No {} of `{}` found (no language server, so this is a search of the \
                 project's text rather than an answer about the code)",
                query.noun(),
                symbol.name
            ));
            return;
        }

        let path = entry.doc.path().map(Path::to_path_buf);
        let locations: Vec<Target> = ranges
            .iter()
            .map(|range| {
                let (line, column) = entry.doc.line_col(range.start);
                Target {
                    path: path.clone(),
                    line: line - 1,
                    column: column - 1,
                }
            })
            .collect();

        self.land_on(query, symbol.name, locations, true);
    }

    /// Search the open project for a definition of `name`.
    ///
    /// The fallback for Go to Definition when no server can answer. Only files
    /// of the same language are considered, and each is skimmed for the name as
    /// plain text before being parsed — reading a file is cheap, parsing it is
    /// not, and in any real project almost every file fails the skim.
    ///
    /// Returns true if it jumped somewhere.
    pub(super) fn find_definition_in_project(&mut self, name: &str, language: LanguageId) -> bool {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            return false;
        };

        let mut found = Vec::new();
        for relative in editor_search::files::list(&root).files {
            if found.len() >= MAX_PROJECT_DEFINITIONS {
                break;
            }
            let path = root.join(&relative);
            let same_language = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| LanguageId::from_extension(e) == language);
            if !same_language {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            // The skim. A definition of `parse` cannot exist in a file that does
            // not contain the word at all.
            if source.len() > MAX_SEARCHED_BYTES || !source.contains(name) {
                continue;
            }

            let rope = ropey::Rope::from_str(&source);
            let Some(highlighter) = Highlighter::new(language, &rope) else {
                continue;
            };
            let Some(tree) = highlighter.tree() else {
                continue;
            };
            for range in editor_syntax::symbols::definitions(tree, &rope, name) {
                let line = rope.char_to_line(range.start);
                let column = range.start - rope.line_to_char(line);
                found.push(Target {
                    path: Some(path.clone()),
                    line,
                    column,
                });
            }
        }

        if found.is_empty() {
            return false;
        }
        let count = found.len();
        self.go_to(&found[0]);
        if count > 1 {
            self.info(format!("{count} definitions of `{name}` in this project"));
        }
        true
    }

    /// Take a set of results and go to the first one.
    pub(super) fn land_on(
        &mut self,
        query: editor_lsp::session::Query,
        name: String,
        locations: Vec<Target>,
        local_only: bool,
    ) {
        use editor_lsp::session::Query;

        self.uses.pending = None;
        if locations.is_empty() {
            self.info(format!("No {} found", query.noun()));
            return;
        }
        let first = locations[0].clone();

        if query == Query::Definition {
            // A definition is a jump, not a list: keeping the previous Find
            // Uses results means F8 still walks what the user was walking.
            self.go_to(&first);
            if locations.len() > 1 {
                self.info(format!("{} definitions of `{name}`", locations.len()));
            }
            return;
        }

        // Servers answer in whatever order suits their index -- pyright does
        // not return references in document order -- so F8 would otherwise jump
        // about the file rather than walking down it.
        let mut locations = locations;
        locations.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.line.cmp(&b.line))
                .then(a.column.cmp(&b.column))
        });
        locations.dedup();

        // Start from the result the caret is already on, if it is one of them,
        // so the first F8 moves to the *next* use rather than back to the top.
        let start = self.current_location_index(&locations).unwrap_or(0);
        let first = locations[start].clone();

        let count = locations.len();
        self.uses = UseResults {
            name,
            locations,
            index: start,
            local_only,
            pending: None,
        };
        self.go_to(&first);
        self.info(format!(
            "{} of {count}{}  \u{2014}  F8 next, Shift+F8 previous",
            start + 1,
            if local_only { " in this file" } else { "" }
        ));
    }

    /// Which result the caret is already sitting on, if any.
    ///
    /// Find Uses is normally run with the caret on one of its own results, so
    /// starting at index 0 would mean the first F8 jumped back to the top of
    /// the file before going anywhere useful.
    pub(super) fn current_location_index(&self, locations: &[Target]) -> Option<usize> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let here = entry.doc.path();
        let (line, column) = entry.doc.line_col(entry.view.selection.head);
        let (line, column) = (line - 1, column - 1);

        locations.iter().position(|target| {
            let same_file = match (&target.path, here) {
                (Some(path), Some(current)) => path == current,
                (None, _) => true,
                _ => false,
            };
            // Column is not compared: the caret can be anywhere within the
            // name, and the result points at its first character.
            same_file && target.line == line && target.column <= column
        })
    }

    /// Step through the Find Uses results.
    pub(super) fn step_use(&mut self, direction: isize) {
        let count = self.uses.locations.len();
        if count == 0 {
            self.info("Nothing to step through \u{2014} run Find Uses first");
            return;
        }
        // Wraps, like Find Next, so walking off the end returns to the start
        // rather than stopping with no explanation.
        let next = (self.uses.index as isize + direction).rem_euclid(count as isize) as usize;
        self.uses.index = next;
        let target = self.uses.locations[next].clone();
        self.go_to(&target);
        self.info(format!(
            "{} of {count} \u{2014} `{}`{}",
            next + 1,
            self.uses.name,
            // Repeated on every step, not just the first: after three F8s the
            // opening message is long gone and the list still is not complete.
            if self.uses.local_only {
                " in this file"
            } else {
                ""
            }
        ));
    }

    /// Move the caret to a result, opening its file if it is not already open.
    pub(super) fn go_to(&mut self, target: &Target) {
        match &target.path {
            Some(path) => self.open_at(path, target.line, target.column),
            // A result in the file already showing, which may be untitled.
            None => {
                if let Some(entry) = self.active_mut() {
                    let offset = entry.doc.offset_at(target.line, target.column);
                    entry.view.set_caret(offset);
                    entry.view.focus();
                }
            }
        }
    }

    /// Keep the language servers' view of the open documents current, and
    /// drain whatever they have said.
    ///
    /// Driven from the documents' own version counters rather than from the
    /// edit path, so no future way of changing text can forget to tell them —
    /// the same reason the highlighter is driven from the change outbox.
    pub(super) fn sync_language_servers(&mut self) {
        let extra_path = self.tool_search_path();
        let mut disabled = self.settings.disabled_servers();
        if !self.folder_trusted() {
            // It builds the project's build scripts and macros to understand
            // it, which is running the project's code.
            disabled.push(editor_lsp::registry::RUST_ANALYZER.id.to_owned());
        }
        self.lsp.set_disabled(disabled);
        self.lsp
            .set_type_checking(self.settings.type_checking().as_str());
        self.lsp
            .set_root(self.tree.root().map(Path::to_path_buf), extra_path);

        // Tell the servers about anything new or changed. The session keeps
        // the only record of what they have been told, so a restart or a new
        // project root there is noticed here without anyone saying so.
        for entry in &self.docs {
            if let Some(path) = entry.doc.path() {
                self.lsp
                    .sync(path, entry.doc.version(), || entry.doc.text().to_string());
            }
        }

        // ...and about anything closed.
        let open: std::collections::HashSet<&Path> =
            self.docs.iter().filter_map(|d| d.doc.path()).collect();
        self.lsp.retain(|path| open.contains(path));

        for notice in self.lsp.poll() {
            match notice {
                editor_lsp::session::Notice::ServerReady(id) => {
                    tracing::info!(server = id, "ready");
                }
                editor_lsp::session::Notice::ServerDied {
                    name, restarting, ..
                } => {
                    if restarting {
                        self.info(format!("{name} stopped unexpectedly; restarting"));
                    } else {
                        self.error(format!("{name} keeps failing; giving up on it"));
                    }
                }
                editor_lsp::session::Notice::DiagnosticsChanged(_) => {}
                editor_lsp::session::Notice::Rename(files) => self.apply_rename(files),
                editor_lsp::session::Notice::Hovered { line, column, text } => {
                    // Only if it is still about where the pointer is. The
                    // pointer moves while the request is in flight, and a
                    // description of somewhere it has left is worse than none.
                    if let Some(hover) = self.hover.as_mut()
                        && !hover.gutter
                        && let Some(entry) = self.active.and_then(|i| self.docs.get(i))
                    {
                        let (at_line, at_column) = entry.doc.line_col(hover.offset);
                        if at_line as u32 - 1 == line && at_column as u32 - 1 == column {
                            hover.waiting = false;
                            if !text.trim().is_empty() {
                                // A server's answer replaces the file's guess:
                                // it knows the type, and the other knows a line.
                                hover.text = text;
                                hover.from_file = false;
                            }
                        }
                    }
                }
                editor_lsp::session::Notice::Completions(items) => {
                    // Matched against the word as it is *now*, not as it was
                    // when the request went out; the popup decides whether the
                    // reply still describes the word being typed.
                    let prefix = self.completion_prefix().map(|(_, p)| p).unwrap_or_default();
                    self.completion.answered(items, &prefix);
                }
                editor_lsp::session::Notice::Answered { query, locations } => {
                    // A stale answer to a question the user has moved on from
                    // would yank the caret somewhere unexpected.
                    if self.uses.pending != Some(query) {
                        continue;
                    }
                    if locations.is_empty() {
                        // The server knows the project and still found nothing,
                        // so falling back to a text-shaped search of one file
                        // would only produce a worse answer to the same
                        // question.
                        self.uses.pending = None;
                        self.info(format!("No {} found", query.noun()));
                        continue;
                    }
                    let name = self.symbol_under_caret().unwrap_or_default();
                    let targets = locations
                        .into_iter()
                        .map(|l| Target {
                            path: Some(l.path),
                            line: l.line as usize,
                            column: l.column as usize,
                        })
                        .collect();
                    self.land_on(query, name, targets, false);
                }
            }
        }
    }

    /// Feed every document's edits to its parse tree, and re-check its syntax.
    ///
    /// Two jobs in one pass because they need the same thing: a parse tree that
    /// matches the text. Draining the change outbox here rather than at each
    /// edit site means no future way of changing text can forget to do it —
    /// typing, paste, undo, redo and replace-all all take this route.
    ///
    /// The syntax check is what makes broken code visible with no language
    /// server installed. It reads the tree the highlighter already maintains,
    /// so it costs a tree walk that returns immediately when the root has no
    /// error flag — which is every keystroke in a file that parses.
    pub(super) fn sync_highlighters(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut next_due: Option<Instant> = None;
        let mut breakpoints_changed: Vec<PathBuf> = Vec::new();

        for entry in &mut self.docs {
            let changes = entry.doc.take_changes();
            // Drained for every document, so a buffer with no breakpoints
            // does not collect them for ever.
            let shifts = entry.doc.take_line_shifts();
            if let Some(h) = entry.highlighter.as_mut() {
                if !changes.is_empty() {
                    h.update(&changes, entry.doc.text());
                }
                // A reparse that ran out of time leaves the tree a keystroke
                // behind. Finish it as soon as nothing is being typed, on the
                // same debounce the syntax check uses — the colours are the
                // stale thing here, and they are worth a moment of lag to keep
                // the keystroke itself instant.
                if h.is_stale() {
                    let due = *entry.syntax_due.get_or_insert(now + SYNTAX_DEBOUNCE);
                    if now >= due {
                        h.catch_up(entry.doc.text());
                    } else {
                        next_due = Some(next_due.map_or(due, |soonest: Instant| soonest.min(due)));
                    }
                }
            }

            let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
                // A never-saved buffer has no key in the diagnostic store, the
                // same reason the language servers cannot see it.
                continue;
            };

            // Carry breakpoints with the lines they were put on.
            if !shifts.is_empty() && self.breakpoints.follow(&path, &shifts) {
                breakpoints_changed.push(path.clone());
            }
            let version = entry.doc.version();
            if entry.syntax_version == Some(version) {
                continue;
            }

            // Wait for a pause in typing. Half a line of Python is not valid
            // Python, so checking on every keystroke would put a squiggle under
            // the caret for as long as the user is writing.
            let due = *entry.syntax_due.get_or_insert(now + SYNTAX_DEBOUNCE);
            if now < due {
                next_due = Some(next_due.map_or(due, |soonest: Instant| soonest.min(due)));
                continue;
            }

            let found = entry
                .highlighter
                .as_ref()
                .map(|h| h.errors(entry.doc.text()))
                .unwrap_or_default();
            self.lsp
                .set_builtin(&path, found.into_iter().map(to_diagnostic).collect());
            entry.syntax_version = Some(version);
            entry.syntax_due = None;
        }

        for path in breakpoints_changed {
            self.send_breakpoints(&path);
        }

        // Nothing else will wake the frame loop once typing stops, so the
        // pending check has to ask for the frame it needs.
        if let Some(due) = next_due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
        }
    }

    /// Directories searched before `PATH` when looking for language servers.
    ///
    /// A project virtual environment's tools come first, so a project with
    /// `ruff` pinned in its venv is linted by that version rather than by
    /// whatever happens to be installed globally.
    pub(super) fn tool_search_path(&mut self) -> Vec<PathBuf> {
        self.environment
            .tool_search_path(&self.settings.python_interpreter(), self.tree.root())
    }

    /// Diagnostics for a document, converted to character offsets.
    ///
    /// The store holds zero-based lines and *character* columns — the language
    /// session has already converted from whatever the server counts in (see
    /// `editor_lsp::position`) — so this only turns them into offsets.
    ///
    /// Every diagnostic is passed on, because the gutter marks them all; the
    /// setting only decides which are also written across the text.
    pub(super) fn underlines_for(
        entry: &OpenDoc,
        store: &editor_lsp::diagnostics::Store,
        level: editor_config::settings::UnderlineDiagnostics,
    ) -> Vec<Underline> {
        let Some(path) = entry.doc.path() else {
            return Vec::new();
        };
        store
            .for_file(path)
            .into_iter()
            .map(|d| Underline {
                range: diagnostic_range(&entry.doc, &d),
                severity: d.severity,
                underlined: is_underlined(&d, level),
                message: d.summary(),
            })
            .collect()
    }

    /// What is actually checking the code right now, for the status bar hover.
    ///
    /// "No problems" from a syntax check alone means something much weaker than
    /// "no problems" from a syntax check plus a type checker, and the user is
    /// entitled to know which one they are looking at.
    pub(super) fn checker_summary(&self) -> String {
        let running = self.lsp.running();
        if running.is_empty() {
            "Checking syntax only \u{2014} no language server is running.\n\
             Help > Check Toolchains lists what could be installed."
                .to_owned()
        } else {
            format!("Checking syntax, plus: {}", running.join(", "))
        }
    }
}

/// The rest of a Problems row after its text, as somewhere to start a text
/// selection.
///
/// Pressing on a problem goes to it, so a selection begun there would jump the
/// editor away before anything could be copied. Pressing here instead starts
/// a selection that can be dragged across the rows above and below, as from
/// the end of a line in a text editor, and copied with Ctrl+C. It takes part
/// in egui's label selection as an empty label the width of the space left.
fn selection_margin(ui: &mut egui::Ui) {
    let width = ui.available_width();
    if !width.is_finite() || width <= 0.0 {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Sense::click_and_drag(),
    );
    let colour = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(String::new(), egui::FontId::default(), colour);
    egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        rect.left_top(),
        galley,
        colour,
        egui::Stroke::NONE,
    );
}

/// One diagnostic in the hover popup: what kind, which rule, who said so, and
/// the whole message.
///
/// The rule and the server are what make a complaint checkable — they are what
/// to search for, and what to switch off if the rule is not wanted here.
fn problem_ui(ui: &mut egui::Ui, problem: &editor_lsp::diagnostics::Diagnostic) {
    ui.horizontal(|ui| {
        ui.colored_label(
            severity_colour(ui.visuals(), problem.severity),
            problem.severity.glyph(),
        );
        ui.strong(problem.severity.label());
        if let Some(code) = &problem.code {
            ui.monospace(code);
        }
        ui.weak(&problem.source);
    });
    ui.label(&problem.message);
}
