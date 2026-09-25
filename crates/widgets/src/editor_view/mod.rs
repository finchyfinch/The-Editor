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
//! This file holds the view's state, its public face and the frame that ties
//! the rest together (`render`). The rest is split by what it is about, each
//! file adding methods to `EditorView`:
//!
//! - `gutter` — where the gutter's columns are, and what a pointer over them
//!   means. Painting and hit-testing both read it, so they cannot disagree.
//! - `input` — pointer and keyboard, and caret motion.
//! - `editing` — typing, deleting and the line commands.
//! - `paint` — the rows on screen, the gutter, the sticky band, and what a
//!   screen reader is told.
//! - `structure` — folds and declarations, from the parse tree.

use editor_core::document::Document;
use editor_core::edit::Transaction;
use editor_core::selection::Selection;
use editor_core::word;
use editor_syntax::LanguageId;
use editor_syntax::docstring;
use editor_syntax::highlight::Highlighter;
use editor_syntax::indent::{self, IndentOptions};
use editor_syntax::methods;
use editor_syntax::theme::SyntaxTheme;
use editor_vcs::diff::LineStatus;
use eframe::egui;

mod editing;
mod gutter;
mod input;
mod paint;
mod structure;
#[cfg(test)]
mod tests;

pub use self::paint::{change_colour, severity_colour};
use self::{editing::*, gutter::*, input::*};

/// Rows painted above and below the viewport, so a fast scroll never exposes a
/// blank band before the next frame lands.
const OVERSCAN_ROWS: usize = 4;
/// Caret blink period.
const BLINK_MS: u128 = 530;
/// How long the pointer must sit still before a hover is worth asking about.
///
/// Long enough that crossing a line does not ask about every word on it, short
/// enough that stopping on a name and waiting feels like the editor answering
/// rather than thinking.
const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// The pointer resting over the text.
#[derive(Debug, Clone, Copy)]
struct Resting {
    /// Character offset under the pointer.
    offset: usize,
    /// Where on screen, so a popup can be put beside it.
    at: egui::Pos2,
    /// When it arrived here.
    since: std::time::Instant,
}

/// What the pointer is over, left to right across the view.
///
/// One description of the geometry, shared by the click handler and the
/// pointer shape. They were separate arithmetic to begin with, which is the
/// arrangement where the cursor promises a click that does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    /// Blame and the change bar: something to read, not something to act on.
    Annotation,
    /// The breakpoint strip.
    Breakpoints,
    /// The line numbers.
    Numbers,
    /// The fold chevrons, at the right-hand edge of the gutter.
    Folds,
    /// The code itself.
    Text,
}

/// One frame's worth of pointer gesture, lifted out of an [`egui::Response`].
///
/// A plain record rather than the response itself, so the rules about what a
/// click or a drag does to the selection can be exercised without a window.
#[derive(Debug, Clone, Copy, Default)]
struct Gesture {
    clicked: bool,
    double_clicked: bool,
    dragged: bool,
    /// First frame of a drag. The one that has to plant a fresh anchor, since
    /// the press before it left the old selection untouched.
    drag_started: bool,
    alt: bool,
    shift: bool,
}

/// Something the pointer has settled on, for the application to describe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hovered {
    pub offset: usize,
    /// Where to put the popup.
    pub at: egui::Pos2,
}

/// How wide the change bar is drawn.
///
/// Narrow on purpose. It is a signal in peripheral vision, not something to be
/// read; anything wider competes with the code for attention it does not need.
const CHANGE_BAR_WIDTH: f32 = 3.0;
/// The change column: the bar, plus the gap that keeps it off the breakpoints.
const CHANGE_COLUMN: f32 = CHANGE_BAR_WIDTH + 2.0;

/// How many characters of blame annotation to show.
///
/// Enough for a date and a first name, which is what makes a line's history
/// recognisable at a glance. Longer names are cut rather than allowed to push
/// the code sideways — the hover has the whole thing.
const BLAME_COLUMNS: usize = 20;

