//! The code editor widget.
//!
//! Custom, because no Rust toolkit ships a usable code editor. Built on the
//! technique the spike validated: allocate the full document height, then lay
//! out and paint only the rows inside the clip rectangle, so cost tracks the
//! viewport rather than the file size.
//!
//! Caret and click positions go through egui galley cursor mapping rather than
//! multiplying by a character width. That is slower per line, but it is correct
//! for proportional fallback glyphs, CJK, and combining marks — the character-
//! width shortcut the spike used breaks on the first accented identifier.
//!
//! Highlighting is asked for one viewport at a time: the paint pass requests
//! spans for exactly the rows it is about to draw, so a 50,000-line file costs
//! the same per frame as a 50-line one.
//!
//! Not here yet: find/replace (M5), multi-cursor, code folding, and word-wise
//! motion.

use editor_core::document::Document;
use editor_core::edit::Transaction;
use editor_core::selection::Selection;
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::indent::{self, IndentOptions};
use editor_syntax::theme::SyntaxTheme;
use eframe::egui;

/// Rows painted above and below the viewport, so a fast scroll never exposes a
/// blank band before the next frame lands.
const OVERSCAN_ROWS: usize = 4;
/// Caret blink period.
const BLINK_MS: u128 = 530;

/// Appearance and behaviour knobs, supplied from settings.
#[derive(Debug, Clone, Copy)]
pub struct EditorOptions {
    pub font_size: f32,
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub show_line_numbers: bool,
    /// Drives the indentation, comment and bracket rules.
    pub language: LanguageId,
    pub auto_close_brackets: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            tab_width: 4,
            insert_spaces: true,
            show_line_numbers: true,
            language: LanguageId::PlainText,
            auto_close_brackets: true,
        }
    }
}

impl EditorOptions {
    fn indent(self) -> IndentOptions {
        IndentOptions {
            tab_width: self.tab_width,
            insert_spaces: self.insert_spaces,
        }
    }
}

/// Per-view state: where the cursor is, and what the view is doing.
///
/// Separate from [`Document`] because one document may eventually be open in
/// several views (split panes), each with its own cursor.
#[derive(Debug, Default)]
pub struct EditorView {
    pub selection: Selection,
    /// Column the caret is "trying" to be in while moving vertically, so that
    /// crossing a short line and coming back returns to the original column
    /// instead of clinging to the short line's end.
    goal_column: Option<usize>,
    /// Set when the selection changes, to scroll the caret into view once.
    scroll_to_caret: bool,
    /// Set when the app wants this view to take the keyboard.
    grab_focus: bool,
    last_interaction: Option<std::time::Instant>,
    /// Search results to highlight, supplied by the find bar.
    search_matches: Vec<std::ops::Range<usize>>,
    current_match: Option<std::ops::Range<usize>>,
}

impl EditorView {
    /// One-based line and column of the caret, for the status bar.
    #[must_use]
    pub fn cursor_position(&self, doc: &Document) -> (usize, usize) {
        doc.line_col(self.selection.head)
    }

    /// Characters currently selected, for the status bar.
    #[must_use]
    pub fn selection_len(&self) -> usize {
        self.selection.len()
    }

    /// Put the caret at `offset`, collapsing any selection, and scroll it into
    /// view. Used to honour a template's `$CURSOR` marker.
    pub fn set_caret(&mut self, offset: usize) {
        self.selection = Selection::at(offset);
        self.goal_column = None;
        self.scroll_to_caret = true;
    }

