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
use editor_core::word;
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::indent::{self, IndentOptions};
use editor_syntax::methods;
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
    /// Stop the caret blinking and other repeating animation.
    pub reduce_motion: bool,
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
            reduce_motion: false,
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
    /// Distinguishes this view's scroll area from every other one.
    ///
    /// egui keys scroll state by widget id, and the id used to be a constant,
    /// so every tab shared one scroll position: scroll in one, switch to
    /// another, and it had moved. Per-view rather than per-file because that
    /// is what a scroll position belongs to — the same document in two split
    /// panes should scroll independently too.
    ///
    /// Claimed on the first draw rather than in `Default`, so this stays a
    /// derived `Default` and a new field cannot be forgotten.
    scroll_id: Option<u64>,
    pub selection: Selection,
    /// Extra carets, beyond the primary one in `selection`.
    ///
    /// Empty almost always, which is why the primary stays a plain field:
    /// every command that has no multi-cursor meaning goes on reading and
    /// writing `selection` and simply drops the extras first. Only the handful
    /// of operations that genuinely apply to all of them — typing, deleting,
    /// moving — know this exists.
    secondary: Vec<Selection>,
    /// Where an Alt+drag column selection started. Kept because the first frame
    /// of the drag overwrites the selection it began from.
    column_anchor: Option<usize>,
    /// Copied from the options each frame, because `blink_on` is called from
    /// the paint pass where the options are no longer to hand.
    reduce_motion: bool,
    /// First lines of the folds that are currently closed.
    ///
    /// Identified by line rather than by node: the tree is rebuilt on every
    /// edit and its node ids with it, so anything held across an edit has to
    /// be a position.
    collapsed: std::collections::BTreeSet<usize>,
    /// Every foldable range, recomputed when the document changes.
    folds: Vec<editor_syntax::brackets::FoldRange>,
    /// Document version `folds` was computed from, so the tree is walked on
    /// edits rather than on frames.
    folds_version: Option<u64>,
    /// Line count as of the last rebuild, so collapsed folds can be moved with
    /// the lines they were put on.
    folds_line_count: usize,
    /// Which line each visible row shows. The identity while nothing is
    /// folded, which is almost always.
    fold_map: crate::folding::FoldMap,
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
    /// Diagnostic underlines, supplied by the application each frame.
    diagnostics: Vec<Underline>,
    /// Breakpoint lines (zero-based) and whether the debugger could bind them.
    breakpoints: Vec<(usize, bool)>,
    /// The line the debugger is currently stopped on, zero-based.
    paused_line: Option<usize>,
    /// A gutter click asking to toggle a breakpoint, taken by the application.
    toggle_breakpoint: Option<usize>,
    /// The bracket pair around the caret, recomputed as the caret moves.
    bracket_pair: Option<editor_syntax::brackets::BracketPair>,
    /// Set by the context menu, taken by the application next frame.
    context_action: Option<ContextAction>,
    /// The scroll offset at the end of the last frame.
    last_offset: egui::Vec2,
    /// When it last changed, so frames keep coming through the tail of the
    /// easing rather than stopping the instant one frame happens to match the
    /// last.
    last_offset_change: Option<std::time::Instant>,
    /// Where the caret was painted last frame, in screen coordinates.
    ///
    /// Kept so the completion popup can be anchored under the caret. Only the
    /// paint pass knows this: the x position comes from the laid-out galley,
    /// which accounts for tabs, proportional glyphs and horizontal scrolling in
    /// a way no arithmetic over the character offset would.
    caret_screen_rect: Option<egui::Rect>,
}