/// How many declarations the sticky header will pin at once.
///
/// The header costs viewport, so it cannot be allowed to grow with the nesting.
/// Four covers a method inside a class inside a module with one to spare, and
/// past that the innermost rows are kept: the function you are in is the one
/// you have lost track of, and the module three levels out is the one you can
/// still guess.
const STICKY_MAX_ROWS: usize = 4;

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
    /// Write a docstring skeleton when `"""` is typed under a `def` or
    /// `class`, in this layout. `None` leaves the quotes alone.
    pub docstrings: Option<docstring::Style>,
    /// Stop the caret blinking and other repeating animation.
    pub reduce_motion: bool,
    /// Pin the declarations enclosing the top of the viewport to the top of the
    /// editor, so the `def` or `class` you are inside stays legible after you
    /// have scrolled past it.
    pub sticky_scopes: bool,
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
            docstrings: Some(docstring::Style::Google),
            reduce_motion: false,
            sticky_scopes: true,
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
    /// Every declaration in the file and the lines it covers, for the sticky
    /// header. Read off the same tree as `folds`, at the same moments, and so
    /// governed by the same staleness rules.
    scopes: Vec<editor_syntax::symbols::Outline>,
    /// Document version `folds` was computed from, so the tree is walked on
    /// edits rather than on frames.
    folds_version: Option<u64>,
    /// Line count as of the last rebuild, so collapsed folds can be moved with
    /// the lines they were put on.
    folds_line_count: usize,
    /// Whether `folds` was read off a tree that had not finished reparsing.
    ///
    /// Tracked because catching up does not change the document version, so
    /// the version check alone would never look at the tree again.
    folds_stale: bool,
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
    /// How this buffer differs from the committed version, supplied by the
    /// application each frame. Sorted by line, one entry per changed line.
    changes: Vec<(usize, LineStatus)>,
    /// Where the pointer is resting over the text, and for how long.
    ///
    /// The offset alone is not enough: a hover has to wait for the pointer to
    /// settle, or moving across a line asks about every word on it.
    resting: Option<Resting>,
    /// Who last touched each line, by zero-based line number. Empty when the
    /// annotations are switched off, which is what decides whether the column
    /// exists at all.
    blame: Vec<(usize, String)>,
    /// How wide the blame column came out this frame. Held rather than passed,
    /// because everything else in the gutter is positioned relative to it and
    /// threading it through four more parameters buys nothing.
    blame_width: f32,
    /// The gutter's columns as laid out this frame.
    gutter: Gutter,
    /// The widest line laid out so far, in pixels, and the font size that
    /// measurement was made at. The scroll area is made this wide, so a long
    /// line can be scrolled to its end.
    widest_text: f32,
    widest_font_size: f32,
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
    /// Where the sticky header's rows were drawn last frame, and the line each
    /// one shows.
    ///
    /// Kept because only the paint pass knows the band's geometry, and input is
    /// handled before it. Last frame's rectangles are the right ones to test a
    /// click against in any case: they are what was on screen when the button
    /// went down.
    sticky_hits: Vec<(egui::Rect, usize)>,
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

    /// How this buffer differs from the committed version, for the gutter.
    ///
    /// Supplied per frame, like the diagnostics and for the same reason: this
    /// is a fact about the file and the repository, not about the view.
    /// An empty list means no repository, no baseline yet, or no changes —
    /// which all draw the same, because an empty gutter is what each of them
    /// honestly looks like.
    pub fn set_changes(&mut self, changes: Vec<(usize, LineStatus)>) {
        self.changes = changes;
    }

    /// Who last touched each line, by zero-based line number.
    ///
    /// An empty list switches the column off entirely rather than drawing an
    /// empty one: a blank strip beside the code is worse than no strip.
    pub fn set_blame(&mut self, blame: Vec<(usize, String)>) {
        self.blame = blame;
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
        self.sync_tree_data(doc, highlighter.as_deref());
        // Rows, not lines, from here on. The two are the same number unless
        // something is folded.
        let row_count = self.fold_map.visible_rows();

        // The blame column, at the very left, and only when there is blame to
        // put in it. Then the change bars, in a narrow column of their own
        // rather than a stripe over the breakpoint dots: a bar behind a dot is
        // a bar you cannot see. Then one glyph column for breakpoints and the
        // diagnostic marker -- its own column, because a breakpoint drawn over
        // the digits was invisible against them. Then the numbers, then the
        // fold chevrons, whose column is reserved even in a file with nothing
        // to fold so the text does not shift sideways as you type.
        self.blame_width = if self.blame.is_empty() {
            0.0
        } else {
            space_width * BLAME_COLUMNS as f32 + 10.0
        };
        self.gutter = Gutter::new(
            self.blame_width,
            row_height,
            space_width,
            line_count,
            opts.show_line_numbers,
        );
        let gutter_width = self.gutter.width();

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
                //
                // At least the height of the viewport, for the same reason the
                // width is at least its width: the blank space under a short
                // file is part of the editor, and clicking it means "put the
                // caret at the end", the way it does in every other editor.
                // Allocated to exactly the text's height, that space belonged
                // to the scroll area's background instead -- the click landed
                // on nothing, the caret did not move, and the editor did not
                // even take focus, so the next thing typed went nowhere.
                //
                // The rows past the end of the text are only ever hit-tested,
                // never painted: the paint loop's last row is capped by the
                // document, not by this rectangle.
                // As wide as the widest line seen so far, and never narrower
                // than a hundred and twenty columns. Fixed at those hundred and
                // twenty, the end of any longer line could not be scrolled to,
                // and typing there put the caret off the edge of the window.
                //
                // "Seen so far" because measuring every line of the file on
                // every edit would cost what virtualised rendering exists to
                // save. Every painted row is measured anyway, so the width
                // grows as long lines come into view, a frame after they do.
                if self.widest_font_size != opts.font_size {
                    self.widest_text = 0.0;
                    self.widest_font_size = opts.font_size;
                }
                let widest = (120.0 * space_width).max(self.widest_text + 4.0 * space_width);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(
                        (gutter_width + widest).max(ui.available_width()),
                        (row_height * row_count as f32).max(ui.available_height()),
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

                let text_left = rect.left() + gutter_width;

                // The pointer says what a click here would do. Mid-drag it
                // says nothing new: the drag began in the text and is still a
                // selection however far into the gutter it wanders.
                let icon = if response.dragged() {
                    egui::CursorIcon::Text
                } else {
                    response.hover_pos().map_or(egui::CursorIcon::Text, |pos| {
                        self.cursor_icon(doc, pos, rect, row_height)
                    })
                };
                let response = response.on_hover_cursor(icon);
                self.describe_for_screen_readers(ui, doc, &response);
                self.track_pointer(ui, doc, &response, &font, rect, text_left, row_height);
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
