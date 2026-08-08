//! Transactions: the only way text ever changes.
//!
//! Every mutation is a [`Transaction`] applied through [`apply`], which returns
//! both the resulting [`Change`]s and the [`Transaction`] that reverses them.
//! Nothing else may touch the rope. That single choke point is what keeps the
//! undo history, the parse tree (M3) and the language server's document version
//! (M6) from ever drifting apart — see PLAN.md §2.3.
//!
//! Offsets are character indices, matching [`crate::selection`].

use std::ops::Range;

use ropey::Rope;

/// Replace `range` with `text`. An empty range is an insertion; empty text is a
/// deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    #[must_use]
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self {
            range: at..at,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn delete(range: Range<usize>) -> Self {
        Self {
            range,
            text: String::new(),
        }
    }

    #[must_use]
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }

    /// True for a plain insertion with no newline — the shape of ordinary
    /// typing, and the only shape the undo history coalesces.
    #[must_use]
    pub fn is_simple_insert(&self) -> bool {
        self.range.is_empty() && !self.text.is_empty() && !self.text.contains('\n')
    }

    /// True for a plain deletion — the shape of holding down backspace.
    #[must_use]
    pub fn is_simple_delete(&self) -> bool {
        !self.range.is_empty() && self.text.is_empty()
    }
}

/// One or more edits applied as a unit. Multi-edit transactions arrive with
/// multi-cursor in M4; the machinery is here already.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transaction {
    pub edits: Vec<Edit>,
}

impl Transaction {
    #[must_use]
    pub fn new(edits: Vec<Edit>) -> Self {
        Self { edits }
    }

    #[must_use]
    pub fn single(edit: Edit) -> Self {
        Self { edits: vec![edit] }
    }

    #[must_use]
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self::single(Edit::insert(at, text))
    }

    #[must_use]
    pub fn delete(range: Range<usize>) -> Self {
        Self::single(Edit::delete(range))
    }

    #[must_use]
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self::single(Edit::replace(range, text))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The single edit, when there is exactly one.
    #[must_use]
    pub fn as_single(&self) -> Option<&Edit> {
        match self.edits.as_slice() {
            [edit] => Some(edit),
            _ => None,
        }
    }
}

/// What one edit actually did, for anyone tracking the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The range in the *pre-edit* document that was replaced.
    pub range: Range<usize>,
    pub removed: String,
    pub inserted: String,
}

/// The result of applying a transaction.
#[derive(Debug, Clone)]
pub struct Applied {
    pub changes: Vec<Change>,
    /// Applying this to the post-edit rope restores the pre-edit rope exactly.
    pub inverse: Transaction,
}

/// Apply a transaction to a rope.
///
/// All edit ranges are interpreted in the *pre-edit* document. Out-of-range
/// edits are clamped rather than panicking — a stale selection must not be able
/// to crash the editor. Overlapping edits are not supported and produce an
/// unspecified (but non-panicking) result.
///
/// Two different coordinate spaces are in play here, and conflating them is the
/// easy mistake:
///
/// * The edits themselves are applied **right to left**, so that rewriting a
///   later range cannot invalidate the offsets of an earlier one.
/// * The returned inverse has to be applied to the document as it exists
///   *after* this call, so each inverse range is shifted by the net length
///   change of every edit before it. Recording the inverse in pre-edit
///   coordinates makes single-edit undo look correct while quietly corrupting
///   any multi-edit undo.
pub fn apply(rope: &mut Rope, tx: &Transaction) -> Applied {
    let len = rope.len_chars();
    let mut ordered: Vec<Edit> = tx
        .edits
        .iter()
        .map(|e| {
            let start = e.range.start.min(len);
            let end = e.range.end.clamp(start, len);
            Edit {
                range: start..end,
                text: e.text.clone(),
            }
        })
        .collect();
    ordered.sort_by_key(|e| e.range.start);

    // Capture the replaced text while the rope is still in pre-edit shape.
    let removed: Vec<String> = ordered
        .iter()
        .map(|e| rope.slice(e.range.clone()).to_string())
        .collect();

    for edit in ordered.iter().rev() {
        if !edit.range.is_empty() {
            rope.remove(edit.range.clone());
        }
        if !edit.text.is_empty() {
            rope.insert(edit.range.start, &edit.text);
        }
    }

    let mut changes = Vec::with_capacity(ordered.len());
    let mut inverse = Vec::with_capacity(ordered.len());
    let mut shift: isize = 0;

    for (edit, was) in ordered.iter().zip(&removed) {
        let inserted_len = edit.text.chars().count();
        let removed_len = was.chars().count();

        // Where this edit's inserted text now sits in the post-edit document.
        let start = edit.range.start.saturating_add_signed(shift);
        inverse.push(Edit::replace(start..start + inserted_len, was.clone()));

        changes.push(Change {
            range: edit.range.clone(),
            removed: was.clone(),
            inserted: edit.text.clone(),
        });

        shift += inserted_len as isize - removed_len as isize;
    }

    Applied {
        changes,
        inverse: Transaction::new(inverse),
    }
}

