//! Which lines are hidden by a collapsed fold, and where the rest are drawn.
//!
//! The editor's painter assumes visual row *n* is document line *n*, and every
//! part of it does — the current-line stripe, the breakpoint gutter, the paint
//! loop, the caret, scroll-to-caret, and turning a click back into an offset.
//! Folding breaks that assumption exactly once, here, so that everything else
//! can go on being arithmetic.
//!
//! The mapping is the identity whenever nothing is collapsed, which is almost
//! always. That is not only an optimisation: it means the ordinary path is the
//! same code it was before folding existed, so the common case cannot be
//! broken by a bug in the folded case.

use std::collections::BTreeSet;

use editor_syntax::brackets::FoldRange;

/// The visible rows of a document, and which line each one shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FoldMap {
    /// Document line for each visual row.
    ///
    /// Empty when nothing is folded, which stands for the identity mapping
    /// rather than for an empty document.
    rows: Vec<usize>,
    line_count: usize,
}

impl FoldMap {
    /// A map with nothing hidden.
    pub(crate) fn identity(line_count: usize) -> Self {
        Self {
            rows: Vec::new(),
            line_count,
        }
    }

    /// Hide the body of every collapsed fold.
    ///
    /// `collapsed` holds the *first* line of each fold that is closed. A fold
    /// hides the lines after its first, never the first itself: the point of a
    /// collapsed function is that you can still see which function it is.
    pub(crate) fn new(line_count: usize, folds: &[FoldRange], collapsed: &BTreeSet<usize>) -> Self {
        if collapsed.is_empty() || line_count == 0 {
            return Self::identity(line_count);
        }

        // Mark hidden lines rather than walking ranges per line: folds nest,
        // and a line hidden by an outer fold must stay hidden whatever the
        // inner ones say.
        let mut hidden = vec![false; line_count];
        for fold in folds {
            if !collapsed.contains(&fold.first) {
                continue;
            }
            let last = fold.last.min(line_count.saturating_sub(1));
            if let Some(body) = hidden.get_mut((fold.first + 1)..=last) {
                body.fill(true);
            }
        }

        let rows: Vec<usize> = (0..line_count).filter(|line| !hidden[*line]).collect();
        // Nothing was actually hidden -- every collapsed entry named a fold
        // that no longer exists, say. Fall back to the identity, so the rest of
        // the painter takes its cheap path.
        if rows.len() == line_count {
            return Self::identity(line_count);
        }
        Self { rows, line_count }
    }