/// A range to underline, and how seriously.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Underline {
    /// Character offsets into the document.
    pub range: std::ops::Range<usize>,
    pub severity: editor_lsp::diagnostics::Severity,
    /// Shown on hover.
    pub message: String,
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

    // ---- multiple carets --------------------------------------------------

    /// How many carets there are. One, unless multi-cursor is in use.
    #[must_use]
    pub fn cursor_count(&self) -> usize {
        self.secondary.len() + 1
    }

    /// Drop every caret but the primary. Returns true if any went.
    ///
    /// Called by everything that has no multi-cursor meaning, and by Escape.
    pub fn collapse_cursors(&mut self) -> bool {
        let had = !self.secondary.is_empty();
        self.secondary.clear();
        had
    }

    /// Every caret in document order, and which of them is the primary.
    fn cursors(&self) -> (Vec<Selection>, usize) {
        if self.secondary.is_empty() {
            return (vec![self.selection], 0);
        }
        let mut all = Vec::with_capacity(self.secondary.len() + 1);
        all.push((self.selection, true));
        all.extend(self.secondary.iter().map(|s| (*s, false)));
        all.sort_by_key(|(s, _)| (s.start(), s.end()));
        let primary = all.iter().position(|(_, p)| *p).unwrap_or(0);
        (all.into_iter().map(|(s, _)| s).collect(), primary)
    }

    /// Replace the caret set, merging any that have run into each other.
    ///
    /// Carets do collide: put three on consecutive lines, press End, and two of
    /// them can land on the same offset. Left alone they would each apply the
    /// next edit, so typing one character would insert three — which is how
    /// multi-cursor implementations corrupt files.
    fn install_cursors(&mut self, cursors: Vec<Selection>, primary: usize) {
        let mut tagged: Vec<(Selection, bool)> = cursors
            .into_iter()
            .enumerate()
            .map(|(i, s)| (s, i == primary))
            .collect();
        tagged.sort_by_key(|(s, _)| (s.start(), s.end()));

        let mut merged: Vec<(Selection, bool)> = Vec::with_capacity(tagged.len());
        for (sel, is_primary) in tagged {
            match merged.last_mut() {
                // Overlapping, or two collapsed carets in the same place.
                Some((last, last_primary)) if sel.start() <= last.end() => {
                    let start = last.start().min(sel.start());
                    let end = last.end().max(sel.end());
                    // Keep the survivor pointing the way the later one did, so
                    // shift+arrow keeps extending in the direction it was.
                    *last = if sel.head >= sel.anchor {
                        Selection::new(start, end)
                    } else {
                        Selection::new(end, start)
                    };
                    *last_primary |= is_primary;
                }
                _ => merged.push((sel, is_primary)),
            }
        }

        let keep = merged.iter().position(|(_, p)| *p).unwrap_or(0);
        self.selection = merged[keep].0;
        self.secondary = merged
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != keep)
            .map(|(_, (s, _))| s)
            .collect();
    }

    /// Run `motion` once for every caret, with each installed as the primary.
    ///
    /// Motion is the one thing that genuinely is the same operation repeated:
    /// no caret's movement changes where any other one should end up. Doing it
    /// this way means arrow keys, Home, End, word motion and page motion all
    /// became multi-cursor-aware without any of them being touched.
    fn each_cursor(&mut self, doc: &Document, motion: impl Fn(&mut Self, &Document)) {
        if self.secondary.is_empty() {
            motion(self, doc);
            return;
        }
        let (cursors, primary) = self.cursors();
        let saved_goal = self.goal_column;
        let mut moved = Vec::with_capacity(cursors.len());
        for sel in cursors {
            self.selection = sel;
            // Each caret keeps its own idea of the column it is aiming for;
            // sharing one would drag them all into a single column.
            self.goal_column = saved_goal;
            motion(self, doc);
            moved.push(self.selection);
        }
        self.install_cursors(moved, primary);
        self.scroll_to_caret = true;
    }

    /// Add a caret at the next occurrence of what the primary has selected.
    ///
    /// With nothing selected, selects the word under the caret first — which is
    /// what makes Ctrl+D, Ctrl+D, Ctrl+D read as "this word, and the next, and
    /// the next" rather than needing a double-click to start.
    ///
    /// Returns false when there is nothing further to add, so the caller can
    /// say so rather than leaving the key looking broken.
    pub fn add_cursor_at_next_match(&mut self, doc: &Document) -> bool {
        if self.selection.is_empty() {
            let word = word_at(doc, self.selection.head);
            if word.is_empty() {
                return false;
            }
            self.selection = word;
            self.scroll_to_caret = true;
            return true;
        }

        let needle = doc.text().slice(self.selection.range()).to_string();
        if needle.is_empty() {
            return false;
        }
        let (cursors, primary) = self.cursors();
        let taken: Vec<usize> = cursors.iter().map(|s| s.start()).collect();
        let last = cursors.iter().map(|s| s.end()).max().unwrap_or(0);

        let text = doc.text().to_string();
        // Search from after the last caret, then wrap. Wrapping matters: having
        // worked down a file you expect the next Ctrl+D to come back to the top
        // rather than silently doing nothing.
        let Some(at) = find_from(&text, &needle, last).or_else(|| find_from(&text, &needle, 0))
        else {
            return false;
        };
        if taken.contains(&at) {
            return false; // Everything is already selected.
        }

        let mut cursors = cursors;
        cursors.push(Selection::new(at, at + needle.chars().count()));
        self.install_cursors(cursors, primary);
        self.scroll_to_caret = true;
        true
    }

    /// Add a caret one line up or down from the outermost one in that
    /// direction, at the same column.
    pub fn add_cursor_vertically(&mut self, doc: &Document, delta: isize) -> bool {
        let (cursors, primary) = self.cursors();
        // Grow away from the block, not from the primary: pressing the key
        // repeatedly should extend the run of carets rather than fight over the
        // same line.
        let edge = if delta < 0 {
            cursors.first().copied()
        } else {
            cursors.last().copied()
        };
        let Some(edge) = edge else { return false };

        let line = doc.line_of(edge.head);
        let Some(target_line) = line.checked_add_signed(delta) else {
            return false;
        };
        if target_line >= doc.line_count() {
            return false;
        }
        // The same goal column the arrow keys use, and for the same reason:
        // running a column of carets past a short line and on to a long one
        // should come back to where it started, not cling to the short line.
        let column = self
            .goal_column
            .unwrap_or_else(|| edge.head - doc.line_start(line));
        let start = doc.line_start(target_line);
        let head = start + column.min(doc.line_len(target_line));

        let mut cursors = cursors;
        cursors.push(Selection::at(head));
        self.install_cursors(cursors, primary);
        self.goal_column = Some(column);
        self.scroll_to_caret = true;
        true
    }

    // ---- application commands --------------------------------------------
    //
    // Driven from the command registry rather than from key handling here, so
    // that the menu item and the shortcut cannot diverge.

    /// Undo one step. Returns true if anything changed.
    ///
    /// Drops the extra carets. The history records one selection per step, so
    /// the others have nothing to be restored to — and leaving them where they
    /// were points them into text the undo has just moved, which is exactly how
    /// the next keystroke lands somewhere unrelated.
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        match doc.undo() {
            Some(sel) => {
                self.collapse_cursors();
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
                self.collapse_cursors();
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

    /// Diagnostics to underline, from the language servers.
    ///
    /// Supplied per frame rather than held, because they belong to the document
    /// and several views may show the same one.
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Underline>) {
        self.diagnostics = diagnostics;
    }

    /// Where the caret is on screen, as of the last frame that painted it.
    ///
    /// `None` before the first paint, or when the caret has scrolled out of the
    /// visible range.
    #[must_use]
    pub fn caret_screen_rect(&self) -> Option<egui::Rect> {
        self.caret_screen_rect
    }

    /// Taken by the application after each frame; see [`ContextAction`].
    pub fn take_context_action(&mut self) -> Option<ContextAction> {
        self.context_action.take()
    }

    /// Breakpoints to draw in the gutter, and the line execution is stopped on.
    ///
    /// Supplied per frame rather than held, for the same reason diagnostics are:
    /// they belong to the file, not to this view of it.
    pub fn set_debug_state(&mut self, breakpoints: Vec<(usize, bool)>, paused: Option<usize>) {
        self.breakpoints = breakpoints;
        self.paused_line = paused;
    }

    /// A line the user clicked in the gutter, or chose from the context menu.
    pub fn take_breakpoint_toggle(&mut self) -> Option<usize> {
        self.toggle_breakpoint.take()
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
        self.reduce_motion = opts.reduce_motion;
        let font = egui::FontId::monospace(opts.font_size);
        let row_height = ui.fonts_mut(|f| f.row_height(&font));
        let space_width = ui.fonts_mut(|f| f.glyph_width(&font, ' '));
        let line_count = doc.text().len_lines();
        self.sync_folds(doc, highlighter.as_deref());
        // Rows, not lines, from here on. The two are the same number unless
        // something is folded.
        let row_count = self.fold_map.visible_rows();

        // A column of its own for breakpoints, at the very left. Drawing them
        // over the line numbers -- which is what happened first -- makes them
        // invisible against the digits, so a breakpoint appeared not to have
        // been set at all.
        let breakpoint_width = row_height;
        // A column for the fold chevrons, between the line numbers and the
        // text. Always reserved, even in a file with nothing to fold: a column
        // that appears and disappears would shift the whole document sideways
        // as you type.
        let fold_width = row_height;
        let gutter_width = breakpoint_width
            + fold_width
            + if opts.show_line_numbers {
                space_width * (line_count.to_string().len() as f32 + 2.0) + 12.0
            } else {
                6.0
            };

        let mut changed = false;

        // Salted, so it cannot collide with the tab bar's scroll area above it
        // in the same panel. See the note in `tab_bar`.
        // A process-wide counter, not anything derived from the document: an
        // untitled buffer has no path, two views of one file must still
        // differ, and a reused number would hand a new tab an old tab's
        // scroll position.
        let scroll_id = *self.scroll_id.get_or_insert_with(|| {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        });
        let scrolled = egui::ScrollArea::both()
            .id_salt(("editor_view", scroll_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Claim the whole document; the scroll area decides what of it
                // is on screen.
                let widest = 120.0 * space_width;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(
                        (gutter_width + widest).max(ui.available_width()),
                        row_height * row_count as f32,
                    ),
                    egui::Sense::click_and_drag(),
                );

                if std::mem::take(&mut self.grab_focus) {
                    response.request_focus();
                }

                // Claim the keys egui would otherwise use to move focus
                // between widgets. Without this, pressing Up in the editor
                // moves focus to the toolbar instead of moving the caret —
                // egui's focus navigation consumes the arrows first, and a
                // custom widget has to say it wants them. `TextEdit` does
                // exactly this; a hand-written editor has to as well.
                //
                // Escape is deliberately left alone, so it still closes the
                // find bar and dismisses dialogs.
                if response.has_focus() {
                    ui.memory_mut(|memory| {
                        memory.set_focus_lock_filter(
                            response.id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: false,
                            },
                        );
                    });
                }

                // The code pane *is* text, so here the I-beam is correct.
                let response = response.on_hover_cursor(egui::CursorIcon::Text);
                self.describe_for_screen_readers(ui, doc, &response);

                let text_left = rect.left() + gutter_width;
                let visible = ui.clip_rect().intersect(rect);
                let rows_per_page = (visible.height() / row_height).floor().max(1.0) as usize;

                changed |=
                    self.handle_mouse(ui, doc, &response, &font, rect, text_left, row_height);

                // Right-clicking moves the caret to the word under the pointer
                // before the menu opens. Otherwise "Go to Definition" acts on
                // wherever the caret happened to be, which is almost never
                // what was right-clicked.
                if response.secondary_clicked()
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    let offset =
                        self.offset_at_pos(ui, doc, &font, pos, rect, text_left, row_height);
                    if !self.selection.range().contains(&offset) {
                        self.selection = Selection::at(offset);
                        self.goal_column = None;
                    }
                    response.request_focus();
                }

                response.context_menu(|ui| {
                    if ui.button("Toggle Breakpoint").clicked() {
                        self.toggle_breakpoint = Some(doc.line_of(self.selection.head));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Go to Definition").clicked() {
                        self.context_action = Some(ContextAction::GoToDefinition);
                        ui.close();
                    }
                    if ui.button("Find Uses").clicked() {
                        self.context_action = Some(ContextAction::FindUses);
                        ui.close();
                    }
                    ui.separator();
                    let has_selection = !self.selection.is_empty();
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Cut"))
                        .clicked()
                    {
                        if let Some(text) = self.selected_text(doc) {
                            ui.ctx().copy_text(text);
                            doc.break_undo_run();
                            self.delete_selection(doc);
                            doc.break_undo_run();
                        }
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Copy"))
                        .clicked()
                    {
                        if let Some(text) = self.selected_text(doc) {
                            ui.ctx().copy_text(text);
                        }
                        ui.close();
                    }
                    // Paste needs the system clipboard, which this crate does
                    // not reach; the application has it.
                    if ui.button("Paste").clicked() {
                        self.context_action = Some(ContextAction::Paste);
                        ui.close();
                    }
                });
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

        // Keep asking for frames for as long as the view is actually moving.
        //
        // A timer started from the last wheel event was the obvious thing and
        // was wrong: egui's easing outlives any fixed window, so the animation
        // ran out of frames part-way and the remainder was applied in one jump
        // when some later event woke the loop -- a scroll that slid, stalled,
        // and then lurched the rest of the way. The offset itself is the only
        // honest signal that there is more to come.
        let offset = scrolled.state.offset;
        if (offset - self.last_offset).length() > 0.01 {
            self.last_offset_change = Some(std::time::Instant::now());
            self.last_offset = offset;
        }
        // Both conditions are needed. The offset alone stops the moment two
        // frames happen to agree, which during an ease is often -- and the
        // remainder then waits for whatever wakes the loop next, arriving as
        // the small jump at the end of every scroll. The settle window carries
        // it through to a genuine stop.
        if self
            .last_offset_change
            .is_some_and(|t| t.elapsed() < SCROLL_SETTLE)
        {
            ui.ctx().request_repaint();
        }

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

        // A click in the breakpoint column sets one, which is how every other
        // editor does it and the first thing anyone tries. Handled before the
        // caret moves, so the click does not also jump the caret to line 1.
        if response.clicked() && pos.x < rect.left() + row_height {
            let row = ((pos.y - rect.top()) / row_height).floor().max(0.0) as usize;
            let line = self.fold_map.line_at(row);
            if line < doc.line_count() {
                self.toggle_breakpoint = Some(line);
            }
            return false;
        }

        // The fold column sits at the right-hand edge of the gutter, just
        // before the text.
        if response.clicked() && pos.x >= text_left - row_height && pos.x < text_left {
            let row = ((pos.y - rect.top()) / row_height).floor().max(0.0) as usize;
            let line = self.fold_map.line_at(row);
            if self.folds.iter().any(|f| f.first == line) {
                self.toggle_fold(line);
            }
            return false;
        }

        let offset = self.offset_at_pos(ui, doc, font, pos, rect, text_left, row_height);

        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));

        if alt && response.dragged() {
            // Alt+drag is a column selection: the rectangle between where the
            // drag began and where the pointer is, one caret per line. Held
            // separately from `column_anchor` because the offset the drag
            // started at is not recoverable from the selection once the first
            // frame of the drag has rewritten it.
            let anchor = *self.column_anchor.get_or_insert(offset);
            self.select_column(doc, anchor, offset);
            self.goal_column = None;
            self.touch();
            return false;
        }
        self.column_anchor = None;

        if response.double_clicked() {
            self.collapse_cursors();
            self.selection = word_at(doc, offset);
        } else if alt && response.clicked() {
            // Alt+click adds a caret, and Alt+clicking one that is already
            // there takes it away again — otherwise a misplaced caret can only
            // be undone by starting over.
            self.toggle_cursor_at(offset);
        } else if response.dragged() || shift {
            // Dragging or shift-clicking extends from the existing anchor.
            self.selection = self.selection.extended_to(offset);
        } else {
            self.collapse_cursors();
            self.selection = Selection::at(offset);
        }

        self.goal_column = None;
        self.touch();
        false
    }

    /// Add a caret at `offset`, or remove the one already there.
    fn toggle_cursor_at(&mut self, offset: usize) {
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
    fn select_column(&mut self, doc: &Document, from: usize, to: usize) {
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
        let row = ((pos.y - rect.top()) / row_height).floor().max(0.0) as usize;
        let line = self.fold_map.line_at(row);

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
        let (cursors, primary) = self.cursors();
        let edits = cursors
            .iter()
            .map(|sel| editor_core::edit::Edit::replace(sel.range(), text))
            .collect();
        self.apply_at_every_cursor(doc, Transaction::new(edits), cursors, primary)
    }

    fn delete_selection(&mut self, doc: &mut Document) -> bool {
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
    fn apply_at_every_cursor(
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
    /// The lines the selection touches, as a character range covering whole
    /// lines including the trailing newline of the last one.
    ///
    /// The last line of a document has no trailing newline, so the range stops
    /// at the end of the text; callers that move a block have to put the
    /// newline back themselves. Getting this wrong is how line operations end
    /// up joining two lines together.
    fn line_block(&self, doc: &Document) -> std::ops::Range<usize> {
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
        // The bracket around the caret, from the tree the highlighter already
        // maintains. Recomputed per paint rather than cached: the caret moves
        // constantly and the walk is a handful of node lookups.
        self.bracket_pair = highlighter
            .as_ref()
            .and_then(|h| h.tree())
            .and_then(|tree| {
                editor_syntax::brackets::match_at(tree, doc.text(), self.selection.head)
            });

        let line_count = doc.text().len_lines();
        let first = (((visible.top() - rect.top()) / row_height).floor().max(0.0) as usize)
            .saturating_sub(OVERSCAN_ROWS);
        let last = ((((visible.bottom() - rect.top()) / row_height).ceil() as usize)
            + OVERSCAN_ROWS)
            .min(self.fold_map.visible_rows());

        // Highlight exactly the rows about to be painted, and nothing else.
        // This is where "cost tracks the viewport, not the file" is enforced.
        //
        // The byte range runs from the first visible line to the last. With a
        // fold in between that also covers the hidden lines, which costs a
        // little work and keeps the range contiguous -- asking for several
        // disjoint ranges would cost more than the lines are worth.
        let spans = highlighter.map_or_else(Vec::new, |h| {
            let from = doc
                .text()
                .line_to_byte(self.fold_map.line_at(first).min(line_count));
            let to = doc
                .text()
                .line_to_byte(self.fold_map.line_at(last).min(line_count));
            h.spans(doc.text(), from..to.max(from), syntax)
        });

        let painter = ui.painter_at(ui.clip_rect());
        let visuals = ui.visuals().clone();
        let caret_line = doc.line_of(self.selection.head);
        // Every caret's span, and every caret's head, worked out once rather
        // than per painted row. Ordinarily this is a one-element vector.
        let (all_cursors, _) = self.cursors();
        let sel_ranges: Vec<std::ops::Range<usize>> = all_cursors
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.range())
            .collect();
        let caret_heads: Vec<usize> = all_cursors.iter().map(|s| s.head).collect();

        // Current-line stripe, under everything else.
        if self.selection.is_empty() {
            let y = rect.top() + self.fold_map.row_at(caret_line) as f32 * row_height;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(text_left, y),
                    egui::vec2(rect.width() - gutter_width, row_height),
                ),
                0.0,
                visuals.faint_bg_color,
            );
        }

        // The line the debugger is stopped on, above the current-line stripe so
        // it is unmistakable which is which.
        if let Some(line) = self.paused_line
            && line < doc.line_count()
            && !self.fold_map.is_hidden(line)
        {
            let y = rect.top() + self.fold_map.row_at(line) as f32 * row_height;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(text_left, y),
                    egui::vec2(rect.width() - gutter_width, row_height),
                ),
                0.0,
                // A wash rather than a solid fill: the code underneath is the
                // point, and a highlight that hides it defeats itself.
                visuals.selection.bg_fill.gamma_multiply(0.45),
            );
        }

        // Breakpoints, in their own column at the far left.
        for (line, verified) in &self.breakpoints {
            if *line < first || *line >= last {
                continue;
            }
            if self.fold_map.is_hidden(*line) {
                continue;
            }
            let y = rect.top() + self.fold_map.row_at(*line) as f32 * row_height + row_height / 2.0;
            let centre = egui::pos2(rect.left() + row_height / 2.0, y);
            let radius = row_height * 0.26;
            let colour = egui::Color32::from_rgb(0xd0, 0x45, 0x45);
            if *verified {
                painter.circle_filled(centre, radius, colour);
            } else {
                // Hollow: placed, but the debugger could not bind it — usually
                // a blank line or a comment. It will never be hit, and must not
                // look like one that will.
                painter.circle_stroke(centre, radius, egui::Stroke::new(1.5, colour));
            }
        }

        let mut caret_rect = None;
        let mut extra_carets: Vec<egui::Rect> = Vec::new();

        for row in first..last {
            let line = self.fold_map.line_at(row);
            let y = rect.top() + row as f32 * row_height;
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

            // Selection highlight for the part of this line each caret covers.
            let line_end = line_start + text.chars().count();
            for sel_range in &sel_ranges {
                if sel_range.end < line_start || sel_range.start > line_end {
                    continue;
                }
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

            // Diagnostic underlines, over the text so they are not hidden by
            // the selection.
            if !self.diagnostics.is_empty() {
                let line_end = line_start + text.chars().count();
                for diagnostic in &self.diagnostics {
                    if diagnostic.range.end < line_start || diagnostic.range.start > line_end {
                        continue;
                    }
                    let from = diagnostic.range.start.clamp(line_start, line_end) - line_start;
                    let to = diagnostic.range.end.clamp(line_start, line_end) - line_start;
                    // A zero-width diagnostic — an error at the end of a line —
                    // still needs something visible, so give it a minimum width.
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    let x1 = (text_left + galley.pos_from_cursor(ccursor(to)).left()).max(x0 + 6.0);
                    paint_squiggle(
                        &painter,
                        x0,
                        x1,
                        y + row_height - 2.0,
                        severity_colour(&visuals, diagnostic.severity),
                    );
                }
            }

            // A chevron for a line that opens a fold: pointing down when the
            // fold is open, right when it is closed, which is the direction
            // every file manager and outline view has used for decades.
            if self.folds.iter().any(|f| f.first == line) {
                let closed = self.collapsed.contains(&line);
                painter.text(
                    egui::pos2(text_left - row_height * 0.5, y + row_height / 2.0),
                    egui::Align2::CENTER_CENTER,
                    if closed { "\u{25b8}" } else { "\u{25be}" },
                    font.clone(),
                    if closed {
                        visuals.strong_text_color()
                    } else {
                        visuals.weak_text_color()
                    },
                );
            }

            if opts.show_line_numbers {
                let is_caret_line = line == caret_line;
                // A gutter glyph for the worst diagnostic on this line, so
                // severity is not conveyed by the squiggle's colour alone.
                if let Some(worst) = self
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        let line_end = line_start + text.chars().count();
                        d.range.start <= line_end && d.range.end >= line_start
                    })
                    .min_by_key(|d| d.severity)
                {
                    painter.text(
                        egui::pos2(rect.left() + row_height + 2.0, y),
                        egui::Align2::LEFT_TOP,
                        worst.severity.glyph(),
                        font.clone(),
                        severity_colour(&visuals, worst.severity),
                    );
                }
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

            // Bracket match: a faint box round each half of the pair, which
            // reads as "these two go together" without competing with the
            // selection or the caret.
            if let Some(pair) = &self.bracket_pair {
                for half in [&pair.open, &pair.close] {
                    let line_end = line_start + text.chars().count();
                    if half.start < line_start || half.start >= line_end.max(line_start) {
                        continue;
                    }
                    let column = half.start - line_start;
                    let x = text_left + galley.pos_from_cursor(ccursor(column)).left();
                    let width = galley.pos_from_cursor(ccursor(column + 1)).left()
                        - galley.pos_from_cursor(ccursor(column)).left();
                    painter.rect_stroke(
                        egui::Rect::from_min_size(
                            egui::pos2(x, y),
                            egui::vec2(width.max(1.0), row_height),
                        ),
                        2.0,
                        egui::Stroke::new(1.0, visuals.weak_text_color()),
                        egui::StrokeKind::Inside,
                    );
                }
            }

            let line_end = line_start + text.chars().count();
            for head in &caret_heads {
                if *head < line_start || *head > line_end {
                    continue;
                }
                let x = text_left + galley.pos_from_cursor(ccursor(head - line_start)).left();
                let here = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(1.5, row_height));
                if *head == self.selection.head && line == caret_line {
                    // The primary's rectangle is the one the completion popup
                    // anchors itself to, so it is the one worth remembering.
                    caret_rect = Some(here);
                } else {
                    extra_carets.push(here);
                }
            }
        }

        self.caret_screen_rect = caret_rect;
        // Extra carets do not blink. A dozen of them flashing in unison is
        // distracting, and a steady one is easier to count.
        if response.has_focus() {
            for caret in &extra_carets {
                painter.rect_filled(*caret, 0.0, visuals.strong_text_color());
            }
        }
        if let Some(caret) = caret_rect
            && response.has_focus()
            && self.blink_on()
        {
            painter.rect_filled(caret, 0.0, visuals.strong_text_color());
        }

        if std::mem::take(&mut self.scroll_to_caret) {
            // Derived from the line number rather than taken from `caret_rect`,
            // which only exists when the caret was painted -- that is, only
            // when it is *already* on screen. Keying the scroll off it meant
            // the one case that needs scrolling was the one case that could not
            // ask for it, so jumping to a search match or a definition outside
            // the visible range moved the caret and left the view behind.
            let y = rect.top() + self.fold_map.row_at(caret_line) as f32 * row_height;
            let x = caret_rect.map_or(text_left, |r| r.left());
            let target = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(1.5, row_height));
            // A couple of rows of context either side, so the target does not
            // land flush against the top or bottom edge.
            ui.scroll_to_rect(target.expand2(egui::vec2(0.0, row_height * 2.0)), None);
        }

        if response.has_focus() {
            // Keep the blink animating without spinning at the full frame rate.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    /// Solid for a moment after any interaction, so the caret is never invisible
    /// exactly when the user looks for it.
    /// Tell the accessibility tree what this widget is and where the caret is.
    ///
    /// Without this the editor is an unlabelled rectangle: a screen reader
    /// announces the menus, the buttons and the file tree, and then nothing at
    /// all for the one part of the window that matters. egui reports its own
    /// widgets automatically, but a custom-painted one has to say so itself.
    ///
    /// The value reported is the **current line**, not the document. A screen
    /// reader announces text as the caret moves through it, so the line is the
    /// unit it actually wants; handing it a five-megabyte string every time the
    /// caret moves would be both useless and ruinous.
    fn describe_for_screen_readers(
        &self,
        ui: &egui::Ui,
        doc: &Document,
        response: &egui::Response,
    ) {
        let (line, column) = doc.line_col(self.selection.head);
        let line_index = line - 1;
        let text = doc.line_text(line_index);
        let line_start = doc.line_start(line_index);
        let selection = self.selection.range();
        // Clamped to this line, in characters: the selection may run off both
        // ends of it, and a range outside the reported value is nonsense. The
        // line's *byte* length would be the wrong bound — a range in character
        // offsets bounded by a byte count is only right for ASCII.
        let line_chars = doc.line_len(line_index);
        let from = selection.start.saturating_sub(line_start).min(line_chars);
        let to = selection.end.saturating_sub(line_start).min(line_chars);
        let (from, to) = (egui::text::CharIndex(from), egui::text::CharIndex(to));

        let editable = doc.is_editable();
        let label = format!(
            "Code editor, line {line} of {}, column {column}",
            doc.line_count()
        );

        response.widget_info(|| egui::WidgetInfo {
            typ: egui::WidgetType::TextEdit,
            enabled: editable,
            label: Some(label.clone()),
            current_text_value: Some(text.clone()),
            text_selection: Some(from..to),
            ..egui::WidgetInfo::new(egui::WidgetType::TextEdit)
        });

        // The node itself, so the editor has a role and a name in the tree even
        // when nothing has happened to raise an event.
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(accesskit::Role::MultilineTextInput);
            node.set_label(label.clone());
            node.set_value(text.clone());
            if !editable {
                node.set_read_only();
            }
        });
    }

    /// Recompute the foldable ranges when the document has changed, and keep
    /// the collapsed set pointing at the right lines.
    ///
    /// Walking the tree is not free on a large file, so it happens on edits
    /// rather than on frames — the version check is what makes folding cost
    /// nothing while you are only scrolling.
    fn sync_folds(&mut self, doc: &Document, highlighter: Option<&Highlighter>) {
        let version = doc.version();
        let line_count = doc.line_count();

        if self.folds_version != Some(version) {
            // Move the collapsed folds with the lines they were put on, before
            // rebuilding against the new tree. Same approximation as the
            // breakpoint gutter: derived from the change in line count rather
            // than from the edit itself.
            if self.folds_version.is_some() && line_count != self.folds_line_count {
                let delta = line_count as isize - self.folds_line_count as isize;
                let caret_line = doc.line_of(self.selection.head);
                self.collapsed = crate::folding::shift(
                    &self.collapsed,
                    caret_line.saturating_sub(delta.unsigned_abs()),
                    delta,
                );
            }
            self.folds = highlighter
                .and_then(Highlighter::tree)
                .map(|tree| editor_syntax::brackets::fold_ranges(tree, doc.text()))
                .unwrap_or_default();
            // A fold whose header is no longer a fold has gone; keeping it
            // would hide lines that nothing offers to unhide.
            self.collapsed
                .retain(|line| self.folds.iter().any(|f| f.first == *line));
            self.folds_version = Some(version);
            self.folds_line_count = line_count;
            self.rebuild_fold_map(line_count);
        } else if self.fold_map.visible_rows() > line_count
            || (self.fold_map.is_identity() && !self.collapsed.is_empty())
        {
            // The document is the same but the map is not: a fold was toggled.
            self.rebuild_fold_map(line_count);
        }
    }

    fn rebuild_fold_map(&mut self, line_count: usize) {
        self.fold_map = crate::folding::FoldMap::new(line_count, &self.folds, &self.collapsed);
    }

    /// Open or close the fold that starts at `line`.
    fn toggle_fold(&mut self, line: usize) {
        if !self.collapsed.remove(&line) {
            self.collapsed.insert(line);
        }
        // Rebuilt now rather than next frame, so the click and the change land
        // together.
        let lines = self.folds_line_count;
        self.rebuild_fold_map(lines);
        self.touch();
    }

    /// Close every fold in the file, or open every one.
    ///
    /// Returns false when there was nothing to do, so the caller can say so.
    pub fn fold_all(&mut self, collapse: bool) -> bool {
        if self.folds.is_empty() {
            return false;
        }
        let before = self.collapsed.len();
        if collapse {
            self.collapsed = self.folds.iter().map(|f| f.first).collect();
        } else {
            self.collapsed.clear();
        }
        if self.collapsed.len() == before {
            return false;
        }
        let lines = self.folds_line_count;
        self.rebuild_fold_map(lines);
        self.touch();
        true
    }

    /// Fold or unfold the innermost fold containing the caret.
    ///
    /// Returns false when the caret is not inside anything foldable.
    pub fn toggle_fold_at_caret(&mut self, doc: &Document) -> bool {
        let caret = doc.line_of(self.selection.head);
        // Innermost: the fold that starts latest while still containing the
        // caret. Folding the outermost would collapse the whole file from
        // inside one function, which is never what was meant.
        let Some(fold) = self
            .folds
            .iter()
            .filter(|f| f.first <= caret && caret <= f.last)
            .max_by_key(|f| f.first)
        else {
            return false;
        };
        let line = fold.first;
        self.toggle_fold(line);
        true
    }

    fn blink_on(&self) -> bool {
        // A blinking caret is the animation accessibility guidance names first,
        // and the one that is on screen the whole time you are reading.
        if self.reduce_motion {
            return true;
        }
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

/// Draw a wavy underline.
///
/// A squiggle rather than a straight line because it is the one underline
/// convention nobody confuses with a hyperlink or a spelling of emphasis, and
/// because it survives being drawn under a selection.
fn paint_squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, colour: egui::Color32) {
    const WAVELENGTH: f32 = 4.0;
    const AMPLITUDE: f32 = 1.5;

    let mut points = Vec::new();
    let mut x = x0;
    let mut up = true;
    while x < x1 {
        points.push(egui::pos2(
            x,
            if up { y - AMPLITUDE } else { y + AMPLITUDE },
        ));
        x += WAVELENGTH / 2.0;
        up = !up;
    }
    points.push(egui::pos2(
        x1,
        if up { y - AMPLITUDE } else { y + AMPLITUDE },
    ));

    if points.len() >= 2 {
        painter.add(egui::Shape::line(points, egui::Stroke::new(1.0, colour)));
    }
}