/// Where `offset` ends up once `tx` has been applied.
///
/// Multi-cursor needs this: after one transaction has inserted a character at
/// each of eight carets, all eight carets are in the wrong place, and each is
/// wrong by a different amount. Every edit before a caret moves it by that
/// edit's net length change.
///
/// A caret *inside* an edited range lands at the end of the replacement. There
/// is no better answer — the text it pointed into is gone — and the end is
/// where typing over a selection leaves the caret, which is what makes it feel
/// right.
///
/// `tx` is interpreted in pre-edit coordinates, exactly as [`apply`] does.
#[must_use]
pub fn remap(tx: &Transaction, offset: usize) -> usize {
    let mut ordered: Vec<&Edit> = tx.edits.iter().collect();
    ordered.sort_by_key(|e| e.range.start);

    let mut shift: isize = 0;
    for edit in ordered {
        let inserted = edit.text.chars().count();
        let removed = edit.range.len();
        if edit.range.end <= offset {
            shift += inserted as isize - removed as isize;
        } else if edit.range.start < offset {
            // Inside this edit: the character it referred to no longer exists.
            return edit.range.start.saturating_add_signed(shift) + inserted;
        } else {
            break; // Sorted, so nothing later can affect this offset.
        }
    }
    offset.saturating_add_signed(shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// The property that matters, stated as a property: remapping an offset
    /// must agree with actually applying the transaction and looking.
    #[test]
    fn remapping_agrees_with_applying_the_edits() {
        // Eight carets each typing a character, as Ctrl+D then a keystroke does.
        let text = "one two one two one";
        let tx = Transaction::new(
            [0usize, 4, 8, 12, 16]
                .iter()
                .map(|at| Edit::insert(*at, "X"))
                .collect(),
        );
        let mut r = rope(text);
        apply(&mut r, &tx);
        assert_eq!(r.to_string(), "Xone Xtwo Xone Xtwo Xone");

        // Each caret ends up just after the character it typed, having also
        // been pushed right by every caret before it.
        for (i, at) in [0usize, 4, 8, 12, 16].iter().enumerate() {
            assert_eq!(
                remap(&tx, *at),
                at + i + 1,
                "the caret at {at} is after its own X and {i} earlier ones"
            );
        }
    }

    #[test]
    fn an_offset_before_every_edit_does_not_move() {
        let tx = Transaction::new(vec![Edit::insert(10, "abc"), Edit::insert(20, "de")]);
        assert_eq!(remap(&tx, 0), 0);
        // An insertion exactly *at* the offset carries it along, which is what
        // puts the caret after what it just typed rather than before it.
        assert_eq!(remap(&tx, 10), 13);
    }

    #[test]
    fn deletions_pull_later_offsets_back() {
        let tx = Transaction::new(vec![Edit::delete(2..5), Edit::delete(10..12)]);
        assert_eq!(remap(&tx, 1), 1, "before everything");
        assert_eq!(remap(&tx, 5), 2, "after the first deletion");
        assert_eq!(remap(&tx, 12), 12 - 3 - 2);
    }

    /// A caret pointing into text that has just been replaced has nowhere
    /// exact to go. It must land somewhere sensible rather than out of range.
    #[test]
    fn an_offset_inside_a_replaced_range_lands_at_the_end_of_the_replacement() {
        let tx = Transaction::single(Edit::replace(4..9, "XY"));
        assert_eq!(remap(&tx, 6), 4 + 2);
        assert_eq!(remap(&tx, 4), 4, "the very start is not inside");
        assert_eq!(remap(&tx, 9), 9 - 5 + 2, "the very end is after");
    }

    /// Undo and redo apply transactions too, and a caret must not end up past
    /// the end of the buffer.
    #[test]
    fn remapping_never_runs_past_what_the_edits_can_justify() {
        let tx = Transaction::single(Edit::delete(0..100));
        assert_eq!(remap(&tx, 3), 0, "everything before it is gone");
        assert_eq!(remap(&tx, 500), 400);
    }

    #[test]
    fn insert_puts_text_where_asked() {
        let mut r = rope("hello world");
        apply(&mut r, &Transaction::insert(5, ","));
        assert_eq!(r.to_string(), "hello, world");
    }

    #[test]
    fn delete_removes_the_range() {
        let mut r = rope("hello, world");
        apply(&mut r, &Transaction::delete(5..7));
        assert_eq!(r.to_string(), "helloworld");
    }

    #[test]
    fn replace_swaps_the_range() {
        let mut r = rope("hello world");
        apply(&mut r, &Transaction::replace(6..11, "there"));
        assert_eq!(r.to_string(), "hello there");
    }

    #[test]
    fn the_inverse_restores_the_original_exactly() {
        for (text, tx) in [
            ("hello world", Transaction::insert(5, ", dear")),
            ("hello world", Transaction::delete(0..6)),
            ("hello world", Transaction::replace(0..5, "goodbye")),
            ("", Transaction::insert(0, "from empty")),
            ("all of it", Transaction::delete(0..9)),
        ] {
            let mut r = rope(text);
            let applied = apply(&mut r, &tx);
            apply(&mut r, &applied.inverse);
            assert_eq!(r.to_string(), text, "round trip failed for {tx:?}");
        }
    }

    #[test]
    fn multiple_edits_do_not_invalidate_each_others_offsets() {
        // Both offsets refer to the ORIGINAL text. Applying left-to-right
        // naively would shift the second edit; this is the bug the
        // right-to-left ordering exists to prevent.
        let mut r = rope("aaa bbb ccc");
        let tx = Transaction::new(vec![
            Edit::replace(0..3, "XXXXXX"),
            Edit::replace(8..11, "Y"),
        ]);
        apply(&mut r, &tx);
        assert_eq!(r.to_string(), "XXXXXX bbb Y");
    }

    /// Regression: the inverse used to be recorded in pre-edit coordinates.
    /// Single-edit undo looked fine, so this only shows up once a transaction
    /// carries two edits whose lengths differ — i.e. the first time multi-cursor
    /// or a project-wide replace is undone.
    #[test]
    fn a_multi_edit_transaction_inverts_as_a_unit() {
        let original = "aaa bbb ccc";
        let mut r = rope(original);
        let applied = apply(
            &mut r,
            &Transaction::new(vec![
                Edit::replace(0..3, "XXXXXX"),
                Edit::replace(8..11, "Y"),
            ]),
        );
        assert_eq!(r.to_string(), "XXXXXX bbb Y");
        apply(&mut r, &applied.inverse);
        assert_eq!(r.to_string(), original);
    }

    #[test]
    fn many_edits_of_differing_lengths_all_invert_correctly() {
        // Growing, shrinking, pure inserts and pure deletes in one transaction,
        // so every sign of length delta contributes to the running shift.
        let original = "one two three four five";
        let mut r = rope(original);
        let applied = apply(
            &mut r,
            &Transaction::new(vec![
                Edit::replace(0..3, "1"),              // shrinks by 2
                Edit::insert(4, "!!!"),                // grows by 3
                Edit::delete(8..13),                   // shrinks by 5
                Edit::replace(14..18, "FOURFOURFOUR"), // grows by 8
            ]),
        );
        let after = r.to_string();
        assert_ne!(after, original);

        apply(&mut r, &applied.inverse);
        assert_eq!(
            r.to_string(),
            original,
            "the inverse must account for the cumulative shift of every earlier edit"
        );
    }

    #[test]
    fn an_empty_transaction_changes_nothing() {
        let mut r = rope("unchanged");
        let applied = apply(&mut r, &Transaction::default());
        assert_eq!(r.to_string(), "unchanged");
        assert!(applied.changes.is_empty());
        assert!(applied.inverse.is_empty());
    }

    #[test]
    fn out_of_range_edits_are_clamped_instead_of_panicking() {
        let mut r = rope("short");
        apply(&mut r, &Transaction::insert(9999, "!"));
        assert_eq!(r.to_string(), "short!");

        let mut r = rope("short");
        apply(&mut r, &Transaction::delete(3..9999));
        assert_eq!(r.to_string(), "sho");
    }

    #[test]
    fn works_on_multibyte_text_because_offsets_are_chars_not_bytes() {
        let mut r = rope("caf\u{e9} na\u{ef}ve");
        // Char index 4 is the space, not a byte offset into the e-acute.
        apply(&mut r, &Transaction::insert(4, "\u{2014}"));
        assert_eq!(r.to_string(), "caf\u{e9}\u{2014} na\u{ef}ve");

        let mut r = rope("\u{1f600}\u{1f601}\u{1f602}");
        apply(&mut r, &Transaction::delete(1..2));
        assert_eq!(r.to_string(), "\u{1f600}\u{1f602}");
    }

    #[test]
    fn changes_report_what_was_removed_and_inserted() {
        let mut r = rope("hello world");
        let applied = apply(&mut r, &Transaction::replace(0..5, "goodbye"));
        let change = applied.changes.first().expect("one change");
        assert_eq!(change.removed, "hello");
        assert_eq!(change.inserted, "goodbye");
        assert_eq!(change.range, 0..5);
    }

    #[test]
    fn edit_shapes_are_classified_for_undo_coalescing() {
        assert!(Edit::insert(0, "a").is_simple_insert());
        assert!(
            !Edit::insert(0, "a\nb").is_simple_insert(),
            "a newline is a boundary"
        );
        assert!(!Edit::insert(0, "").is_simple_insert());
        assert!(Edit::delete(0..1).is_simple_delete());
        assert!(!Edit::replace(0..1, "x").is_simple_delete());
    }
}
