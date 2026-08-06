//! Cursors and selections.
//!
//! Offsets are **character** indices into the rope, never bytes. Mixing the two
//! is the classic way to panic an editor on the first non-ASCII file, so the
//! byte representation never escapes `ropey`.
//!
//! M2 ships a single cursor. The type is shaped for multi-cursor from the
//! start — a selection is an anchor and a head, not a sorted range — so M4 adds
//! cursors rather than rewriting this.

use std::ops::Range;

/// A cursor, possibly with a selection behind it.
///
/// `anchor` is where the selection started; `head` is where the cursor is now
/// and where typing inserts. `head < anchor` for a backwards selection, which
/// is why this is not just a `Range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    /// A collapsed cursor at `offset`.
    #[must_use]
    pub const fn at(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    #[must_use]
    pub const fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// The selected span, always low-to-high.
    #[must_use]
    pub fn range(self) -> Range<usize> {
        if self.anchor <= self.head {
            self.anchor..self.head
        } else {
            self.head..self.anchor
        }
    }

    #[must_use]
    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    #[must_use]
    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    /// True when nothing is selected — the caret is a thin line, not a block.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    #[must_use]
    pub fn len(self) -> usize {
        self.end() - self.start()
    }

    /// Collapse to the head, discarding the selection.
    #[must_use]
    pub const fn collapsed(self) -> Self {
        Self::at(self.head)
    }

    /// Move the head, keeping the anchor — what shift+arrow does.
    #[must_use]
    pub const fn extended_to(self, head: usize) -> Self {
        Self {
            anchor: self.anchor,
            head,
        }
    }

    /// Clamp both ends into a document of `len` characters.
    ///
    /// Called after every edit: a selection pointing past the end of the buffer
    /// is how an editor panics on undo.
    #[must_use]
    pub fn clamped(self, len: usize) -> Self {
        Self {
            anchor: self.anchor.min(len),
            head: self.head.min(len),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backwards_selection_still_yields_a_low_to_high_range() {
        let forwards = Selection::new(3, 9);
        let backwards = Selection::new(9, 3);
        assert_eq!(forwards.range(), 3..9);
        assert_eq!(
            backwards.range(),
            3..9,
            "selecting right-to-left must produce the same span"
        );
        assert_eq!(forwards.len(), backwards.len());
    }

    #[test]
    fn direction_is_preserved_so_shift_arrow_can_shrink_a_selection() {
        let sel = Selection::new(9, 3);
        assert_eq!(sel.head, 3, "the head is where the caret is drawn");
        let shrunk = sel.extended_to(5);
        assert_eq!(shrunk.range(), 5..9);
    }

    #[test]
    fn an_empty_selection_is_a_caret() {
        let caret = Selection::at(7);
        assert!(caret.is_empty());
        assert_eq!(caret.len(), 0);
        assert_eq!(caret.range(), 7..7);
    }

    #[test]
    fn collapsing_keeps_the_head_not_the_anchor() {
        assert_eq!(Selection::new(2, 8).collapsed(), Selection::at(8));
        assert_eq!(Selection::new(8, 2).collapsed(), Selection::at(2));
    }

    #[test]
    fn clamping_keeps_selections_inside_a_shrunken_document() {
        let sel = Selection::new(4, 20).clamped(10);
        assert_eq!(sel, Selection::new(4, 10));
        assert!(sel.end() <= 10, "an out-of-range selection panics on undo");
    }
}