    /// Select `start..end` and scroll it into view, without taking focus —
    /// stepping through search results must not pull the keyboard out of the
    /// find field mid-search.
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.selection = Selection::new(start, end);
        self.goal_column = None;
        self.scroll_to_caret = true;
    }

    /// Take the keyboard on the next frame.
    ///
    /// Called when a document is opened or a tab is selected, so that typing
    /// works immediately instead of requiring a click into the text first.
    pub fn focus(&mut self) {
        self.grab_focus = true;
    }

    // ---- application commands --------------------------------------------
    //
    // Driven from the command registry rather than from key handling here, so
    // that the menu item and the shortcut cannot diverge.

    /// Undo one step. Returns true if anything changed.
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        match doc.undo() {
            Some(sel) => {
                self.selection = sel;
                self.scroll_to_caret = true;
                self.touch();
                true
            }
            None => false,
        }
    }

    /// Redo one step. Returns true if anything changed.
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        match doc.redo() {
            Some(sel) => {
                self.selection = sel;
                self.scroll_to_caret = true;
                self.touch();
                true
            }
            None => false,
        }
    }

    pub fn select_all(&mut self, doc: &Document) {
        self.selection = Selection::new(0, doc.len_chars());
        self.goal_column = None;
        self.touch();
    }

    /// The selected text, or `None` when nothing is selected.
    #[must_use]
    pub fn copy(&self, doc: &Document) -> Option<String> {
        self.selected_text(doc)
    }

    /// Remove and return the selection.
    pub fn cut(&mut self, doc: &mut Document) -> Option<String> {
        let text = self.selected_text(doc)?;
        doc.break_undo_run();
        self.delete_selection(doc);
        doc.break_undo_run();
        Some(text)
    }

    /// Insert `text` at the caret, replacing any selection.
    pub fn paste(&mut self, doc: &mut Document, text: &str) -> bool {
        doc.break_undo_run();
        let changed = self.insert(doc, text);
        doc.break_undo_run();
        changed
    }

    /// Draw the editor and process input. Returns true if the document changed.
    ///
    /// `highlighter` is the document's own parse state; `None` means the
    /// language has no grammar and the text is painted in one colour.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        highlighter: Option<&mut Highlighter>,
        syntax: &SyntaxTheme,
        opts: EditorOptions,
    ) -> bool {
        self.render(ui, doc, highlighter, syntax, opts)
    }

    /// Ranges to highlight as search results, and which of them is current.
    ///
    /// Set once per frame before drawing; cleared when the find bar closes.
    pub fn set_search_matches(
        &mut self,
        matches: &[std::ops::Range<usize>],
        current: Option<std::ops::Range<usize>>,
    ) {
        self.search_matches.clear();
        self.search_matches.extend_from_slice(matches);
        self.current_match = current;
    }

    fn render(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        mut highlighter: Option<&mut Highlighter>,
        syntax: &SyntaxTheme,
        opts: EditorOptions,
    ) -> bool {
        let font = egui::FontId::monospace(opts.font_size);
        let row_height = ui.fonts_mut(|f| f.row_height(&font));
        let space_width = ui.fonts_mut(|f| f.glyph_width(&font, ' '));
        let line_count = doc.text().len_lines();

        let gutter_width = if opts.show_line_numbers {
            space_width * (line_count.to_string().len() as f32 + 2.0) + 12.0
        } else {
            6.0
        };

        let mut changed = false;

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Claim the whole document; the scroll area decides what of it
                // is on screen.
                let widest = 120.0 * space_width;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(
                        (gutter_width + widest).max(ui.available_width()),
                        row_height * line_count as f32,
                    ),
                    egui::Sense::click_and_drag(),
                );

                if std::mem::take(&mut self.grab_focus) {
                    response.request_focus();
                }
                // The code pane *is* text, so here the I-beam is correct.
                let response = response.on_hover_cursor(egui::CursorIcon::Text);

                let text_left = rect.left() + gutter_width;
                let visible = ui.clip_rect().intersect(rect);
                let rows_per_page = (visible.height() / row_height).floor().max(1.0) as usize;

                changed |=
                    self.handle_mouse(ui, doc, &response, &font, rect, text_left, row_height);
                if response.has_focus() {
                    changed |= self.handle_keys(ui, doc, opts, rows_per_page);
                }

                self.paint(
                    ui,
                    doc,
                    highlighter.take(),
                    syntax,
                    opts,
                    &font,
                    rect,
                    text_left,
                    gutter_width,
                    row_height,
                    visible,
                    &response,
                );
            });

        changed
    }

    // ---- input -----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn handle_mouse(
        &mut self,
        ui: &egui::Ui,
        doc: &Document,
        response: &egui::Response,
        font: &egui::FontId,
        rect: egui::Rect,
        text_left: f32,
        row_height: f32,
    ) -> bool {
        if !(response.clicked() || response.dragged() || response.double_clicked()) {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };

        response.request_focus();
        let offset = self.offset_at_pos(ui, doc, font, pos, rect, text_left, row_height);

        if response.double_clicked() {
            self.selection = word_at(doc, offset);
        } else if response.dragged() || ui.input(|i| i.modifiers.shift) {
            // Dragging or shift-clicking extends from the existing anchor.
            self.selection = self.selection.extended_to(offset);
        } else {
            self.selection = Selection::at(offset);
        }

        self.goal_column = None;
        self.touch();
        false
    }

    /// Map a screen position to a character offset.
    #[allow(clippy::too_many_arguments)]
    fn offset_at_pos(
        &self,
        ui: &egui::Ui,
        doc: &Document,
        font: &egui::FontId,
        pos: egui::Pos2,
        rect: egui::Rect,
        text_left: f32,
        row_height: f32,
    ) -> usize {
        let line_count = doc.text().len_lines();
        let line = (((pos.y - rect.top()) / row_height).floor().max(0.0) as usize)
            .min(line_count.saturating_sub(1));

        // Lay the line out and ask the galley, rather than assuming every
        // glyph is one character wide.
        let galley = ui.painter().layout_no_wrap(
            doc.line_text(line),
            font.clone(),
            ui.visuals().text_color(),
        );
        let local = egui::vec2((pos.x - text_left).max(0.0), 0.0);
        let column = galley.cursor_from_pos(local).index.0;

        doc.offset_at(line, column)
    }

    fn handle_keys(
        &mut self,
        ui: &egui::Ui,
        doc: &mut Document,
        opts: EditorOptions,
        rows_per_page: usize,
    ) -> bool {
        let events = ui.input(|i| i.events.clone());
        let mut changed = false;

        for event in events {
            match event {
                egui::Event::Text(text) => {
                    changed |= self.type_text(doc, opts, &text);
                }
                egui::Event::Paste(text) => {
                    doc.break_undo_run();
                    changed |= self.insert(doc, &text);
                    doc.break_undo_run();
                }
                egui::Event::Copy => {
                    if let Some(text) = self.selected_text(doc) {
                        ui.ctx().copy_text(text);
                    }
                }
                egui::Event::Cut => {
                    if let Some(text) = self.selected_text(doc) {
                        ui.ctx().copy_text(text);
                        doc.break_undo_run();
                        changed |= self.delete_selection(doc);
                        doc.break_undo_run();
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    changed |= self.handle_key(doc, opts, key, modifiers, rows_per_page);
                }
                _ => {}
            }
        }

        changed
    }

    fn handle_key(
        &mut self,
        doc: &mut Document,
        opts: EditorOptions,
        key: egui::Key,
        modifiers: egui::Modifiers,
        rows_per_page: usize,
    ) -> bool {
        use egui::Key;

        let extend = modifiers.shift;
        self.touch();

        match key {
            Key::Enter => {
                doc.break_undo_run();
                let indent = indent::new_line_indent(
                    doc.text(),
                    self.selection.range().start,
                    opts.language,
                    opts.indent(),
                );

                // Pressing Enter between a freshly auto-closed pair opens the
                // block out, rather than leaving the closer stranded:
                //
                //     fn f() {|}   ->   fn f() {
                //                           |
                //                       }
                let between_pair = self.selection.is_empty()
                    && char_before(doc, self.selection.head).is_some_and(|before| {
                        indent::auto_close(opts.language, before)
                            .is_some_and(|closer| char_at(doc, self.selection.head) == Some(closer))
                    });

                let changed = if between_pair {
                    let inner = format!("{indent}{}", opts.indent().one_level());
                    let caret = self.selection.range().start + 1 + inner.chars().count();
                    let changed = self.insert(doc, &format!("\n{inner}\n{indent}"));
                    self.selection = Selection::at(caret);
                    changed
                } else {
                    self.insert(doc, &format!("\n{indent}"))
                };

                doc.break_undo_run();
                changed
            }
            Key::Tab if !extend => {
                // With a selection spanning lines, Tab indents the block. The
                // alternative — replacing the selection with a tab character —
                // silently deletes whatever was selected.
                if self.spans_multiple_lines(doc) {
                    return self.shift_lines(doc, opts, 1);
                }
                let text = if opts.insert_spaces {
                    let col = doc.line_col(self.selection.head).1 - 1;
                    " ".repeat(opts.tab_width - (col % opts.tab_width))
                } else {
                    "\t".to_owned()
                };
                self.insert(doc, &text)
            }
            Key::Tab if extend => self.shift_lines(doc, opts, -1),
            Key::Backspace => {
                if self.selection.is_empty() {
                    if self.selection.head == 0 {
                        return false;
                    }
                    // Smart backspace: inside leading whitespace, delete back
                    // to the previous tab stop rather than one space at a time.
                    let back = self.backspace_width(doc, opts);
                    let head = self.selection.head;
                    self.selection = Selection::new(head - back, head);
                }
                self.delete_selection(doc)
            }
            Key::Delete => {
                if self.selection.is_empty() {
                    let head = self.selection.head;
                    if head >= doc.len_chars() {
                        return false;
                    }
                    self.selection = Selection::new(head, head + 1);
                }
                self.delete_selection(doc)
            }
            // Undo, redo and select-all are application commands, dispatched
            // through the registry so the menus and the keyboard agree. They
            // are deliberately not handled here.
            Key::ArrowLeft => {
                self.move_horizontal(doc, -1, extend);
                false
            }
            Key::ArrowRight => {
                self.move_horizontal(doc, 1, extend);
                false
            }
            Key::ArrowUp => {
                self.move_vertical(doc, -1, extend);
                false
            }
            Key::ArrowDown => {
                self.move_vertical(doc, 1, extend);
                false
            }
            Key::PageUp => {
                self.move_vertical(doc, -(rows_per_page as isize), extend);
                false
            }
            Key::PageDown => {
                self.move_vertical(doc, rows_per_page as isize, extend);
                false
            }
            Key::Home if modifiers.command => {
                self.set_head(0, extend);
                false
            }
            Key::End if modifiers.command => {
                self.set_head(doc.len_chars(), extend);
                false
            }
            Key::Home => {
                // Toggle between the first non-whitespace character and column
                // zero — pressing Home twice on an indented line reaches the
                // margin, which is what every editor does.
                let line = doc.line_of(self.selection.head);
                let start = doc.line_start(line);
                let indent = doc
                    .line_text(line)
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();
                let target = if self.selection.head == start + indent {
                    start
                } else {
                    start + indent
                };
                self.set_head(target, extend);
                false
            }
            Key::End => {
                let line = doc.line_of(self.selection.head);
                self.set_head(doc.line_start(line) + doc.line_len(line), extend);
                false
            }
            _ => false,
        }
    }

    // ---- editing helpers -------------------------------------------------

    /// Handle typed text, applying the bracket, quote and dedent rules.
    fn type_text(&mut self, doc: &mut Document, opts: EditorOptions, text: &str) -> bool {
        // Only single characters get special treatment. Anything longer is a
        // paste or an IME commit and must go in verbatim.
        let single = (text.chars().count() == 1)
            .then(|| text.chars().next())
            .flatten();
        let Some(c) = single.filter(|_| opts.auto_close_brackets) else {
            return self.insert(doc, text);
        };

        // Surround: typing an opener with text selected wraps it instead of
        // replacing it. Losing a selection to a stray bracket is infuriating.
        if let Some(closer) = indent::auto_close(opts.language, c)
            && !self.selection.is_empty()
        {
            let range = self.selection.range();
            let before = self.selection;
            doc.break_undo_run();
            doc.apply(
                &editor_core::edit::Transaction::new(vec![
                    editor_core::edit::Edit::insert(range.start, c.to_string()),
                    editor_core::edit::Edit::insert(range.end, closer.to_string()),
                ]),
                before,
                Selection::new(range.start + 1, range.end + 1),
            );
            self.selection = Selection::new(range.start + 1, range.end + 1);
            doc.break_undo_run();
            self.scroll_to_caret = true;
            return true;
        }

        // Type-over: typing the closer that is already sitting under the caret
        // steps past it rather than doubling it.
        if indent::is_closing(c)
            && self.selection.is_empty()
            && char_at(doc, self.selection.head) == Some(c)
        {
            self.set_head(self.selection.head + 1, false);
            return false;
        }

        if let Some(closer) = indent::auto_close(opts.language, c)
            && should_auto_close(doc, self.selection.head, c)
        {
            let changed = self.insert(doc, &format!("{c}{closer}"));
            // Leave the caret between the pair.
            self.selection = Selection::at(self.selection.head.saturating_sub(1));
            return changed;
        }

        let changed = self.insert(doc, text);

        // Re-align `else`, `except` and friends as soon as the word is
        // complete, and a closing brace as soon as it is typed.
        let line = doc.line_of(self.selection.head);
        if let Some(target) =
            indent::dedent_after_typing(doc.text(), line, opts.language, opts.indent())
        {
            self.reindent_line(doc, opts, line, target);
        }
        changed
    }

    /// Replace a line's leading whitespace with `width` columns, keeping the
    /// caret in the same place relative to the text.
    fn reindent_line(
        &mut self,
        doc: &mut Document,
        opts: EditorOptions,
        line: usize,
        width: usize,
    ) {
        let text = doc.line_text(line);
        let existing: String = text
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let replacement = opts.indent().columns(width);
        if replacement == existing {
            return;
        }
        let start = doc.line_start(line);
        let existing_len = existing.chars().count();
        let delta = replacement.chars().count() as isize - existing_len as isize;

        let before = self.selection;
        let after = Selection::at(before.head.saturating_add_signed(delta));
        doc.apply(
            &editor_core::edit::Transaction::replace(start..start + existing_len, replacement),
            before,
            after,
        );
        self.selection = after.clamped(doc.len_chars());
    }

    fn insert(&mut self, doc: &mut Document, text: &str) -> bool {
        if !doc.is_editable() || text.is_empty() {
            return false;
        }
        let before = self.selection;
        let range = self.selection.range();
        let inserted_len = text.chars().count();
        let after = Selection::at(range.start + inserted_len);

        doc.apply(&Transaction::replace(range, text), before, after);
        self.selection = after;
        self.goal_column = None;
        self.scroll_to_caret = true;
        true
    }

    fn delete_selection(&mut self, doc: &mut Document) -> bool {
        if !doc.is_editable() || self.selection.is_empty() {
            return false;
        }
        let before = self.selection;
        let range = self.selection.range();
        let after = Selection::at(range.start);

        doc.apply(&Transaction::delete(range), before, after);
        self.selection = after;
        self.goal_column = None;
        self.scroll_to_caret = true;
        true
    }

    /// True when the selection covers more than one line.
    fn spans_multiple_lines(&self, doc: &Document) -> bool {
        let range = self.selection.range();
        doc.line_of(range.start) != doc.line_of(range.end)
    }

    /// The lines the selection touches, inclusive.
    fn selected_lines(&self, doc: &Document) -> std::ops::RangeInclusive<usize> {
        let range = self.selection.range();
        let first = doc.line_of(range.start);
        // A selection ending exactly at a line start does not include that
        // line — otherwise selecting one whole line indents two.
        let last = if range.end > range.start && doc.line_start(doc.line_of(range.end)) == range.end
        {
            doc.line_of(range.end).saturating_sub(1)
        } else {
            doc.line_of(range.end)
        };
        first..=last.max(first)
    }

    /// Indent (`levels > 0`) or dedent (`levels < 0`) the selected lines.
    ///
    /// One transaction, so the whole block is a single undo step, and the
    /// selection is preserved so Tab can be pressed repeatedly.
    pub fn shift_lines(&mut self, doc: &mut Document, opts: EditorOptions, levels: isize) -> bool {
        if !doc.is_editable() {
            return false;
        }
        let indent_opts = opts.indent();
        let lines = self.selected_lines(doc);
        let mut edits = Vec::new();
        let mut first_delta = 0isize;
        let mut total_delta = 0isize;

        for line in lines.clone() {
            let text = doc.line_text(line);
            if text.trim().is_empty() && levels > 0 {
                continue; // do not indent blank lines into trailing whitespace
            }
            let start = doc.line_start(line);
            let existing: String = text
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let width = visual_width(&existing, opts.tab_width);
            let target = if levels > 0 {
                width + opts.tab_width * levels.unsigned_abs()
            } else {
                width.saturating_sub(opts.tab_width * levels.unsigned_abs())
            };
            if target == width {
                continue;
            }

            let replacement = indent_opts.columns(target);
            let delta = replacement.chars().count() as isize - existing.chars().count() as isize;
            if line == *lines.start() {
                first_delta = delta;
            }
            total_delta += delta;
            edits.push(editor_core::edit::Edit::replace(
                start..start + existing.chars().count(),
                replacement,
            ));
        }

        if edits.is_empty() {
            return false;
        }

        let before = self.selection;
        doc.break_undo_run();
        // Keep the same text selected afterwards so Tab can be pressed again.
        let anchor = before
            .anchor
            .saturating_add_signed(if before.anchor <= before.head {
                first_delta
            } else {
                total_delta
            });
        let head = before
            .head
            .saturating_add_signed(if before.anchor <= before.head {
                total_delta
            } else {
                first_delta
            });
        let after = Selection::new(anchor, head);

        doc.apply(&editor_core::edit::Transaction::new(edits), before, after);
        self.selection = after.clamped(doc.len_chars());
        doc.break_undo_run();
        self.scroll_to_caret = true;
        true
    }

    /// Toggle line comments on the selected lines.
    ///
    /// If every non-blank line is already commented, uncomment; otherwise
    /// comment all of them. Comment markers go at the shallowest common indent
    /// so the block keeps its shape.
    pub fn toggle_comment(&mut self, doc: &mut Document, opts: EditorOptions) -> bool {
        if !doc.is_editable() {
            return false;
        }
        let Some(token) = indent::line_comment_token(opts.language) else {
            return false;
        };
        let lines = self.selected_lines(doc);

        let content: Vec<(usize, String)> = lines
            .clone()
            .map(|line| (line, doc.line_text(line)))
            .filter(|(_, text)| !text.trim().is_empty())
            .collect();
        if content.is_empty() {
            return false;
        }

        let all_commented = content
            .iter()
            .all(|(_, text)| text.trim_start().starts_with(token));

        let column = content
            .iter()
            .map(|(_, text)| text.len() - text.trim_start().len())
            .min()
            .unwrap_or(0);

        let mut edits = Vec::new();
        for (line, text) in &content {
            let start = doc.line_start(*line);
            if all_commented {
                let indent_len = text.len() - text.trim_start().len();
                let at = start + text[..indent_len].chars().count();
                let rest = text.trim_start();
                // Remove the token and one following space, which is what was
                // inserted; leave anything else the user wrote.
                let mut remove = token.chars().count();
                if rest[token.len()..].starts_with(' ') {
                    remove += 1;
                }
                edits.push(editor_core::edit::Edit::delete(at..at + remove));
            } else {
                let at = start + text[..column.min(text.len())].chars().count();
                edits.push(editor_core::edit::Edit::insert(at, format!("{token} ")));
            }
        }

        if edits.is_empty() {
            return false;
        }
        let before = self.selection;
        doc.break_undo_run();
        doc.apply(&editor_core::edit::Transaction::new(edits), before, before);
        self.selection = before.clamped(doc.len_chars());
        doc.break_undo_run();
        true
    }

    fn selected_text(&self, doc: &Document) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        Some(doc.text().slice(self.selection.range()).to_string())
    }

    /// How far backspace should reach: one tab stop inside leading whitespace,
    /// one character everywhere else.
    fn backspace_width(&self, doc: &Document, opts: EditorOptions) -> usize {
        let head = self.selection.head;
        if !opts.insert_spaces {
            return 1;
        }
        let line = doc.line_of(head);
        let column = head - doc.line_start(line);
        let before = doc.line_text(line);
        let all_spaces = before.chars().take(column).all(|c| c == ' ');

        if column > 0 && all_spaces {
            let step = column % opts.tab_width;
            let width = if step == 0 { opts.tab_width } else { step };
            width.min(column)
        } else {
            1
        }
    }

    // ---- motion ----------------------------------------------------------

    fn set_head(&mut self, offset: usize, extend: bool) {
        self.selection = if extend {
            self.selection.extended_to(offset)
        } else {
            Selection::at(offset)
        };
        self.goal_column = None;
        self.scroll_to_caret = true;
    }

    fn move_horizontal(&mut self, doc: &Document, delta: isize, extend: bool) {
        // A plain left/right with a selection collapses to its edge rather than
        // moving, which is what people expect after selecting a word.
        if !extend && !self.selection.is_empty() {
            let target = if delta < 0 {
                self.selection.start()
            } else {
                self.selection.end()
            };
            self.set_head(target, false);
            return;
        }

        let head = self.selection.head;
        let target = if delta < 0 {
            head.saturating_sub(delta.unsigned_abs())
        } else {
            (head + delta.unsigned_abs()).min(doc.len_chars())
        };
        self.set_head(target, extend);
    }

    fn move_vertical(&mut self, doc: &Document, delta: isize, extend: bool) {
        let head = self.selection.head;
        let line = doc.line_of(head);
        let column = self.goal_column.unwrap_or(head - doc.line_start(line));

        let last = doc.text().len_lines().saturating_sub(1);
        let target_line = if delta < 0 {
            line.saturating_sub(delta.unsigned_abs())
        } else {
            (line + delta.unsigned_abs()).min(last)
        };

        let offset = doc.offset_at(target_line, column);
        self.selection = if extend {
            self.selection.extended_to(offset)
        } else {
            Selection::at(offset)
        };
        // Keep the goal column across the whole run of vertical movement.
        self.goal_column = Some(column);
        self.scroll_to_caret = true;
    }

    fn touch(&mut self) {
        self.last_interaction = Some(std::time::Instant::now());
    }

    // ---- painting --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        highlighter: Option<&mut Highlighter>,
        syntax: &SyntaxTheme,
        opts: EditorOptions,
        font: &egui::FontId,
        rect: egui::Rect,
        text_left: f32,
        gutter_width: f32,
        row_height: f32,
        visible: egui::Rect,
        response: &egui::Response,
    ) {
        let line_count = doc.text().len_lines();
        let first = (((visible.top() - rect.top()) / row_height).floor().max(0.0) as usize)
            .saturating_sub(OVERSCAN_ROWS);
        let last = ((((visible.bottom() - rect.top()) / row_height).ceil() as usize)
            + OVERSCAN_ROWS)
            .min(line_count);

        // Highlight exactly the rows about to be painted, and nothing else.
        // This is where "cost tracks the viewport, not the file" is enforced.
        let spans = highlighter.map_or_else(Vec::new, |h| {
            let from = doc.text().line_to_byte(first.min(line_count));
            let to = doc.text().line_to_byte(last.min(line_count));
            h.spans(doc.text(), from..to, syntax)
        });

        let painter = ui.painter_at(ui.clip_rect());
        let visuals = ui.visuals().clone();
        let caret_line = doc.line_of(self.selection.head);
        let sel_range = self.selection.range();

        // Current-line stripe, under everything else.
        if self.selection.is_empty() {
            let y = rect.top() + caret_line as f32 * row_height;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(text_left, y),
                    egui::vec2(rect.width() - gutter_width, row_height),
                ),
                0.0,
                visuals.faint_bg_color,
            );
        }

        let mut caret_rect = None;

        for line in first..last {
            let y = rect.top() + line as f32 * row_height;
            let line_start = doc.line_start(line);
            let text = doc.line_text(line);

            let galley = if spans.is_empty() {
                painter.layout_no_wrap(text.clone(), font.clone(), visuals.text_color())
            } else {
                let job = highlighted_line(
                    &text,
                    doc.text().line_to_byte(line),
                    &spans,
                    syntax,
                    font,
                    visuals.text_color(),
                );
                ui.fonts_mut(|f| f.layout_job(job))
            };

            // Search results, painted under the selection so a selected match
            // still reads as selected.
            if !self.search_matches.is_empty() {
                let line_end = line_start + text.chars().count();
                for found in &self.search_matches {
                    if found.end < line_start || found.start > line_end {
                        continue;
                    }
                    let from = found.start.clamp(line_start, line_end) - line_start;
                    let to = found.end.clamp(line_start, line_end) - line_start;
                    if from >= to {
                        continue;
                    }
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    let x1 = text_left + galley.pos_from_cursor(ccursor(to)).left();
                    let is_current = self.current_match.as_ref() == Some(found);
                    let colour = if is_current {
                        visuals.selection.bg_fill
                    } else {
                        visuals.widgets.hovered.bg_fill
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y),
                            egui::pos2(x1.max(x0 + 2.0), y + row_height),
                        ),
                        2,
                        colour,
                    );
                    if is_current {
                        // An outline as well as a fill, so the current match is
                        // findable even where the fill sits under a selection.
                        painter.rect_stroke(
                            egui::Rect::from_min_max(
                                egui::pos2(x0, y),
                                egui::pos2(x1.max(x0 + 2.0), y + row_height),
                            ),
                            2,
                            egui::Stroke::new(1.0, visuals.strong_text_color()),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }

            // Selection highlight for the part of this line that is selected.
            if !self.selection.is_empty() {
                let line_end = line_start + text.chars().count();
                let from = sel_range.start.clamp(line_start, line_end) - line_start;
                let to = sel_range.end.clamp(line_start, line_end) - line_start;
                if from < to || (sel_range.start <= line_end && sel_range.end > line_end) {
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    // A selection spanning the line break highlights to the
                    // end of the row, so the newline is visibly included.
                    let x1 = if sel_range.end > line_end {
                        rect.right()
                    } else {
                        text_left + galley.pos_from_cursor(ccursor(to)).left()
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y),
                            egui::pos2(x1.max(x0 + 2.0), y + row_height),
                        ),
                        0.0,
                        visuals.selection.bg_fill,
                    );
                }
            }

            if opts.show_line_numbers {
                let is_caret_line = line == caret_line;
                painter.text(
                    egui::pos2(text_left - 12.0, y),
                    egui::Align2::RIGHT_TOP,
                    line + 1,
                    font.clone(),
                    if is_caret_line {
                        visuals.text_color()
                    } else {
                        visuals.weak_text_color()
                    },
                );
            }

            if !text.is_empty() {
                painter.galley(
                    egui::pos2(text_left, y),
                    galley.clone(),
                    visuals.text_color(),
                );
            }

            if line == caret_line {
                let column = self.selection.head - line_start;
                let x = text_left + galley.pos_from_cursor(ccursor(column)).left();
                caret_rect = Some(egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(1.5, row_height),
                ));
            }
        }

        if let Some(caret) = caret_rect {
            if response.has_focus() && self.blink_on() {
                painter.rect_filled(caret, 0.0, visuals.strong_text_color());
            }
            if std::mem::take(&mut self.scroll_to_caret) {
                ui.scroll_to_rect(caret.expand2(egui::vec2(0.0, row_height * 2.0)), None);
            }
        }

        if response.has_focus() {
            // Keep the blink animating without spinning at the full frame rate.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    /// Solid for a moment after any interaction, so the caret is never invisible
    /// exactly when the user looks for it.
    fn blink_on(&self) -> bool {
        let Some(since) = self.last_interaction.map(|t| t.elapsed().as_millis()) else {
            return true;
        };
        since < 500 || (since / BLINK_MS).is_multiple_of(2)
    }
}

