//! Pointer and keyboard: turning what the user did into caret movement and
//! edits.

use super::*;

impl EditorView {
    /// Note where the pointer is resting over the text.
    ///
    /// Only records; [`Self::hovered`] decides when it has rested long enough.
    /// Moving to a different character restarts the clock, so dragging across a
    /// line never asks about anything.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn track_pointer(
        &mut self,
        ui: &egui::Ui,
        doc: &Document,
        response: &egui::Response,
        font: &egui::FontId,
        rect: egui::Rect,
        text_left: f32,
        row_height: f32,
    ) {
        // A pointer that is dragging, or over the gutter, is not asking about
        // anything. Nor is one over a different widget.
        let Some(pos) = response.hover_pos() else {
            self.resting = None;
            return;
        };
        // Nor is one over the sticky header: the offset under it belongs to a
        // line the reader cannot see, so a popup about it would be about the
        // wrong symbol entirely.
        if response.dragged() || pos.x < text_left || self.sticky_at(pos).is_some() {
            self.resting = None;
            return;
        }

        let offset = self.offset_at_pos(ui, doc, font, pos, rect, text_left, row_height);
        match self.resting {
            Some(resting) if resting.offset == offset => {}
            _ => {
                self.resting = Some(Resting {
                    offset,
                    at: pos,
                    since: std::time::Instant::now(),
                });
            }
        }

        // The clock has to be *watched*, or nothing wakes the loop between the
        // pointer stopping and the delay expiring — and the hover would appear
        // only on the next keystroke.
        if self
            .resting
            .is_some_and(|r| r.since.elapsed() < HOVER_DELAY)
        {
            ui.ctx().request_repaint_after(HOVER_DELAY);
        }
    }

    /// What the pointer has settled on, once it has been there long enough.
    ///
    /// `None` while it is still moving, or when it is not over the text at all.
    #[must_use]
    pub fn hovered(&self) -> Option<Hovered> {
        let resting = self.resting?;
        (resting.since.elapsed() >= HOVER_DELAY).then_some(Hovered {
            offset: resting.offset,
            at: resting.at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_mouse(
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

        // The sticky header takes a click before the text hidden behind it
        // does. Jumping to the declaration is what a pinned row is for: it is
        // the line you scrolled away from, and clicking it is how you go back.
        if response.clicked()
            && let Some(line) = self.sticky_at(pos)
        {
            self.set_caret(doc.line_start(line));
            return false;
        }

        // The gutter takes clicks of its own. Dragging through it is still a
        // selection, so only a click is intercepted here.
        if response.clicked() {
            let line = self.line_at_pos(pos.y, rect, row_height);
            match self.zone_at(pos.x, rect) {
                // Blame and the change bar are something to read.
                Zone::Annotation => return false,
                // Setting a breakpoint by clicking the gutter is how every
                // other editor does it and the first thing anyone tries.
                // Handled before the caret moves, so the click does not also
                // jump the caret to line 1.
                Zone::Breakpoints => {
                    if line < doc.line_count() {
                        self.toggle_breakpoint = Some(line);
                    }
                    return false;
                }
                Zone::Folds => {
                    if self.folds.iter().any(|f| f.first == line) {
                        self.toggle_fold(line);
                    }
                    return false;
                }
                // A click on a line number puts the caret at the start of that
                // line, which is what falling through does.
                Zone::Numbers | Zone::Text => {}
            }
        }

        let offset = self.offset_at_pos(ui, doc, font, pos, rect, text_left, row_height);

        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));

        // Where the button went down, for a drag that is only just starting.
        // `interact_pointer_pos` follows the live pointer, and egui does not
        // call a press a drag until it has travelled past the drag threshold,
        // so by this frame `pos` can already be a character or two along from
        // the character the user actually pressed on.
        let press_offset = if response.drag_started() {
            ui.input(|i| i.pointer.press_origin())
                .map_or(offset, |origin| {
                    self.offset_at_pos(ui, doc, font, origin, rect, text_left, row_height)
                })
        } else {
            offset
        };

        self.apply_pointer(
            doc,
            Gesture {
                clicked: response.clicked(),
                double_clicked: response.double_clicked(),
                dragged: response.dragged(),
                drag_started: response.drag_started(),
                alt,
                shift,
            },
            offset,
            press_offset,
        );
        false
    }

    /// What a pointer gesture does to the selection.
    ///
    /// Split out of [`Self::handle_mouse`] because everything above it is
    /// hit-testing that needs a live egui context, while this is a rule about
    /// anchors that can be stated — and tested — in offsets alone.
    pub(super) fn apply_pointer(
        &mut self,
        doc: &Document,
        g: Gesture,
        offset: usize,
        press_offset: usize,
    ) {
        if g.alt && g.dragged {
            // Alt+drag is a column selection: the rectangle between where the
            // drag began and where the pointer is, one caret per line. Held
            // separately from `column_anchor` because the offset the drag
            // started at is not recoverable from the selection once the first
            // frame of the drag has rewritten it.
            if g.drag_started {
                // Anchor this drag, not the one before it.
                self.column_anchor = None;
            }
            let anchor = *self.column_anchor.get_or_insert(press_offset);
            self.select_column(doc, anchor, offset);
            self.goal_column = None;
            self.touch();
            return;
        }
        self.column_anchor = None;

        if g.double_clicked {
            self.collapse_cursors();
            self.selection = word_at(doc, offset);
        } else if g.alt && g.clicked {
            // Alt+click adds a caret, and Alt+clicking one that is already
            // there takes it away again — otherwise a misplaced caret can only
            // be undone by starting over.
            self.toggle_cursor_at(offset);
        } else if g.dragged || g.shift {
            // Dragging or shift-clicking extends from the existing anchor.
            //
            // A drag that is only just starting has no anchor of its own yet,
            // so it drops one where the button went down. Without that it goes
            // on extending whatever the previous drag selected: the press
            // itself changes nothing (egui reports a click on release, and a
            // drag only once the pointer has moved), so by the time the drag
            // is recognised the stale anchor is still sitting there. Shift is
            // the exception — shift+drag is asking to extend what is already
            // selected.
            if g.drag_started && !g.shift {
                self.collapse_cursors();
                self.selection = Selection::at(press_offset);
            }
            self.selection = self.selection.extended_to(offset);
        } else {
            self.collapse_cursors();
            self.selection = Selection::at(offset);
        }

        self.goal_column = None;
        self.touch();
    }

    /// Add a caret at `offset`, or remove the one already there.
    pub(super) fn toggle_cursor_at(&mut self, offset: usize) {
        let (mut cursors, primary) = self.cursors();
        if let Some(at) = cursors
            .iter()
            .position(|s| s.is_empty() && s.head == offset)
        {
            // Never remove the last one: an editor with no caret has no way to
            // get one back except by clicking, which is what just happened.
            if cursors.len() > 1 {
                cursors.remove(at);
                let primary = if primary == at {
                    0
                } else if primary > at {
                    primary - 1
                } else {
                    primary
                };
                self.install_cursors(cursors, primary);
            }
            return;
        }
        cursors.push(Selection::at(offset));
        // The caret just placed becomes the primary: it is the one being
        // looked at, so it is the one the status bar and scrolling follow.
        let last = cursors.len() - 1;
        self.install_cursors(cursors, last);
    }

    /// Replace the caret set with a rectangle: one selection per line between
    /// the two offsets, spanning the same two columns.
    ///
    /// Lines shorter than the left-hand column get nothing rather than a caret
    /// jammed against their end. A column selection is about a rectangle of
    /// text, and inventing carets on lines that do not reach into it means the
    /// next keystroke edits lines the rectangle never covered.
    pub(super) fn select_column(&mut self, doc: &Document, from: usize, to: usize) {
        let (first_line, first_col) = doc.line_col(from);
        let (last_line, last_col) = doc.line_col(to);
        let (first_line, first_col) = (first_line - 1, first_col - 1);
        let (last_line, last_col) = (last_line - 1, last_col - 1);

        let (top, bottom) = (first_line.min(last_line), first_line.max(last_line));
        let (left, right) = (first_col.min(last_col), first_col.max(last_col));

        let mut cursors = Vec::new();
        for line in top..=bottom.min(doc.line_count().saturating_sub(1)) {
            let len = doc.line_len(line);
            if left > len {
                continue;
            }
            let start = doc.line_start(line);
            // A zero-width rectangle is a column of carets, which is the whole
            // point of Alt+drag straight down.
            cursors.push(Selection::new(start + left, start + right.min(len)));
        }
        if cursors.is_empty() {
            return;
        }
        // The line the pointer is on stays primary, so the view follows it.
        let primary = if last_line >= first_line {
            cursors.len() - 1
        } else {
            0
        };
        self.install_cursors(cursors, primary);
    }

    /// Where the caret goes for a click in the blank space under the text, or
    /// `None` if this click is on a row that has text in it.
    ///
    /// Below the last row there is no line to measure against and no column the
    /// pointer can be said to be over, so the caret goes to the end of the last
    /// line rather than to whatever column the x happens to fall in. That is
    /// what every other editor does, and the only answer that does not depend
    /// on how far along an empty strip you clicked.
    ///
    /// `line_at` clamps to the last *visible* row, so with the tail of the file
    /// folded away this lands at the end of the fold's header rather than
    /// somewhere inside text the user cannot see.
    ///
    /// Split out of [`Self::offset_at_pos`] for the reason [`Self::apply_pointer`]
    /// is split out of [`Self::handle_mouse`]: measuring a column needs a live
    /// egui context to lay text out, while this is a rule about offsets that can
    /// be stated -- and tested -- without one.
    pub(super) fn offset_below_last_row(
        &self,
        doc: &Document,
        y: f32,
        rect: egui::Rect,
        row_height: f32,
    ) -> Option<usize> {
        let row = ((y - rect.top()) / row_height).floor().max(0.0) as usize;
        if row < self.fold_map.visible_rows() {
            return None;
        }
        Some(doc.offset_at(self.fold_map.line_at(row), usize::MAX))
    }

    /// Map a screen position to a character offset.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn offset_at_pos(
        &self,
        ui: &egui::Ui,
        doc: &Document,
        font: &egui::FontId,
        pos: egui::Pos2,
        rect: egui::Rect,
        text_left: f32,
        row_height: f32,
    ) -> usize {
        let row = ((pos.y - rect.top()) / row_height).floor().max(0.0) as usize;
        let line = self.fold_map.line_at(row);

        if let Some(offset) = self.offset_below_last_row(doc, pos.y, rect, row_height) {
            return offset;
        }

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

    pub(super) fn handle_keys(
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

    pub(super) fn handle_key(
        &mut self,
        doc: &mut Document,
        opts: EditorOptions,
        key: egui::Key,
        modifiers: egui::Modifiers,
        rows_per_page: usize,
    ) -> bool {
        use egui::Key;

        let extend = modifiers.shift;
        let by_word = word_modifier(modifiers);
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
                // Deleting a whole word is one undo step, not one per
                // character, so the run has to be broken either side of it.
                if by_word {
                    doc.break_undo_run();
                }
                // Widen every collapsed caret into the span it would delete,
                // then delete the lot in one transaction.
                self.each_cursor(doc, |view, doc| {
                    if !view.selection.is_empty() || view.selection.head == 0 {
                        return;
                    }
                    let head = view.selection.head;
                    let start = if by_word {
                        // Ctrl+Backspace deletes the word, not the tab stop.
                        word::prev_boundary(doc.text(), head)
                    } else {
                        // Smart backspace: inside leading whitespace, delete
                        // back to the previous tab stop rather than one space
                        // at a time.
                        head - view.backspace_width(doc, opts)
                    };
                    view.selection = Selection::new(start, head);
                });
                let changed = self.delete_selection(doc);
                if by_word {
                    doc.break_undo_run();
                }
                changed
            }
            Key::Delete => {
                if by_word {
                    doc.break_undo_run();
                }
                self.each_cursor(doc, |view, doc| {
                    let head = view.selection.head;
                    if !view.selection.is_empty() || head >= doc.len_chars() {
                        return;
                    }
                    let end = if by_word {
                        word::next_boundary(doc.text(), head)
                    } else {
                        head + 1
                    };
                    view.selection = Selection::new(head, end);
                });
                let changed = self.delete_selection(doc);
                if by_word {
                    doc.break_undo_run();
                }
                changed
            }
            // Undo, redo and select-all are application commands, dispatched
            // through the registry so the menus and the keyboard agree. They
            // are deliberately not handled here.
            // Every motion below runs once per caret. Making a motion
            // multi-cursor aware therefore takes nothing: `each_cursor`
            // installs each caret in turn and collects where it ended up.
            Key::ArrowLeft if by_word => {
                self.each_cursor(doc, |v, doc| {
                    let target = word::prev_boundary(doc.text(), v.selection.head);
                    v.set_head(target, extend);
                });
                false
            }
            Key::ArrowRight if by_word => {
                self.each_cursor(doc, |v, doc| {
                    let target = word::next_boundary(doc.text(), v.selection.head);
                    v.set_head(target, extend);
                });
                false
            }
            Key::ArrowLeft => {
                self.each_cursor(doc, |v, doc| v.move_horizontal(doc, -1, extend));
                false
            }
            Key::ArrowRight => {
                self.each_cursor(doc, |v, doc| v.move_horizontal(doc, 1, extend));
                false
            }
            Key::ArrowUp if modifiers.command && modifiers.alt => {
                self.add_cursor_vertically(doc, -1);
                false
            }
            Key::ArrowDown if modifiers.command && modifiers.alt => {
                self.add_cursor_vertically(doc, 1);
                false
            }
            Key::ArrowUp => {
                self.each_cursor(doc, |v, doc| v.move_vertical(doc, -1, extend));
                false
            }
            Key::ArrowDown => {
                self.each_cursor(doc, |v, doc| v.move_vertical(doc, 1, extend));
                false
            }
            Key::PageUp => {
                let rows = rows_per_page as isize;
                self.each_cursor(doc, |v, doc| v.move_vertical(doc, -rows, extend));
                false
            }
            Key::PageDown => {
                let rows = rows_per_page as isize;
                self.each_cursor(doc, |v, doc| v.move_vertical(doc, rows, extend));
                false
            }
            Key::Escape => {
                // How you get out of multi-cursor. Handled here rather than as
                // an application command because with no extra carets it has to
                // fall through to whatever else wants Escape.
                self.collapse_cursors();
                false
            }
            Key::Home if modifiers.command => {
                self.collapse_cursors();
                self.set_head(0, extend);
                false
            }
            Key::End if modifiers.command => {
                self.collapse_cursors();
                self.set_head(doc.len_chars(), extend);
                false
            }
            Key::Home => {
                self.each_cursor(doc, |v, doc| {
                    // Toggle between the first non-whitespace character and
                    // column zero -- pressing Home twice on an indented line
                    // reaches the margin, which is what every editor does.
                    let line = doc.line_of(v.selection.head);
                    let start = doc.line_start(line);
                    let indent = doc
                        .line_text(line)
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .count();
                    let target = if v.selection.head == start + indent {
                        start
                    } else {
                        start + indent
                    };
                    v.set_head(target, extend);
                });
                false
            }
            Key::End => {
                self.each_cursor(doc, |v, doc| {
                    let line = doc.line_of(v.selection.head);
                    v.set_head(doc.line_start(line) + doc.line_len(line), extend);
                });
                false
            }
            _ => false,
        }
    }

    // ---- motion ----------------------------------------------------------

    pub(super) fn set_head(&mut self, offset: usize, extend: bool) {
        self.selection = if extend {
            self.selection.extended_to(offset)
        } else {
            Selection::at(offset)
        };
        self.goal_column = None;
        self.scroll_to_caret = true;
    }

    pub(super) fn move_horizontal(&mut self, doc: &Document, delta: isize, extend: bool) {
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

    pub(super) fn move_vertical(&mut self, doc: &Document, delta: isize, extend: bool) {
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

    pub(super) fn touch(&mut self) {
        self.last_interaction = Some(std::time::Instant::now());
    }
}

/// Whether this key press means "by word" rather than "by character".
///
/// Ctrl on Windows and Linux; Option on macOS, where Cmd+arrow is start/end of
/// line and Option+arrow is the word motion. `Modifiers::COMMAND` cannot be used
/// because it maps to Cmd on macOS, which would put word motion on the one
/// combination macOS users expect to do something else.
pub(super) fn word_modifier(modifiers: egui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.alt
    } else {
        modifiers.ctrl
    }
}

/// Select the word around `offset`, for double-click.
///
/// A "word" is a run of alphanumerics and underscores — which covers both
/// `snake_case` identifiers and ordinary prose. Clicking on whitespace or
/// punctuation selects that run instead, rather than selecting nothing.
/// Character offset of the first `needle` at or after character offset `from`.
///
/// Character offsets, not byte offsets: everything else in the editor counts
/// characters, and `str::find` counts bytes, so the conversion has to happen
/// somewhere. Doing it here keeps it out of the caller, where mixing the two
/// would put a caret in the middle of a multi-byte character.
pub(super) fn find_from(text: &str, needle: &str, from: usize) -> Option<usize> {
    let start_byte = text
        .char_indices()
        .nth(from)
        .map_or(text.len(), |(byte, _)| byte);
    let hit = text.get(start_byte..)?.find(needle)? + start_byte;
    Some(text[..hit].chars().count())
}

pub(super) fn word_at(doc: &Document, offset: usize) -> Selection {
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
