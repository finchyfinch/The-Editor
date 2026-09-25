//! Changing the text: typing, deleting, and the line-level commands, each one
//! transaction so that each is one undo step.

use super::*;

impl EditorView {
    /// Handle typed text, applying the bracket, quote and dedent rules.
    pub(super) fn type_text(
        &mut self,
        doc: &mut Document,
        opts: EditorOptions,
        text: &str,
    ) -> bool {
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

        // `def name(` inside a class wants `self` next, near enough always.
        // Only for a single caret: eight carets each starting a method is not
        // a thing anybody does, and working out the answer per caret in a
        // document the other carets are also editing is not worth it.
        if c == '('
            && opts.language == LanguageId::Python
            && self.selection.is_empty()
            && self.secondary.is_empty()
            && let Some(first) = methods::first_parameter(doc.text(), self.selection.head)
        {
            let parameter = first.as_str();
            let changed = self.insert(doc, &format!("({parameter})"));
            // Caret after the parameter, before the `)`, so the next thing
            // typed is the comma and the argument that follows it.
            self.selection = Selection::at(self.selection.head.saturating_sub(1));
            return changed;
        }

        // The third quote of a triple. Typing one quote gives a pair to type
        // between, and the pair is exactly what makes typing a triple awkward:
        // by the third keystroke the buffer holds two quotes and the caret is
        // past them, and every further quote either steps over something or
        // adds another pair. Recognising the triple ends that, and is also the
        // one moment where what is being written is unambiguous.
        if matches!(c, '"' | '\'')
            && opts.language == LanguageId::Python
            && self.selection.is_empty()
            && self.secondary.is_empty()
            && completes_triple_quote(doc, self.selection.head, c)
        {
            return self.open_triple_quote(doc, opts, c);
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
    pub(super) fn reindent_line(
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

    /// Finish a triple quote, and fill it in if it is a docstring.
    ///
    /// Two quotes are already in the buffer and the third is being typed. The
    /// closing three go in with it, so the string is terminated from the
    /// moment it is opened — an unterminated one turns the rest of the file
    /// into a string, which colours it wrongly and stops it parsing.
    pub(super) fn open_triple_quote(
        &mut self,
        doc: &mut Document,
        opts: EditorOptions,
        quote: char,
    ) -> bool {
        let head = self.selection.head;
        let opened = head - 2;
        let line = doc.line_of(opened);

        if let Some(style) = opts.docstrings
            && quote == '"'
            // Only when the quotes are the first thing on their line. A triple
            // quote in the middle of an expression is a string, whatever is
            // above it.
            && doc.text().slice(doc.line_start(line)..opened).chars().all(char::is_whitespace)
            && let Some(definition) = docstring::definition_above(doc.text(), line)
        {
            let indent: String = doc
                .line_text(line)
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let unit = if opts.insert_spaces {
                " ".repeat(opts.tab_width)
            } else {
                "\t".to_owned()
            };
            let (body, caret) = docstring::render(&definition, style, &indent, &unit);

            // Added after the two quotes already typed rather than replacing
            // them, because a plain insertion at the caret folds into the undo
            // entry those quotes made: one Ctrl+Z then takes back the whole
            // docstring instead of leaving two quotes behind. A dozen lines
            // that have to be deleted by hand would be worse than not
            // offering them.
            let rest: String = body.chars().skip(2).collect();
            let changed = self.insert(doc, &rest);
            self.selection = Selection::at(opened + caret);
            // The summary typed next is its own entry, not an extension of
            // this one.
            doc.break_undo_run();
            self.scroll_to_caret = true;
            return changed;
        }

        let closer: String = std::iter::repeat_n(quote, 4).collect();
        let changed = self.insert(doc, &closer);
        // Between the two triples, which is where the string goes.
        self.selection = Selection::at(head + 1);
        changed
    }

    pub(super) fn insert(&mut self, doc: &mut Document, text: &str) -> bool {
        if !doc.is_editable() || text.is_empty() {
            return false;
        }
        let (cursors, primary) = self.cursors();
        let edits = cursors
            .iter()
            .map(|sel| editor_core::edit::Edit::replace(sel.range(), text))
            .collect();
        self.apply_at_every_cursor(doc, Transaction::new(edits), cursors, primary)
    }

    pub(super) fn delete_selection(&mut self, doc: &mut Document) -> bool {
        if !doc.is_editable() || self.cursors().0.iter().all(|s| s.is_empty()) {
            return false;
        }
        let (cursors, primary) = self.cursors();
        let edits = cursors
            .iter()
            .filter(|sel| !sel.is_empty())
            .map(|sel| editor_core::edit::Edit::delete(sel.range()))
            .collect();
        self.apply_at_every_cursor(doc, Transaction::new(edits), cursors, primary)
    }

    /// Apply one transaction and put every caret where its own edit left it.
    ///
    /// The whole point of doing this in a single transaction is that it is a
    /// single undo step: eight carets typing a word is one Ctrl+Z, not eight.
    ///
    /// Each caret lands at `remap` of the *end* of its old range. That one rule
    /// covers both cases — a collapsed caret is carried along by its own
    /// insertion, and a caret with a selection ends up after the replacement —
    /// which is why the caret positions are not computed per case here.
    pub(super) fn apply_at_every_cursor(
        &mut self,
        doc: &mut Document,
        tx: Transaction,
        cursors: Vec<Selection>,
        primary: usize,
    ) -> bool {
        if tx.is_empty() {
            return false;
        }
        let after: Vec<Selection> = cursors
            .iter()
            .map(|sel| Selection::at(editor_core::edit::remap(&tx, sel.range().end)))
            .collect();

        // Undo restores the primary caret; the extra ones are not worth
        // recording in the history, and an undo that resurrects carets the user
        // has since dismissed is worse than one that does not.
        doc.apply(&tx, cursors[primary], after[primary]);
        self.install_cursors(after, primary);
        self.goal_column = None;
        self.scroll_to_caret = true;
        true
    }

    /// True when the selection covers more than one line.
    pub(super) fn spans_multiple_lines(&self, doc: &Document) -> bool {
        let range = self.selection.range();
        doc.line_of(range.start) != doc.line_of(range.end)
    }

    /// The lines the selection touches, inclusive.
    pub(super) fn selected_lines(&self, doc: &Document) -> std::ops::RangeInclusive<usize> {
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
    /// The lines the selection touches, as a character range covering whole
    /// lines including the trailing newline of the last one.
    ///
    /// The last line of a document has no trailing newline, so the range stops
    /// at the end of the text; callers that move a block have to put the
    /// newline back themselves. Getting this wrong is how line operations end
    /// up joining two lines together.
    pub(super) fn line_block(&self, doc: &Document) -> std::ops::Range<usize> {
        let lines = self.selected_lines(doc);
        let start = doc.line_start(*lines.start());
        let last = *lines.end();
        let end = (doc.line_start(last) + doc.line_len(last) + 1).min(doc.len_chars());
        start..end
    }

    /// Copy the selected lines and paste them directly below.
    ///
    /// The caret follows the copy, so pressing it twice gives three, which is
    /// what people expect from a duplicate command.
    pub fn duplicate_lines(&mut self, doc: &mut Document) -> bool {
        if !doc.is_editable() {
            return false;
        }
        let block = self.line_block(doc);
        let mut text: String = doc.text().slice(block.clone()).chars().collect();
        // The final line of a file has no newline of its own; the copy needs
        // one or it runs into the line it was copied from.
        if !text.ends_with('\n') {
            text.insert(0, '\n');
        }
        let inserted = text.chars().count();

        let before = self.selection;
        let after = Selection::new(before.anchor + inserted, before.head + inserted);
        doc.break_undo_run();
        doc.apply(
            &Transaction::new(vec![editor_core::edit::Edit::insert(block.end, text)]),
            before,
            after,
        );
        self.selection = after;
        doc.break_undo_run();
        self.scroll_to_caret = true;
        self.touch();
        true
    }

    /// Delete the selected lines outright.
    pub fn delete_lines(&mut self, doc: &mut Document) -> bool {
        if !doc.is_editable() || doc.len_chars() == 0 {
            return false;
        }
        let mut block = self.line_block(doc);
        // Deleting the last line takes the newline *before* it, or the file is
        // left ending in a blank line that was not there before.
        if block.end >= doc.len_chars() && block.start > 0 {
            block.start -= 1;
        }

        let before = self.selection;
        let after = Selection::at(block.start.min(doc.len_chars().saturating_sub(1)));
        doc.break_undo_run();
        doc.apply(
            &Transaction::new(vec![editor_core::edit::Edit::delete(block.clone())]),
            before,
            after,
        );
        self.selection = Selection::at(block.start.min(doc.len_chars()));
        doc.break_undo_run();
        self.scroll_to_caret = true;
        self.touch();
        true
    }

    /// Move the selected lines up or down by one.
    ///
    /// Implemented as one replace over both blocks rather than two edits, so
    /// there is no intermediate state in which the text is duplicated or lost,
    /// and so the whole move is a single undo step.
    pub fn move_lines(&mut self, doc: &mut Document, direction: isize) -> bool {
        if !doc.is_editable() {
            return false;
        }
        let lines = self.selected_lines(doc);
        let (first, last) = (*lines.start(), *lines.end());
        // A file ending in a newline has a final empty line that ropey counts
        // but nobody wrote. Moving the last real line "down" into it would
        // append a blank line that was not there before.
        let count = doc.line_count();
        let movable = if count > 1 && doc.line_len(count - 1) == 0 {
            count - 1
        } else {
            count
        };

        let other = if direction < 0 {
            if first == 0 {
                return false;
            }
            first - 1
        } else {
            if last + 1 >= movable {
                return false;
            }
            last + 1
        };

        // The two blocks, in document order.
        let (upper, lower) = if direction < 0 {
            (line_range(doc, other, other), line_range(doc, first, last))
        } else {
            (line_range(doc, first, last), line_range(doc, other, other))
        };

        let span = upper.start..lower.end;
        let upper_text: String = doc.text().slice(upper.clone()).chars().collect();
        let lower_text: String = doc.text().slice(lower.clone()).chars().collect();

        // Whichever block ends the file has no trailing newline. Swapping the
        // two verbatim would move that missing newline to the middle and glue
        // two lines together, so the separator is rebuilt rather than carried.
        let upper_body = upper_text.strip_suffix('\n').unwrap_or(&upper_text);
        let lower_body = lower_text.strip_suffix('\n').unwrap_or(&lower_text);
        let ends_with_newline = lower_text.ends_with('\n');
        let mut swapped = format!("{lower_body}\n{upper_body}");
        if ends_with_newline {
            swapped.push('\n');
        }

        // Where the moved block lands, so the same lines stay selected.
        let shift = if direction < 0 {
            -((upper_body.chars().count() + 1) as isize)
        } else {
            (lower_body.chars().count() + 1) as isize
        };
        let moved = |offset: usize| (offset as isize + shift).max(0) as usize;

        let before = self.selection;
        let after = Selection::new(moved(before.anchor), moved(before.head));
        doc.break_undo_run();
        doc.apply(
            &Transaction::new(vec![editor_core::edit::Edit::replace(span, swapped)]),
            before,
            after,
        );
        self.selection = after;
        doc.break_undo_run();
        self.scroll_to_caret = true;
        self.touch();
        true
    }

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

    pub(super) fn selected_text(&self, doc: &Document) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        Some(doc.text().slice(self.selection.range()).to_string())
    }

    /// How far backspace should reach: one tab stop inside leading whitespace,
    /// one character everywhere else.
    pub(super) fn backspace_width(&self, doc: &Document, opts: EditorOptions) -> usize {
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
}

pub(super) fn char_at(doc: &Document, offset: usize) -> Option<char> {
    (offset < doc.len_chars()).then(|| doc.text().char(offset))
}

pub(super) fn char_before(doc: &Document, offset: usize) -> Option<char> {
    (offset > 0).then(|| doc.text().char(offset - 1))
}

/// Whether typing `open` here should also insert its closer.
///
/// Auto-closing in front of a word produces `(word` → `()word`, which is
/// almost never wanted; it is only helpful before whitespace, a closing
/// bracket, or the end of the line. Quotes additionally refuse to close when
/// the character before is a word character, so `it's` and `don't` type
/// normally.
/// True when typing `quote` at `offset` would be the third of a triple.
///
/// The two before it have to be the same quote, and the one before *those*
/// must not be: `""""` is not a triple being opened, it is a triple that is
/// already there being typed past.
pub(super) fn completes_triple_quote(doc: &Document, offset: usize, quote: char) -> bool {
    offset >= 2
        && char_before(doc, offset) == Some(quote)
        && char_before(doc, offset - 1) == Some(quote)
        && char_before(doc, offset - 2) != Some(quote)
}

pub(super) fn should_auto_close(doc: &Document, offset: usize, open: char) -> bool {
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

pub(super) fn visual_width(text: &str, tab_width: usize) -> usize {
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

/// The character range of whole lines `first..=last`, including the trailing
/// newline of the last one where there is one.
pub(super) fn line_range(doc: &Document, first: usize, last: usize) -> std::ops::Range<usize> {
    let start = doc.line_start(first);
    let end = (doc.line_start(last) + doc.line_len(last) + 1).min(doc.len_chars());
    start..end
}