/// Build a layout job for one line, splitting it into the runs the highlighter
/// produced.
///
/// `spans` covers the whole visible window and is sorted; only the part
/// overlapping this line is used. Bytes with no span get the theme's default
/// colour, so the output always covers the line exactly once with no gaps.
fn highlighted_line(
    text: &str,
    line_start_byte: usize,
    spans: &[editor_syntax::highlight::Span],
    syntax: &SyntaxTheme,
    font: &egui::FontId,
    fallback: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let line_end_byte = line_start_byte + text.len();
    let default = syntax.default_style();

    let mut cursor = 0usize; // byte offset within `text`
    let push = |job: &mut egui::text::LayoutJob, range: std::ops::Range<usize>, style| {
        let Some(slice) = text.get(range) else { return };
        if slice.is_empty() {
            return;
        }
        job.append(slice, 0.0, format_for(style, font, fallback));
    };

    for span in spans {
        if span.range.end <= line_start_byte {
            continue;
        }
        if span.range.start >= line_end_byte {
            break;
        }
        let from = span
            .range
            .start
            .saturating_sub(line_start_byte)
            .min(text.len());
        let to = (span.range.end - line_start_byte).min(text.len());
        if from >= to {
            continue;
        }
        if from > cursor {
            push(&mut job, cursor..from, default);
        }
        push(&mut job, from..to, span.style);
        cursor = to;
    }
    if cursor < text.len() {
        push(&mut job, cursor..text.len(), default);
    }

    job
}