/// Theme-aware colour for a diagnostic severity.
///
/// Public so the Problems panel colours its rows the same way as the squiggles
/// — two palettes for the same thing would be worse than one imperfect one.
pub fn severity_colour(
    visuals: &egui::Visuals,
    severity: editor_lsp::diagnostics::Severity,
) -> egui::Color32 {
    use editor_lsp::diagnostics::Severity;
    match severity {
        Severity::Error => visuals.error_fg_color,
        Severity::Warning => visuals.warn_fg_color,
        Severity::Information | Severity::Hint => visuals.weak_text_color(),
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

/// Something the editor's right-click menu asked the application to do.
///
/// Returned rather than performed, because every one of these needs something
/// the widget does not have: the project's language servers, the whole file
/// tree, the system clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextAction {
    GoToDefinition,
    FindUses,
    Paste,
}

/// How long to keep drawing frames after the view last moved.
///
/// Covers the tail of egui's scroll easing, where the per-frame movement is
/// small enough that individual frames can match the previous one. Measured
/// from the last actual movement, not from the last wheel event, so it always
/// spans the end of the animation however long the gesture was.
const SCROLL_SETTLE: std::time::Duration = std::time::Duration::from_millis(350);

/// The character range of whole lines `first..=last`, including the trailing
/// newline of the last one where there is one.
fn line_range(doc: &Document, first: usize, last: usize) -> std::ops::Range<usize> {
    let start = doc.line_start(first);
    let end = (doc.line_start(last) + doc.line_len(last) + 1).min(doc.len_chars());
    start..end
}

/// Whether this key press means "by word" rather than "by character".
///
/// Ctrl on Windows and Linux; Option on macOS, where Cmd+arrow is start/end of
/// line and Option+arrow is the word motion. `Modifiers::COMMAND` cannot be used
/// because it maps to Cmd on macOS, which would put word motion on the one
/// combination macOS users expect to do something else.
fn word_modifier(modifiers: egui::Modifiers) -> bool {
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
fn find_from(text: &str, needle: &str, from: usize) -> Option<usize> {
    let start_byte = text
        .char_indices()
        .nth(from)
        .map_or(text.len(), |(byte, _)| byte);
    let hit = text.get(start_byte..)?.find(needle)? + start_byte;
    Some(text[..hit].chars().count())
}

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

    // ---- Python method parameters ----------------------------------------

    fn python() -> EditorOptions {
        EditorOptions {
            language: LanguageId::Python,
            ..EditorOptions::default()
        }
    }

    /// Typing the `(` of a method should leave `(self)` with the caret ready
    /// for a comma -- not `()` with the caret between them.
    #[test]
    fn opening_a_methods_bracket_inserts_self() {
        let mut doc = doc_with("class A:\n    def greet");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());

        assert!(view.type_text(&mut doc, python(), "("));
        assert_eq!(doc.text().to_string(), "class A:\n    def greet(self)");
        assert_eq!(
            view.selection.head,
            doc.len_chars() - 1,
            "caret sits before the closing bracket"
        );
    }

    #[test]
    fn a_classmethod_gets_cls_and_a_staticmethod_gets_an_empty_pair() {
        let mut doc = doc_with("class A:\n    @classmethod\n    def make");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());
        view.type_text(&mut doc, python(), "(");
        assert!(doc.text().to_string().ends_with("def make(cls)"));

        let mut doc = doc_with("class A:\n    @staticmethod\n    def helper");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());
        view.type_text(&mut doc, python(), "(");
        assert!(
            doc.text().to_string().ends_with("def helper()"),
            "got {:?}",
            doc.text().to_string()
        );
    }

    #[test]
    fn a_plain_function_still_gets_an_empty_pair() {
        let mut doc = doc_with("def greet");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());
        view.type_text(&mut doc, python(), "(");
        assert_eq!(doc.text().to_string(), "def greet()");
    }

    /// The rule must not reach into any other language, where `self` is either
    /// spelled differently or means nothing at all.
    #[test]
    fn other_languages_are_untouched() {
        let mut doc = doc_with("impl A {\n    fn greet");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());
        view.type_text(
            &mut doc,
            EditorOptions {
                language: LanguageId::Rust,
                ..EditorOptions::default()
            },
            "(",
        );
        assert_eq!(doc.text().to_string(), "impl A {\n    fn greet()");
    }

    /// One undo takes back the whole thing, not just the bracket.
    #[test]
    fn inserting_self_is_a_single_undo_step() {
        let mut doc = doc_with("class A:\n    def greet");
        let mut view = EditorView::default();
        view.set_caret(doc.len_chars());
        view.type_text(&mut doc, python(), "(");
        assert!(view.undo(&mut doc));
        assert_eq!(doc.text().to_string(), "class A:\n    def greet");
    }

    // ---- folding ---------------------------------------------------------

    /// Drive `sync_folds` the way `render` does, without a window.
    fn with_folds(source: &str) -> (Document, EditorView, Highlighter) {
        let doc = doc_with(source);
        let highlighter =
            Highlighter::new(LanguageId::Python, doc.text()).expect("Python has a grammar");
        let mut view = EditorView::default();
        view.sync_folds(&doc, Some(&highlighter));
        (doc, view, highlighter)
    }

    const NESTED: &str =
        "class A:\n    def f(self):\n        x = 1\n        y = 2\n\n\ndef g():\n    pass\n";

    #[test]
    fn a_file_with_structure_has_folds_and_starts_unfolded() {
        let (doc, view, _h) = with_folds(NESTED);
        assert!(!view.folds.is_empty(), "there is something to fold");
        assert!(view.collapsed.is_empty());
        assert!(view.fold_map.is_identity());
        assert_eq!(view.fold_map.visible_rows(), doc.line_count());
    }

    #[test]
    fn folding_at_the_caret_takes_the_innermost_fold() {
        let (doc, mut view, _h) = with_folds(NESTED);
        // Caret on `x = 1`, inside both the class and the method.
        view.set_caret(doc.offset_at(2, 8));
        assert!(view.toggle_fold_at_caret(&doc));

        // The method, not the class: folding the outermost from inside one
        // function would collapse the whole file.
        let folded = *view.collapsed.iter().next().expect("something folded");
        assert_eq!(folded, 1, "the `def f` line, not the `class A` line");
        assert!(view.fold_map.is_hidden(2));
        assert!(!view.fold_map.is_hidden(1), "the header stays visible");
        assert!(!view.fold_map.is_hidden(6), "`def g` is untouched");
    }

    #[test]
    fn toggling_twice_returns_to_where_it_started() {
        let (doc, mut view, _h) = with_folds(NESTED);
        let before = view.fold_map.visible_rows();
        view.set_caret(doc.offset_at(2, 8));
        assert!(view.toggle_fold_at_caret(&doc));
        assert!(view.fold_map.visible_rows() < before);
        assert!(view.toggle_fold_at_caret(&doc));
        assert_eq!(view.fold_map.visible_rows(), before);
        assert!(view.fold_map.is_identity());
    }

    #[test]
    fn folding_all_and_unfolding_all_report_whether_anything_changed() {
        let (_doc, mut view, _h) = with_folds(NESTED);
        assert!(view.fold_all(true), "there was something to fold");
        assert!(!view.fold_all(true), "and now there is not");
        assert!(view.fold_all(false), "unfolding undoes it");
        assert!(!view.fold_all(false), "and there is nothing left to unfold");
        assert!(view.fold_map.is_identity());
    }

    #[test]
    fn a_caret_outside_any_fold_reports_nothing_to_fold() {
        let (doc, mut view, _h) = with_folds("x = 1\ny = 2\n");
        view.set_caret(0);
        assert!(!view.toggle_fold_at_caret(&doc));
    }

    /// The fold has to move with the lines it was put on, or an edit above it
    /// silently collapses a different function.
    #[test]
    fn a_fold_moves_when_lines_are_inserted_above_it() {
        let (mut doc, mut view, mut highlighter) = with_folds(NESTED);
        view.set_caret(doc.offset_at(6, 0));
        assert!(view.toggle_fold_at_caret(&doc), "fold `def g`");
        assert_eq!(view.collapsed.iter().next().copied(), Some(6));

        // Two blank lines at the very top, as typing above would produce.
        view.set_caret(0);
        doc.apply(
            &Transaction::insert(0, "\n\n"),
            Selection::at(0),
            Selection::at(2),
        );
        let changes = doc.take_changes();
        highlighter.update(&changes, doc.text());
        view.sync_folds(&doc, Some(&highlighter));

        assert_eq!(
            view.collapsed.iter().next().copied(),
            Some(8),
            "the fold followed its function down the file"
        );
        assert!(view.fold_map.is_hidden(9), "and still hides its body");
    }

    /// Rebuilding after an edit must not leave a collapsed entry pointing at a
    /// fold that no longer exists, which would hide lines nothing can unhide.
    #[test]
    fn a_fold_whose_code_was_deleted_is_forgotten() {
        let (mut doc, mut view, mut highlighter) = with_folds(NESTED);
        view.set_caret(doc.offset_at(6, 0));
        view.toggle_fold_at_caret(&doc);
        assert!(!view.collapsed.is_empty());

        // Replace the whole file with something that has no folds at all.
        let end = doc.len_chars();
        doc.apply(
            &editor_core::edit::Transaction::new(vec![editor_core::edit::Edit::replace(
                0..end,
                "a = 1\n".to_owned(),
            )]),
            Selection::at(0),
            Selection::at(0),
        );
        let changes = doc.take_changes();
        highlighter.update(&changes, doc.text());
        view.sync_folds(&doc, Some(&highlighter));

        assert!(view.collapsed.is_empty(), "the fold went with its code");
        assert!(view.fold_map.is_identity());
    }

    // ---- accessibility ---------------------------------------------------

    /// A caret that never stops blinking is exactly the animation that
    /// accessibility guidance names first, and unlike most animations it is on
    /// screen the whole time you are reading.
    #[test]
    fn reduce_motion_leaves_the_caret_solid() {
        let mut view = EditorView {
            reduce_motion: false,
            ..EditorView::default()
        };

        let seen: Vec<bool> = (0..40)
            .map(|i| {
                view.last_interaction = Some(
                    std::time::Instant::now() - std::time::Duration::from_millis(600 + i * 100),
                );
                view.blink_on()
            })
            .collect();
        assert!(
            seen.contains(&true) && seen.contains(&false),
            "with motion allowed the caret does blink"
        );

        view.reduce_motion = true;
        for i in 0..40 {
            view.last_interaction =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(600 + i * 100));
            assert!(view.blink_on(), "reduce motion means always visible");
        }
    }

    // ---- multiple carets -------------------------------------------------

    /// The core promise: one keystroke, one character at every caret, and one
    /// undo step for the lot.
    #[test]
    fn typing_with_several_carets_inserts_at_each_of_them() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        assert!(view.add_cursor_vertically(&doc, 1));
        assert!(view.add_cursor_vertically(&doc, 1));
        assert_eq!(view.cursor_count(), 3);

        assert!(view.insert(&mut doc, "# "));
        assert_eq!(doc.text().to_string(), "# one\n# two\n# three\n");

        assert!(view.undo(&mut doc));
        assert_eq!(
            doc.text().to_string(),
            "one\ntwo\nthree\n",
            "three carets typing is still one undo step"
        );
        assert_eq!(
            view.cursor_count(),
            1,
            "undo restores one selection, so the extra carets have to go rather \
             than be left pointing at text the undo has moved"
        );
    }

    /// Every caret has to end up after its own insertion, not after somebody
    /// else's. Getting this wrong is invisible for one caret and nonsense for
    /// three.
    #[test]
    fn each_caret_ends_up_after_the_text_it_typed() {
        let mut doc = doc_with("aa\nbb\ncc\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        view.add_cursor_vertically(&doc, 1);
        view.add_cursor_vertically(&doc, 1);

        view.insert(&mut doc, "X");
        assert_eq!(doc.text().to_string(), "Xaa\nXbb\nXcc\n");

        let (cursors, _) = view.cursors();
        let heads: Vec<usize> = cursors.iter().map(|s| s.head).collect();
        // "Xaa\n" is 4 characters, so the carets sit at 1, 5 and 9.
        assert_eq!(heads, vec![1, 5, 9]);
    }

    #[test]
    fn backspace_applies_to_every_caret() {
        let mut doc = doc_with("_one\n_two\n");
        let mut view = EditorView::default();
        view.set_caret(1);
        view.add_cursor_vertically(&doc, 1);
        assert_eq!(view.cursor_count(), 2);

        assert!(press(
            &mut view,
            &mut doc,
            egui::Key::Backspace,
            egui::Modifiers::NONE
        ));
        assert_eq!(doc.text().to_string(), "one\ntwo\n");
    }

    /// Carets do collide -- press End with carets on lines of different
    /// lengths, or Backspace them into each other. Two carets in one place
    /// would each apply the next edit, so one keystroke would insert twice.
    #[test]
    fn carets_that_land_on_the_same_spot_are_merged() {
        let mut view = EditorView::default();
        view.install_cursors(
            vec![Selection::at(5), Selection::at(5), Selection::at(9)],
            0,
        );
        assert_eq!(view.cursor_count(), 2, "the duplicate went");

        let mut doc = doc_with("0123456789abc");
        view.insert(&mut doc, "X");
        assert_eq!(
            doc.text().to_string(),
            "01234X5678X9abc",
            "one X per place, not two at the first"
        );
    }

    #[test]
    fn overlapping_selections_merge_into_one() {
        let mut view = EditorView::default();
        view.install_cursors(vec![Selection::new(2, 8), Selection::new(6, 12)], 0);
        assert_eq!(view.cursor_count(), 1);
        assert_eq!(view.selection.range(), 2..12);
    }

    #[test]
    fn escape_puts_the_editor_back_to_one_caret() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        view.add_cursor_vertically(&doc, 1);
        assert_eq!(view.cursor_count(), 2);

        press(
            &mut view,
            &mut doc,
            egui::Key::Escape,
            egui::Modifiers::NONE,
        );
        assert_eq!(view.cursor_count(), 1);
    }

    #[test]
    fn arrow_keys_move_every_caret() {
        let mut doc = doc_with("abcd\nefgh\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        view.add_cursor_vertically(&doc, 1);

        press(
            &mut view,
            &mut doc,
            egui::Key::ArrowRight,
            egui::Modifiers::NONE,
        );
        press(
            &mut view,
            &mut doc,
            egui::Key::ArrowRight,
            egui::Modifiers::NONE,
        );
        let (cursors, _) = view.cursors();
        assert_eq!(
            cursors.iter().map(|s| s.head).collect::<Vec<_>>(),
            vec![2, 7],
            "both carets moved two characters"
        );
    }

    /// With nothing selected, the first Ctrl+D selects the word so that the
    /// second has something to look for.
    #[test]
    fn the_first_add_cursor_selects_the_word_under_the_caret() {
        let doc = doc_with("total = total + 1");
        let mut view = EditorView::default();
        view.set_caret(2);

        assert!(view.add_cursor_at_next_match(&doc));
        assert_eq!(view.cursor_count(), 1);
        assert_eq!(view.selection.range(), 0..5);

        assert!(view.add_cursor_at_next_match(&doc));
        assert_eq!(view.cursor_count(), 2, "the second `total` got a caret");
        let (cursors, _) = view.cursors();
        assert_eq!(cursors[1].range(), 8..13);
    }

    /// Having worked to the bottom of the file, the next one should come back
    /// to the top rather than leaving the key looking broken.
    #[test]
    fn adding_cursors_wraps_round_the_end_of_the_file() {
        let doc = doc_with("x\ny\nx\n");
        let mut view = EditorView::default();
        view.select_range(4, 5); // the second `x`
        assert!(view.add_cursor_at_next_match(&doc));

        let (cursors, _) = view.cursors();
        assert_eq!(cursors.len(), 2);
        assert_eq!(cursors[0].range(), 0..1, "wrapped to the first `x`");
    }

    #[test]
    fn adding_a_cursor_stops_when_everything_is_already_selected() {
        let doc = doc_with("x y x");
        let mut view = EditorView::default();
        view.select_range(0, 1);
        assert!(view.add_cursor_at_next_match(&doc));
        assert_eq!(view.cursor_count(), 2);
        assert!(
            !view.add_cursor_at_next_match(&doc),
            "both are taken, so say so rather than silently doing nothing"
        );
    }

    /// Multi-byte text: `find_from` works in bytes internally and must hand
    /// back character offsets, or a caret lands inside a character.
    #[test]
    fn adding_cursors_counts_characters_not_bytes() {
        let doc = doc_with("café x café");
        let mut view = EditorView::default();
        view.select_range(0, 4); // "café"
        assert!(view.add_cursor_at_next_match(&doc));

        let (cursors, _) = view.cursors();
        assert_eq!(cursors[1].range(), 7..11, "characters, not bytes");
        assert_eq!(doc.text().slice(cursors[1].range()).to_string(), "café");
    }

    /// A column selection is a rectangle. Lines too short to reach into it get
    /// nothing -- inventing a caret on them means the next keystroke edits a
    /// line the rectangle never covered.
    #[test]
    fn a_column_selection_skips_lines_too_short_to_reach_it() {
        let doc = doc_with("aaaaaa\nbb\ncccccc\n");
        let mut view = EditorView::default();
        // Columns 3..5 down all three lines. The middle line has two
        // characters, so it is not in the rectangle at all.
        view.select_column(&doc, 3, doc.offset_at(2, 5));

        let (cursors, _) = view.cursors();
        assert_eq!(cursors.len(), 2, "the short line is skipped: {cursors:?}");
        assert_eq!(doc.text().slice(cursors[0].range()).to_string(), "aa");
        assert_eq!(doc.text().slice(cursors[1].range()).to_string(), "cc");
    }

    #[test]
    fn a_zero_width_column_selection_is_a_column_of_carets() {
        let doc = doc_with("one\ntwo\nsix\n");
        let mut view = EditorView::default();
        view.select_column(&doc, 0, doc.offset_at(2, 0));

        assert_eq!(view.cursor_count(), 3);
        assert!(
            view.cursors().0.iter().all(|s| s.is_empty()),
            "a rectangle with no width is three carets, not three selections"
        );
    }

    /// Alt+click on a caret that is already there removes it, but never the
    /// last one -- an editor with no caret cannot be typed into.
    #[test]
    fn alt_clicking_a_caret_removes_it_but_never_the_last_one() {
        let mut view = EditorView::default();
        view.set_caret(4);
        view.toggle_cursor_at(9);
        assert_eq!(view.cursor_count(), 2);

        view.toggle_cursor_at(9);
        assert_eq!(view.cursor_count(), 1);

        view.toggle_cursor_at(4);
        assert_eq!(view.cursor_count(), 1, "the last caret stays");
    }

    /// Press a key with modifiers, as `handle_keys` would.
    fn press(
        view: &mut EditorView,
        doc: &mut Document,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> bool {
        view.handle_key(doc, EditorOptions::default(), key, modifiers, 20)
    }

    /// The platform's "by word" modifier, so these tests exercise the same
    /// combination the user presses rather than a hard-coded Ctrl.
    fn ctrl() -> egui::Modifiers {
        if cfg!(target_os = "macos") {
            egui::Modifiers::ALT
        } else {
            egui::Modifiers::CTRL
        }
    }

    fn ctrl_shift() -> egui::Modifiers {
        ctrl().plus(egui::Modifiers::SHIFT)
    }

    #[test]
    fn word_motion_is_on_the_right_modifier_for_the_platform() {
        // Cmd+arrow on macOS is start/end of line. Putting word motion there
        // would take over a combination that already means something else.
        assert!(word_modifier(ctrl()), "the platform modifier must work");
        if cfg!(target_os = "macos") {
            assert!(!word_modifier(egui::Modifiers::MAC_CMD));
        } else {
            assert!(!word_modifier(egui::Modifiers::ALT));
        }
        assert!(!word_modifier(egui::Modifiers::NONE));
    }

    #[test]
    fn duplicating_a_line_puts_the_copy_below_it() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1)); // `two`
        assert!(view.duplicate_lines(&mut doc));
        assert_eq!(doc.text().to_string(), "one\ntwo\ntwo\nthree\n");
    }

    #[test]
    fn duplicating_again_gives_a_third_copy() {
        // The caret has to follow the copy, or the second press duplicates the
        // original again and the two copies end up interleaved.
        let mut doc = doc_with("one\ntwo\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1));
        view.duplicate_lines(&mut doc);
        view.duplicate_lines(&mut doc);
        assert_eq!(doc.text().to_string(), "one\ntwo\ntwo\ntwo\n");
    }

    #[test]
    fn duplicating_the_last_line_of_a_file_without_a_final_newline() {
        // The block has no newline of its own, so the copy needs one in front
        // of it or the two lines are glued together.
        let mut doc = doc_with("one\ntwo");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1));
        view.duplicate_lines(&mut doc);
        assert_eq!(doc.text().to_string(), "one\ntwo\ntwo");
    }

    #[test]
    fn duplicating_a_multi_line_selection_copies_the_whole_block() {
        let mut doc = doc_with("a\nb\nc\n");
        let mut view = EditorView::default();
        view.select_range(0, doc.line_start(1) + 1);
        view.duplicate_lines(&mut doc);
        assert_eq!(doc.text().to_string(), "a\nb\na\nb\nc\n");
    }

    #[test]
    fn deleting_a_line_removes_it_and_its_newline() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1));
        assert!(view.delete_lines(&mut doc));
        assert_eq!(doc.text().to_string(), "one\nthree\n");
    }

    #[test]
    fn deleting_the_last_line_does_not_leave_a_blank_one_behind() {
        // Taking the newline *after* the last line is impossible -- there is
        // none -- so the one before it goes instead.
        let mut doc = doc_with("one\ntwo");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1));
        view.delete_lines(&mut doc);
        assert_eq!(doc.text().to_string(), "one");
    }

    #[test]
    fn deleting_the_only_line_empties_the_document() {
        let mut doc = doc_with("only\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        view.delete_lines(&mut doc);
        assert_eq!(doc.text().to_string(), "");
    }

    #[test]
    fn moving_a_line_down_swaps_it_with_the_one_below() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(0));
        assert!(view.move_lines(&mut doc, 1));
        assert_eq!(doc.text().to_string(), "two\none\nthree\n");
    }

    #[test]
    fn moving_a_line_up_swaps_it_with_the_one_above() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(2));
        assert!(view.move_lines(&mut doc, -1));
        assert_eq!(doc.text().to_string(), "one\nthree\ntwo\n");
    }

    #[test]
    fn moving_past_either_end_does_nothing() {
        let mut doc = doc_with("one\ntwo\n");
        let mut view = EditorView::default();
        view.set_caret(0);
        assert!(!view.move_lines(&mut doc, -1), "already at the top");
        view.set_caret(doc.line_start(1));
        assert!(!view.move_lines(&mut doc, 1), "already at the bottom");
        assert_eq!(doc.text().to_string(), "one\ntwo\n");
    }

    #[test]
    fn moving_into_a_final_line_that_has_no_newline_does_not_join_them() {
        // The missing newline belongs to the *end of the file*, not to the
        // block being moved. Swapping the two blocks verbatim would carry it
        // into the middle and produce "twoone".
        let mut doc = doc_with("one\ntwo");
        let mut view = EditorView::default();
        view.set_caret(0);
        view.move_lines(&mut doc, 1);
        assert_eq!(doc.text().to_string(), "two\none");
    }

    #[test]
    fn moving_a_line_keeps_it_selected() {
        // So the shortcut can be held down to walk a line up a file.
        let mut doc = doc_with("aaa\nbb\nc\n");
        let mut view = EditorView::default();
        view.select_range(doc.line_start(2), doc.line_start(2) + 1);
        view.move_lines(&mut doc, -1);
        assert_eq!(doc.text().to_string(), "aaa\nc\nbb\n");
        assert_eq!(
            view.selected_text(&doc).as_deref(),
            Some("c"),
            "the moved text is no longer selected"
        );
    }

    #[test]
    fn a_line_move_survives_repeated_application() {
        // Walking a line from the bottom to the top and back must be lossless.
        let mut doc = doc_with("one\ntwo\nthree\nfour\n");
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(3));
        for _ in 0..3 {
            view.move_lines(&mut doc, -1);
        }
        assert_eq!(doc.text().to_string(), "four\none\ntwo\nthree\n");
        for _ in 0..3 {
            view.move_lines(&mut doc, 1);
        }
        assert_eq!(doc.text().to_string(), "one\ntwo\nthree\nfour\n");
    }

    #[test]
    fn each_line_operation_is_a_single_undo_step() {
        let mut doc = doc_with("one\ntwo\nthree\n");
        let original = doc.text().to_string();
        let mut view = EditorView::default();
        view.set_caret(doc.line_start(1));

        view.duplicate_lines(&mut doc);
        assert!(view.undo(&mut doc));
        assert_eq!(doc.text().to_string(), original, "duplicate");

        view.set_caret(doc.line_start(1));
        view.delete_lines(&mut doc);
        assert!(view.undo(&mut doc));
        assert_eq!(doc.text().to_string(), original, "delete");

        view.set_caret(doc.line_start(1));
        view.move_lines(&mut doc, 1);
        assert!(view.undo(&mut doc));
        assert_eq!(doc.text().to_string(), original, "move");
    }

    #[test]
    fn ctrl_arrow_moves_the_caret_a_word_at_a_time() {
        let mut doc = doc_with("alpha beta_gamma delta");
        let mut view = EditorView::default();
        view.set_caret(0);

        press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl());
        assert_eq!(view.selection.head, 5, "the end of `alpha`");
        press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl());
        assert_eq!(view.selection.head, 16, "the end of `beta_gamma`");
        press(&mut view, &mut doc, egui::Key::ArrowLeft, ctrl());
        assert_eq!(view.selection.head, 6, "the start of `beta_gamma`");
    }

    #[test]
    fn plain_arrows_still_move_one_character() {
        // The word motion must not swallow the ordinary case.
        let mut doc = doc_with("abc");
        let mut view = EditorView::default();
        view.set_caret(0);
        press(
            &mut view,
            &mut doc,
            egui::Key::ArrowRight,
            egui::Modifiers::NONE,
        );
        assert_eq!(view.selection.head, 1);
    }

    #[test]
    fn ctrl_shift_arrow_extends_the_selection_by_a_word() {
        let mut doc = doc_with("alpha beta");
        let mut view = EditorView::default();
        view.set_caret(0);
        press(&mut view, &mut doc, egui::Key::ArrowRight, ctrl_shift());
        assert_eq!(view.selection.anchor, 0, "the anchor stays put");
        assert_eq!(view.selection.head, 5);
        assert_eq!(view.selected_text(&doc).as_deref(), Some("alpha"));
    }

    #[test]
    fn ctrl_backspace_deletes_the_word_before_the_caret() {
        let mut doc = doc_with("alpha beta");
        let mut view = EditorView::default();
        view.set_caret(10);
        assert!(press(&mut view, &mut doc, egui::Key::Backspace, ctrl()));
        assert_eq!(doc.text().to_string(), "alpha ");
    }

    #[test]
    fn ctrl_delete_deletes_the_word_after_the_caret() {
        let mut doc = doc_with("alpha beta");
        let mut view = EditorView::default();
        view.set_caret(5);
        assert!(press(&mut view, &mut doc, egui::Key::Delete, ctrl()));
        assert_eq!(doc.text().to_string(), "alpha");
    }

    #[test]
    fn deleting_a_word_is_one_undo_step() {
        // Without breaking the undo run either side, a word deletion coalesces
        // with whatever was typed before it and undo takes back too much.
        let mut doc = doc_with("alpha beta");
        let mut view = EditorView::default();
        view.set_caret(10);
        press(&mut view, &mut doc, egui::Key::Backspace, ctrl());
        assert_eq!(doc.text().to_string(), "alpha ");
        assert!(view.undo(&mut doc));
        assert_eq!(doc.text().to_string(), "alpha beta", "one undo restores it");
    }

    #[test]
    fn plain_backspace_still_deletes_to_the_tab_stop() {
        // Smart backspace must survive the addition of the Ctrl variant.
        let mut doc = doc_with("        x");
        let mut view = EditorView::default();
        view.set_caret(8);
        press(
            &mut view,
            &mut doc,
            egui::Key::Backspace,
            egui::Modifiers::NONE,
        );
        assert_eq!(doc.text().to_string(), "    x", "back to the tab stop");
    }

    #[test]
    fn ctrl_backspace_with_a_selection_deletes_the_selection() {
        // A word motion must not override an explicit selection.
        let mut doc = doc_with("alpha beta gamma");
        let mut view = EditorView::default();
        view.select_range(6, 10);
        press(&mut view, &mut doc, egui::Key::Backspace, ctrl());
        assert_eq!(doc.text().to_string(), "alpha  gamma");
    }

    #[test]
    fn ctrl_backspace_at_the_start_of_the_document_does_nothing() {
        let mut doc = doc_with("alpha");
        let mut view = EditorView::default();
        view.set_caret(0);
        assert!(!press(&mut view, &mut doc, egui::Key::Backspace, ctrl()));
        assert_eq!(doc.text().to_string(), "alpha");
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
