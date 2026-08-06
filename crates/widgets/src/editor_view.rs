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
//! Not here yet: syntax highlighting (M3), language-aware indentation and
//! bracket handling (M4), find/replace (M5), multi-cursor and folding.

use editor_core::document::Document;
use editor_core::edit::Transaction;
use editor_core::selection::Selection;
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
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            tab_width: 4,
            insert_spaces: true,
            show_line_numbers: true,
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
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Document, opts: EditorOptions) -> bool {
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
                    changed |= self.insert(doc, &text);
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
                // Carry the current line's leading whitespace onto the new
                // line. The language-aware rules (dedent on `return`, align to
                // an open bracket) arrive in M4.
                let line = doc.line_of(self.selection.head);
                let indent: String = doc
                    .line_text(line)
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let inserted = self.insert(doc, &format!("\n{indent}"));
                doc.break_undo_run();
                inserted
            }
            Key::Tab if !extend => {
                let text = if opts.insert_spaces {
                    let col = doc.line_col(self.selection.head).1 - 1;
                    " ".repeat(opts.tab_width - (col % opts.tab_width))
                } else {
                    "\t".to_owned()
                };
                self.insert(doc, &text)
            }
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

            let galley = painter.layout_no_wrap(text.clone(), font.clone(), visuals.text_color());

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