    /// True when no line is hidden.
    pub(crate) fn is_identity(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many rows the document occupies on screen.
    pub(crate) fn visible_rows(&self) -> usize {
        if self.is_identity() {
            self.line_count
        } else {
            self.rows.len()
        }
    }

    /// How many rows are worth painting in a document of `line_count` lines.
    ///
    /// Ordinarily every one of them, but the map is rebuilt at the top of a
    /// frame and a keystroke handled later in that same frame changes the
    /// document immediately. Backspacing a selection that spans lines leaves
    /// this map describing rows the document no longer has, and [`Self::line_at`]
    /// clamps to the map's own line count rather than the document's — so the
    /// paint loop asked the rope for a line past its end, which is a panic
    /// rather than a blank row.
    pub(crate) fn rows_within(&self, line_count: usize) -> usize {
        if self.is_identity() {
            return self.line_count.min(line_count);
        }
        // `rows` is ascending, so this is the first row showing a line the
        // document no longer has.
        self.rows.partition_point(|line| *line < line_count)
    }

    /// The document line drawn at `row`.
    ///
    /// Clamped rather than optional: this is called from click handling, where
    /// a pointer below the last row is an ordinary thing to happen and means
    /// "the end".
    pub(crate) fn line_at(&self, row: usize) -> usize {
        if self.is_identity() {
            return row.min(self.line_count.saturating_sub(1));
        }
        let index = row.min(self.rows.len().saturating_sub(1));
        self.rows.get(index).copied().unwrap_or(0)
    }

    /// The row `line` is drawn at.
    ///
    /// A hidden line reports the row of the fold that hides it, which is what
    /// makes scrolling to a caret inside a collapsed region put the collapsed
    /// header on screen instead of scrolling to nothing.
    pub(crate) fn row_at(&self, line: usize) -> usize {
        if self.is_identity() {
            return line.min(self.line_count.saturating_sub(1));
        }
        match self.rows.binary_search(&line) {
            Ok(row) => row,
            // Not visible: the row before the insertion point is the nearest
            // visible line above it, which is its fold's header.
            Err(insertion) => insertion.saturating_sub(1),
        }
    }

    /// True when `line` is inside a collapsed fold.
    pub(crate) fn is_hidden(&self, line: usize) -> bool {
        !self.is_identity() && self.rows.binary_search(&line).is_err()
    }
}

/// Move collapsed folds with the lines they were put on.
///
/// The same approximation the breakpoint gutter uses, and for the same reason:
/// tracking exactly would need every edit's range, and a fold that is one line
/// out after a multi-cursor paste is a far smaller problem than one that
/// silently collapses the wrong function.
///
/// `after` is the line the edit happened at — the caret's — so only folds below
/// it move.
pub(crate) fn shift(collapsed: &BTreeSet<usize>, after: usize, delta: isize) -> BTreeSet<usize> {
    if delta == 0 {
        return collapsed.clone();
    }
    collapsed
        .iter()
        .filter_map(|line| {
            if *line <= after {
                return Some(*line);
            }
            // A fold whose header was deleted goes with it.
            line.checked_add_signed(delta)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(first: usize, last: usize) -> FoldRange {
        FoldRange { first, last }
    }

    fn collapsed(lines: &[usize]) -> BTreeSet<usize> {
        lines.iter().copied().collect()
    }

    /// The path taken on nearly every frame: nothing folded, so row and line
    /// are the same number and the painter's arithmetic is untouched.
    #[test]
    fn nothing_folded_is_the_identity() {
        let map = FoldMap::new(10, &[fold(2, 5)], &collapsed(&[]));
        assert!(map.is_identity());
        assert_eq!(map.visible_rows(), 10);
        for line in 0..10 {
            assert_eq!(map.row_at(line), line);
            assert_eq!(map.line_at(line), line);
            assert!(!map.is_hidden(line));
        }
    }

    /// A collapsed fold hides its body but not its first line: the point of a
    /// folded function is still being able to see which function it is.
    #[test]
    fn a_collapsed_fold_hides_its_body_but_keeps_its_header() {
        // Lines 0..9, with 2..=5 folded away.
        let map = FoldMap::new(10, &[fold(2, 5)], &collapsed(&[2]));
        assert!(!map.is_identity());
        // Lines 3, 4 and 5 go; the header on line 2 stays.
        assert_eq!(map.visible_rows(), 10 - 3);

        assert!(!map.is_hidden(2), "the header stays");
        for line in 3..=5 {
            assert!(map.is_hidden(line), "line {line} should be hidden");
        }
        assert!(!map.is_hidden(6));

        // Rows read 0,1,2,6,7,8,9.
        assert_eq!(map.line_at(0), 0);
        assert_eq!(map.line_at(2), 2);
        assert_eq!(map.line_at(3), 6, "the row after the fold is line 6");
        assert_eq!(map.line_at(5), 8);
    }

    /// The inverse has to agree with the forward map, or the caret is painted
    /// somewhere other than where a click at that point would land.
    #[test]
    fn rows_and_lines_are_inverses_for_every_visible_line() {
        let map = FoldMap::new(20, &[fold(3, 7), fold(12, 15)], &collapsed(&[3, 12]));
        for row in 0..map.visible_rows() {
            let line = map.line_at(row);
            assert_eq!(map.row_at(line), row, "row {row} -> line {line} -> ?");
        }
    }

    /// A caret inside a collapsed region has to scroll *somewhere*, and the
    /// header is the only sensible answer.
    #[test]
    fn a_hidden_line_reports_the_row_of_the_fold_hiding_it() {
        let map = FoldMap::new(10, &[fold(2, 5)], &collapsed(&[2]));
        let header_row = map.row_at(2);
        for line in 3..=5 {
            assert_eq!(map.row_at(line), header_row, "line {line}");
        }
    }

    /// Folds nest. Collapsing the outer one must hide the inner one whole,
    /// whether or not the inner one is itself collapsed.
    #[test]
    fn an_outer_fold_hides_an_inner_one_entirely() {
        let folds = [fold(1, 9), fold(3, 5)];
        let outer_only = FoldMap::new(12, &folds, &collapsed(&[1]));
        for line in 2..=9 {
            assert!(outer_only.is_hidden(line), "line {line}");
        }
        assert_eq!(outer_only.visible_rows(), 12 - 8);

        // And with both collapsed the answer is the same, not doubly hidden.
        let both = FoldMap::new(12, &folds, &collapsed(&[1, 3]));
        assert_eq!(both, outer_only);
    }

    /// Two folds side by side each hide their own body.
    #[test]
    fn sibling_folds_are_independent() {
        let map = FoldMap::new(12, &[fold(1, 3), fold(6, 9)], &collapsed(&[6]));
        assert!(!map.is_hidden(2), "the uncollapsed fold is untouched");
        assert!(map.is_hidden(7));
        assert_eq!(map.visible_rows(), 12 - 3);
    }

    /// An entry naming a fold that no longer exists — the code under it was
    /// deleted, say — must not hide anything.
    #[test]
    fn a_collapsed_entry_with_no_matching_fold_hides_nothing() {
        let map = FoldMap::new(10, &[fold(2, 5)], &collapsed(&[7]));
        assert!(map.is_identity());
        assert_eq!(map.visible_rows(), 10);
    }

    /// A fold running past the end of a shrunken document must clamp rather
    /// than index out of range.
    #[test]
    fn a_fold_past_the_end_of_the_document_is_clamped() {
        let map = FoldMap::new(5, &[fold(1, 99)], &collapsed(&[1]));
        assert_eq!(map.visible_rows(), 2, "lines 0 and 1 remain");
        assert_eq!(map.line_at(1), 1);
    }

    /// Clicking below the last row is ordinary, and means "the end".
    #[test]
    fn a_row_past_the_end_clamps_to_the_last_visible_line() {
        let map = FoldMap::new(10, &[fold(2, 5)], &collapsed(&[2]));
        assert_eq!(map.line_at(999), 9);
        let plain = FoldMap::identity(10);
        assert_eq!(plain.line_at(999), 9);
    }

    #[test]
    fn an_empty_document_does_not_panic() {
        let map = FoldMap::new(0, &[], &collapsed(&[]));
        assert_eq!(map.visible_rows(), 0);
        assert_eq!(map.line_at(0), 0);
        assert_eq!(map.row_at(0), 0);
    }

    // ---- shifting --------------------------------------------------------

    #[test]
    fn inserting_above_a_fold_moves_it_down() {
        let got = shift(&collapsed(&[10, 20]), 5, 2);
        assert_eq!(got, collapsed(&[12, 22]));
    }

    #[test]
    fn a_fold_above_the_edit_does_not_move() {
        let got = shift(&collapsed(&[3, 10]), 5, 2);
        assert_eq!(got, collapsed(&[3, 12]));
    }

    #[test]
    fn deleting_lines_pulls_later_folds_up() {
        let got = shift(&collapsed(&[10, 20]), 2, -4);
        assert_eq!(got, collapsed(&[6, 16]));
    }

    /// A fold whose header was deleted goes with it rather than wrapping round
    /// to the top of the file.
    #[test]
    fn a_fold_deleted_off_the_top_is_dropped() {
        let got = shift(&collapsed(&[3]), 0, -10);
        assert!(got.is_empty());
    }

    #[test]
    fn no_change_means_no_movement() {
        let before = collapsed(&[1, 2, 3]);
        assert_eq!(shift(&before, 0, 0), before);
    }

    /// A keystroke is applied to the document in the same frame that paints
    /// it, but the row map was built before the keystroke. After deleting
    /// lines the map outlives them by a frame, and the painter has to stop at
    /// the end of the document rather than at the end of the map.
    #[test]
    fn rows_are_capped_by_the_document_not_by_the_map() {
        let map = FoldMap::identity(13);
        assert_eq!(map.visible_rows(), 13);
        assert_eq!(map.rows_within(11), 11, "two lines have just been deleted");
        assert_eq!(map.rows_within(13), 13, "and nothing is capped otherwise");
        assert_eq!(map.rows_within(20), 13, "a longer document adds no rows");
    }

    /// Same again with a fold in the way, where rows and lines differ.
    #[test]
    fn a_folded_map_is_capped_by_the_document_too() {
        let folds = [FoldRange { first: 1, last: 3 }];
        let collapsed = BTreeSet::from([1]);
        let map = FoldMap::new(10, &folds, &collapsed);
        // Lines 2 and 3 are hidden, so the visible lines are 0,1,4,5,6,7,8,9.
        assert_eq!(map.visible_rows(), 8);
        assert_eq!(map.rows_within(10), 8);
        assert_eq!(
            map.rows_within(5),
            3,
            "rows showing lines 0, 1 and 4 survive a document cut to five lines"
        );
        assert_eq!(map.rows_within(0), 0);
    }
}