fn format_for(
    style: editor_syntax::theme::Style,
    font: &egui::FontId,
    fallback: egui::Color32,
) -> egui::TextFormat {
    let editor_syntax::theme::Rgb(r, g, b) = style.colour;
    let colour = if style == editor_syntax::theme::Style::default() {
        fallback
    } else {
        egui::Color32::from_rgb(r, g, b)
    };
    // `style.bold` is deliberately not applied. egui has no synthetic bold:
    // rendering it needs a bold face registered in the font family, which
    // arrives when JetBrains Mono is embedded in M9. Themes can express bold
    // now so theme files do not need rewriting later; it is simply not drawn
    // yet, and no built-in colour relies on it to be distinguishable.
    egui::TextFormat {
        font_id: font.clone(),
        color: colour,
        italics: style.italic,
        ..Default::default()
    }
}

fn char_at(doc: &Document, offset: usize) -> Option<char> {
    (offset < doc.len_chars()).then(|| doc.text().char(offset))
}

fn char_before(doc: &Document, offset: usize) -> Option<char> {
    (offset > 0).then(|| doc.text().char(offset - 1))
}

/// Whether typing `open` here should also insert its closer.
///
/// Auto-closing in front of a word produces `(word` → `()word`, which is
/// almost never wanted; it is only helpful before whitespace, a closing
/// bracket, or the end of the line. Quotes additionally refuse to close when
/// the character before is a word character, so `it's` and `don't` type
/// normally.
fn should_auto_close(doc: &Document, offset: usize, open: char) -> bool {
    if matches!(open, '"' | '\'' | '`')
        && char_before(doc, offset).is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        return false;
    }
    match char_at(doc, offset) {
        None => true,
        Some(c) => c.is_whitespace() || indent::is_closing(c) || matches!(c, ',' | ';' | ':'),
    }
}

