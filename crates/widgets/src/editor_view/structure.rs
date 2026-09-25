//! What the parse tree says about the file's shape: the fold ranges and the
//! declarations the sticky band pins, recomputed when the text changes rather
//! than when the view scrolls.

use super::*;

impl EditorView {
    /// Recompute the foldable ranges when the document has changed, and keep
    /// the collapsed set pointing at the right lines.
    ///
    /// Walking the tree is not free on a large file, so it happens on edits
    /// rather than on frames — the version check is what makes folding cost
    /// nothing while you are only scrolling.
    pub(super) fn sync_tree_data(&mut self, doc: &Document, highlighter: Option<&Highlighter>) {
        let version = doc.version();
        let line_count = doc.line_count();

        // A reparse that ran out of time leaves a tree whose extents have been
        // stretched to fit the edit but whose structure is the one from before
        // it, and folds read off that sit on lines that open nothing. The
        // catch-up reparse repairs the tree without touching the document, so
        // the version check alone would never look at it again — the folds
        // would stay wrong until the next keystroke.
        let tree_stale = highlighter.is_some_and(Highlighter::is_stale);

        if self.folds_need_rebuild(version, tree_stale) {
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
            let tree = highlighter.and_then(Highlighter::tree);
            self.folds = tree
                .map(|tree| editor_syntax::brackets::fold_ranges(tree, doc.text()))
                .unwrap_or_default();
            // Walked here rather than per frame for the same reason the folds
            // are: it is a whole-tree walk, the answer only changes when the
            // tree does, and the sticky header asks for it on every paint.
            self.scopes = tree
                .map(|tree| editor_syntax::symbols::outline(tree, doc.text()))
                .unwrap_or_default();
            // A fold whose header is no longer a fold has gone; keeping it
            // would hide lines that nothing offers to unhide.
            self.collapsed
                .retain(|line| self.folds.iter().any(|f| f.first == *line));
            self.folds_version = Some(version);
            self.folds_line_count = line_count;
            self.folds_stale = tree_stale;
            self.rebuild_fold_map(line_count);
        } else if self.fold_map.visible_rows() > line_count
            || (self.fold_map.is_identity() && !self.collapsed.is_empty())
        {
            // The document is the same but the map is not: a fold was toggled.
            self.rebuild_fold_map(line_count);
        }
    }

    /// The lines to pin at the top of the viewport, outermost first.
    ///
    /// The declarations `top_line` is inside, minus any whose own header is
    /// still on screen -- pinning a copy of a row the reader can already see is
    /// how a sticky header ends up showing the same line twice.
    ///
    /// Note what is *not* here: the caret. The question this answers is "what
    /// am I looking at", which is a property of the scroll position, and a
    /// header driven by the caret would sit unchanged while the file scrolled
    /// underneath it.
    pub(super) fn sticky_lines(&self, top_line: usize, line_count: usize) -> Vec<usize> {
        let mut lines: Vec<usize> = editor_syntax::symbols::enclosing(&self.scopes, top_line)
            .iter()
            .map(|item| item.first_line)
            // Capped by the document for the same reason the paint loop's last
            // row is: the scopes were walked at the top of the frame and a
            // keystroke handled later in it applies to `doc` at once, so after
            // deleting a selection that spanned lines these can name lines the
            // document no longer has.
            .filter(|first| {
                *first < top_line && *first < line_count && !self.fold_map.is_hidden(*first)
            })
            .collect();
        // Two declarations can share a first line -- a Rust `fn` written on one
        // line inside a one-line `mod`, say -- and pinning it twice would be
        // one wasted row showing the same text.
        lines.dedup();
        if lines.len() > STICKY_MAX_ROWS {
            lines.drain(..lines.len() - STICKY_MAX_ROWS);
        }
        lines
    }

    /// The line the sticky header shows at `pos`, if the header is under it.
    ///
    /// From the rectangles the last paint recorded, which is what the reader
    /// was looking at when they moved the pointer there.
    pub(super) fn sticky_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.sticky_hits
            .iter()
            .find(|(rect, _)| rect.contains(pos))
            .map(|(_, line)| *line)
    }

    /// Whether the fold list has to be walked out of the tree again.
    ///
    /// A separate predicate because the second half of it is the easy thing to
    /// forget: the document is the only obvious input, and the tree changing
    /// under a document that did not is exactly the case that went unnoticed.
    pub(super) fn folds_need_rebuild(&self, version: u64, tree_stale: bool) -> bool {
        self.folds_version != Some(version) || (self.folds_stale && !tree_stale)
    }

    pub(super) fn rebuild_fold_map(&mut self, line_count: usize) {
        self.fold_map = crate::folding::FoldMap::new(line_count, &self.folds, &self.collapsed);
    }

    /// Open or close the fold that starts at `line`.
    pub(super) fn toggle_fold(&mut self, line: usize) {
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
}