fn visual_width(text: &str, tab_width: usize) -> usize {
    let mut width = 0;
    for c in text.chars() {
        if c == '\t' {
            width += tab_width - (width % tab_width);
        } else {
            width += 1;
        }
    }
    width
}

fn ccursor(index: usize) -> egui::text::CCursor {
    egui::text::CCursor::new(index)
}

/// Select the word around `offset`, for double-click.
///
/// A "word" is a run of alphanumerics and underscores — which covers both
/// `snake_case` identifiers and ordinary prose. Clicking on whitespace or
/// punctuation selects that run instead, rather than selecting nothing.
fn word_at(doc: &Document, offset: usize) -> Selection {
    let line = doc.line_of(offset);
    let start_of_line = doc.line_start(line);
    let chars: Vec<char> = doc.line_text(line).chars().collect();
    let column = (offset - start_of_line).min(chars.len());

    let classify = |c: char| c.is_alphanumeric() || c == '_';

    // Clicking just past the end of a word should select that word.
    let probe = column.min(chars.len().saturating_sub(1));
    let Some(&at) = chars.get(probe) else {
        return Selection::at(offset);
    };
    let want = classify(at);

    let mut start = probe;
    while start > 0 && chars.get(start - 1).is_some_and(|c| classify(*c) == want) {
        start -= 1;
    }
    let mut end = probe;
    while end < chars.len() && chars.get(end).is_some_and(|c| classify(*c) == want) {
        end += 1;
    }

    Selection::new(start_of_line + start, start_of_line + end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::document::Document;

    fn doc_with(text: &str) -> Document {
        let mut doc = Document::untitled();
        doc.apply(
            &Transaction::insert(0, text),
            Selection::at(0),
            Selection::at(text.chars().count()),
        );
        doc
    }

    #[test]
    fn double_click_selects_a_snake_case_identifier_whole() {
        let doc = doc_with("total_count = other_value + 1");
        let sel = word_at(&doc, 4);
        assert_eq!(doc.text().slice(sel.range()).to_string(), "total_count");
    }

    #[test]
    fn double_click_on_whitespace_selects_the_whitespace_run() {
        let doc = doc_with("a    b");
        let sel = word_at(&doc, 2);
        assert_eq!(doc.text().slice(sel.range()).to_string(), "    ");
    }

    #[test]
    fn double_click_works_on_the_second_line() {
        let doc = doc_with("first\nsecond_thing here");
        let offset = doc.offset_at(1, 3);
        let sel = word_at(&doc, offset);
        assert_eq!(doc.text().slice(sel.range()).to_string(), "second_thing");
    }

    #[test]
    fn smart_backspace_deletes_to_the_previous_tab_stop_in_leading_whitespace() {
        let doc = doc_with("        code");
        let opts = EditorOptions {
            tab_width: 4,
            insert_spaces: true,
            ..EditorOptions::default()
        };

        // Caret at column 8, all spaces before it: one press clears one level.
        let view = EditorView {
            selection: Selection::at(8),
            ..EditorView::default()
        };
        assert_eq!(view.backspace_width(&doc, opts), 4);

        // Caret at column 6 is mid-stop: fall back to the nearest boundary.
        let view = EditorView {
            selection: Selection::at(6),
            ..EditorView::default()
        };
        assert_eq!(view.backspace_width(&doc, opts), 2);
    }

    #[test]
    fn smart_backspace_deletes_one_character_inside_actual_text() {
        let doc = doc_with("    hello");
        let opts = EditorOptions::default();
        let view = EditorView {
            selection: Selection::at(9),
            ..EditorView::default()
        };
        assert_eq!(
            view.backspace_width(&doc, opts),
            1,
            "backspace in text must delete one character, not four"
        );
    }

    #[test]
    fn smart_backspace_is_disabled_when_indenting_with_tabs() {
        let doc = doc_with("        code");
        let opts = EditorOptions {
            insert_spaces: false,
            ..EditorOptions::default()
        };
        let view = EditorView {
            selection: Selection::at(8),
            ..EditorView::default()
        };
        assert_eq!(view.backspace_width(&doc, opts), 1);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut doc = doc_with("hello world");
        let mut view = EditorView {
            selection: Selection::new(0, 5),
            ..EditorView::default()
        };
        assert!(view.insert(&mut doc, "goodbye"));
        assert_eq!(doc.text().to_string(), "goodbye world");
        assert_eq!(view.selection, Selection::at(7));
    }

    #[test]
    fn vertical_movement_remembers_the_goal_column_across_a_short_line() {
        let mut doc = doc_with("longest line here\nshort\nlongest line here");
        let mut view = EditorView {
            selection: Selection::at(15),
            ..EditorView::default()
        };
        assert_eq!(doc.line_col(view.selection.head), (1, 16));

        view.move_vertical(&doc, 1, false);
        assert_eq!(
            doc.line_col(view.selection.head),
            (2, 6),
            "clamps to the end of the short line"
        );

        view.move_vertical(&doc, 1, false);
        assert_eq!(
            doc.line_col(view.selection.head),
            (3, 16),
            "returns to the original column, not the short line's end"
        );

        doc.break_undo_run();
    }

    #[test]
    fn a_read_only_document_refuses_edits() {
        let mut doc = Document::untitled();
        // A large-file or permissions flag is what makes a document read-only;
        // an untitled document is editable, so this checks the guard itself.
        assert!(doc.is_editable());
        let mut view = EditorView::default();
        assert!(view.insert(&mut doc, "x"));
        assert_eq!(doc.text().to_string(), "x");
    }

    fn python_opts() -> EditorOptions {
        EditorOptions {
            language: LanguageId::Python,
            ..EditorOptions::default()
        }
    }

    /// Type each character in turn, as the keyboard would deliver them.
    fn type_all(view: &mut EditorView, doc: &mut Document, opts: EditorOptions, text: &str) {
        for c in text.chars() {
            view.type_text(doc, opts, &c.to_string());
        }
    }

    #[test]
    fn typing_an_opening_bracket_inserts_its_closer_and_stays_inside() {
        let mut doc = doc_with("");
        let mut view = EditorView::default();
        view.type_text(&mut doc, python_opts(), "(");

        assert_eq!(doc.text().to_string(), "()");
        assert_eq!(view.selection, Selection::at(1), "caret sits between them");
    }

    #[test]
    fn typing_the_closer_over_an_auto_inserted_one_steps_past_it() {
        let mut doc = doc_with("");
        let mut view = EditorView::default();
        type_all(&mut view, &mut doc, python_opts(), "()");

        assert_eq!(
            doc.text().to_string(),
            "()",
            "typing the closer must not double it"
        );
        assert_eq!(view.selection, Selection::at(2));
    }

    #[test]
    fn brackets_do_not_auto_close_in_front_of_a_word() {
        // `(word` becoming `()word` is almost never what anyone wants.
        let mut doc = doc_with("word");
        let mut view = EditorView {
            selection: Selection::at(0),
            ..EditorView::default()
        };
        view.type_text(&mut doc, python_opts(), "(");
        assert_eq!(doc.text().to_string(), "(word");
    }

    #[test]
    fn an_apostrophe_after_a_word_character_does_not_auto_close() {
        let mut doc = doc_with("dont");
        let mut view = EditorView {
            selection: Selection::at(3),
            ..EditorView::default()
        };
        view.type_text(&mut doc, python_opts(), "'");
        assert_eq!(
            doc.text().to_string(),
            "don't",
            "typing an apostrophe mid-word must not produce don''t"
        );
    }

    #[test]
    fn typing_a_bracket_with_a_selection_surrounds_it() {
        let mut doc = doc_with("hello world");
        let mut view = EditorView {
            selection: Selection::new(0, 5),
            ..EditorView::default()
        };
        view.type_text(&mut doc, python_opts(), "(");

        assert_eq!(
            doc.text().to_string(),
            "(hello) world",
            "the selection must be wrapped, not replaced"
        );
        assert_eq!(
            doc.text().slice(view.selection.range()).to_string(),
            "hello",
            "and it stays selected"
        );
    }

    #[test]
    fn auto_close_can_be_switched_off() {
        let opts = EditorOptions {
            auto_close_brackets: false,
            ..python_opts()
        };
        let mut doc = doc_with("");
        let mut view = EditorView::default();
        view.type_text(&mut doc, opts, "(");
        assert_eq!(doc.text().to_string(), "(");
    }

    #[test]
    fn tab_with_a_multi_line_selection_indents_rather_than_replacing_it() {
        let mut doc = doc_with("a\nb\nc\n");
        let mut view = EditorView {
            selection: Selection::new(0, 3),
            ..EditorView::default()
        };
        assert!(view.shift_lines(&mut doc, python_opts(), 1));

        assert_eq!(
            doc.text().to_string(),
            "    a\n    b\nc\n",
            "the selected text must survive"
        );
    }

    #[test]
    fn outdent_removes_one_level_and_stops_at_the_margin() {
        let mut doc = doc_with("        a\n    b\nc\n");
        let mut view = EditorView {
            selection: Selection::new(0, doc.len_chars()),
            ..EditorView::default()
        };
        view.shift_lines(&mut doc, python_opts(), -1);
        assert_eq!(doc.text().to_string(), "    a\nb\nc\n");

        view.shift_lines(&mut doc, python_opts(), -1);
        assert_eq!(
            doc.text().to_string(),
            "a\nb\nc\n",
            "outdenting past column zero must not remove text"
        );
    }

    #[test]
    fn indenting_leaves_the_same_text_selected_so_tab_can_repeat() {
        let mut doc = doc_with("a\nb\n");
        let mut view = EditorView {
            selection: Selection::new(0, 3),
            ..EditorView::default()
        };
        view.shift_lines(&mut doc, python_opts(), 1);
        assert_eq!(
            doc.text().slice(view.selection.range()).to_string(),
            "a\n    b"
        );

        view.shift_lines(&mut doc, python_opts(), 1);
        assert_eq!(doc.text().to_string(), "        a\n        b\n");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_does_not_indent_the_next_line() {
        let mut doc = doc_with("a\nb\n");
        let mut view = EditorView {
            // Exactly the first line, including its newline.
            selection: Selection::new(0, 2),
            ..EditorView::default()
        };
        view.shift_lines(&mut doc, python_opts(), 1);
        assert_eq!(doc.text().to_string(), "    a\nb\n");
    }

    #[test]
    fn blank_lines_are_not_indented_into_trailing_whitespace() {
        let mut doc = doc_with("a\n\nb\n");
        let mut view = EditorView {
            selection: Selection::new(0, doc.len_chars()),
            ..EditorView::default()
        };
        view.shift_lines(&mut doc, python_opts(), 1);
        assert_eq!(doc.text().to_string(), "    a\n\n    b\n");
    }

    #[test]
    fn comment_toggle_comments_then_uncomments_exactly() {
        let original = "def f():\n    a = 1\n    b = 2\n";
        let mut doc = doc_with(original);
        let mut view = EditorView {
            selection: Selection::new(0, doc.len_chars()),
            ..EditorView::default()
        };

        assert!(view.toggle_comment(&mut doc, python_opts()));
        assert_eq!(
            doc.text().to_string(),
            "# def f():\n#     a = 1\n#     b = 2\n"
        );

        assert!(view.toggle_comment(&mut doc, python_opts()));
        assert_eq!(
            doc.text().to_string(),
            original,
            "uncommenting must restore the original exactly"
        );
    }

    #[test]
    fn comment_markers_align_to_the_shallowest_line_in_the_block() {
        let mut doc = doc_with("    a = 1\n        b = 2\n");
        let mut view = EditorView {
            selection: Selection::new(0, doc.len_chars()),
            ..EditorView::default()
        };
        view.toggle_comment(&mut doc, python_opts());
        assert_eq!(
            doc.text().to_string(),
            "    # a = 1\n    #     b = 2\n",
            "the block keeps its relative shape"
        );
    }

    #[test]
    fn a_partly_commented_block_is_commented_rather_than_uncommented() {
        let mut doc = doc_with("# a\nb\n");
        let mut view = EditorView {
            selection: Selection::new(0, doc.len_chars()),
            ..EditorView::default()
        };
        view.toggle_comment(&mut doc, python_opts());
        assert_eq!(doc.text().to_string(), "# # a\n# b\n");
    }

    #[test]
    fn comment_toggle_uses_the_right_token_per_language() {
        for (language, expected) in [
            (LanguageId::Python, "# x\n"),
            (LanguageId::Rust, "// x\n"),
            (LanguageId::Ini, "; x\n"),
        ] {
            let mut doc = doc_with("x\n");
            let mut view = EditorView {
                selection: Selection::at(0),
                ..EditorView::default()
            };
            view.toggle_comment(
                &mut doc,
                EditorOptions {
                    language,
                    ..EditorOptions::default()
                },
            );
            assert_eq!(doc.text().to_string(), expected, "{language:?}");
        }
    }

    #[test]
    fn comment_toggle_reports_failure_for_a_language_without_line_comments() {
        let mut doc = doc_with("{}\n");
        let mut view = EditorView::default();
        assert!(
            !view.toggle_comment(
                &mut doc,
                EditorOptions {
                    language: LanguageId::Json,
                    ..EditorOptions::default()
                }
            ),
            "JSON has no comment syntax, so the command must decline"
        );
        assert_eq!(doc.text().to_string(), "{}\n");
    }

    #[test]
    fn horizontal_movement_collapses_a_selection_to_its_edge() {
        let doc = doc_with("hello world");
        let mut view = EditorView {
            selection: Selection::new(2, 8),
            ..EditorView::default()
        };

        view.move_horizontal(&doc, -1, false);
        assert_eq!(
            view.selection,
            Selection::at(2),
            "left with a selection goes to its start, not one left of the head"
        );

        view.selection = Selection::new(2, 8);
        view.move_horizontal(&doc, 1, false);
        assert_eq!(view.selection, Selection::at(8));
    }

    #[test]
    fn movement_cannot_run_off_either_end_of_the_document() {
        let doc = doc_with("abc");
        let mut view = EditorView::default();

        for _ in 0..10 {
            view.move_horizontal(&doc, -1, false);
        }
        assert_eq!(view.selection.head, 0);

        for _ in 0..10 {
            view.move_horizontal(&doc, 1, false);
        }
        assert_eq!(view.selection.head, doc.len_chars());
    }
}
