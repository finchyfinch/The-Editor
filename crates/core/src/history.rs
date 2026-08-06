//! Undo and redo.
//!
//! Each entry stores the transaction that redoes the change, the one that
//! undoes it, and the selection either side — so undo restores the cursor to
//! where the edit happened, not wherever the caret drifted to since.
//!
//! Consecutive typing is coalesced into one entry. Without that, undo steps one
//! character at a time and is useless. The boundaries are the ones people
//! expect: a pause, a newline, a cursor jump, a save, or any command that is
//! not plain typing.

use std::time::{Duration, Instant};

use crate::edit::{Edit, Transaction};
use crate::selection::Selection;

/// Typing separated by more than this starts a new undo entry.
const COALESCE_WINDOW: Duration = Duration::from_millis(300);

/// One undoable step.
#[derive(Debug, Clone)]
struct Revision {
    redo: Transaction,
    undo: Transaction,
    before: Selection,
    after: Selection,
}

/// What an undo or redo produced: apply the transaction, then set the cursor.
#[derive(Debug, Clone)]
pub struct Step {
    pub transaction: Transaction,
    pub selection: Selection,
}

/// The undo stack.
#[derive(Debug, Default)]
pub struct History {
    undo_stack: Vec<Revision>,
    redo_stack: Vec<Revision>,
    last_edit_at: Option<Instant>,
    /// Set by [`History::break_run`]; prevents the next edit merging with the
    /// previous one.
    barrier: bool,
}

impl History {
    /// Record an applied edit.
    ///
    /// `redo` is what was applied, `undo` is its inverse (from
    /// [`crate::edit::Applied`]).
    pub fn push(
        &mut self,
        redo: Transaction,
        undo: Transaction,
        before: Selection,
        after: Selection,
    ) {
        // Any new edit invalidates the redo branch.
        self.redo_stack.clear();

        let now = Instant::now();
        let within_window = self
            .last_edit_at
            .is_some_and(|t| now.duration_since(t) < COALESCE_WINDOW);

        if !self.barrier
            && within_window
            && let Some(previous) = self.undo_stack.last_mut()
            && merge(previous, &redo, &undo, after)
        {
            self.last_edit_at = Some(now);
            return;
        }

        self.undo_stack.push(Revision {
            redo,
            undo,
            before,
            after,
        });
        self.last_edit_at = Some(now);
        self.barrier = false;
    }

    /// Force the next edit to start a new undo entry.
    ///
    /// Called on save, on a cursor jump, and by any command that is not plain
    /// typing — so undo after "delete line" does not also swallow the word
    /// typed a moment earlier.
    pub fn break_run(&mut self) {
        self.barrier = true;
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Take the next undo step, if any.
    pub fn undo(&mut self) -> Option<Step> {
        let revision = self.undo_stack.pop()?;
        let step = Step {
            transaction: revision.undo.clone(),
            selection: revision.before,
        };
        self.redo_stack.push(revision);
        self.barrier = true;
        self.last_edit_at = None;
        Some(step)
    }

    /// Take the next redo step, if any.
    pub fn redo(&mut self) -> Option<Step> {
        let revision = self.redo_stack.pop()?;
        let step = Step {
            transaction: revision.redo.clone(),
            selection: revision.after,
        };
        self.undo_stack.push(revision);
        self.barrier = true;
        self.last_edit_at = None;
        Some(step)
    }

    /// Number of undo entries, for tests and diagnostics.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.undo_stack.len()
    }
}

/// Try to fold a new edit into the previous revision. Returns whether it merged.
///
/// Only the two shapes that actually matter are merged: a run of typed
/// characters, and a run of backspaces. Anything else gets its own entry, which
/// is the conservative and predictable choice.
fn merge(
    previous: &mut Revision,
    redo: &Transaction,
    undo: &Transaction,
    after: Selection,
) -> bool {
    let (Some(prev_redo), Some(new_redo)) = (previous.redo.as_single(), redo.as_single()) else {
        return false;
    };

    // A run of typing: each insert lands exactly where the last one ended.
    if prev_redo.is_simple_insert() && new_redo.is_simple_insert() {
        let prev_end = prev_redo.range.start + prev_redo.text.chars().count();
        if new_redo.range.start != prev_end {
            return false;
        }
        let combined = format!("{}{}", prev_redo.text, new_redo.text);
        let start = prev_redo.range.start;
        let len = combined.chars().count();
        previous.redo = Transaction::single(Edit::insert(start, combined));
        previous.undo = Transaction::single(Edit::delete(start..start + len));
        previous.after = after;
        return true;
    }

    // A run of backspaces: each deletion ends where the last one began.
    if prev_redo.is_simple_delete()
        && new_redo.is_simple_delete()
        && new_redo.range.end == prev_redo.range.start
    {
        let (Some(prev_undo), Some(new_undo)) = (previous.undo.as_single(), undo.as_single())
        else {
            return false;
        };
        let combined_removed = format!("{}{}", new_undo.text, prev_undo.text);
        let start = new_redo.range.start;
        let end = prev_redo.range.end;
        previous.redo = Transaction::single(Edit::delete(start..end));
        previous.undo = Transaction::single(Edit::insert(start, combined_removed));
        previous.after = after;
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{self, Transaction};
    use ropey::Rope;

    /// Drive a rope through the history the way the editor does, so the tests
    /// exercise the real interaction rather than the bookkeeping alone.
    struct Harness {
        rope: Rope,
        history: History,
        selection: Selection,
    }

    impl Harness {
        fn new(text: &str) -> Self {
            Self {
                rope: Rope::from_str(text),
                history: History::default(),
                selection: Selection::at(text.chars().count()),
            }
        }

        fn edit(&mut self, tx: Transaction, new_caret: usize) {
            let before = self.selection;
            let applied = edit::apply(&mut self.rope, &tx);
            self.selection = Selection::at(new_caret);
            self.history
                .push(tx, applied.inverse, before, self.selection);
        }

        fn type_char(&mut self, c: char) {
            let at = self.selection.head;
            self.edit(Transaction::insert(at, c.to_string()), at + 1);
        }

        fn backspace(&mut self) {
            let at = self.selection.head;
            if at == 0 {
                return;
            }
            self.edit(Transaction::delete(at - 1..at), at - 1);
        }

        fn undo(&mut self) -> bool {
            match self.history.undo() {
                Some(step) => {
                    edit::apply(&mut self.rope, &step.transaction);
                    self.selection = step.selection;
                    true
                }
                None => false,
            }
        }

        fn redo(&mut self) -> bool {
            match self.history.redo() {
                Some(step) => {
                    edit::apply(&mut self.rope, &step.transaction);
                    self.selection = step.selection;
                    true
                }
                None => false,
            }
        }

        fn text(&self) -> String {
            self.rope.to_string()
        }
    }

    #[test]
    fn typing_a_word_is_one_undo_step_not_one_per_character() {
        let mut h = Harness::new("");
        for c in "hello".chars() {
            h.type_char(c);
        }
        assert_eq!(h.text(), "hello");
        assert_eq!(
            h.history.depth(),
            1,
            "five keystrokes within the coalesce window are one edit"
        );

        assert!(h.undo());
        assert_eq!(h.text(), "", "undo removes the whole word");
    }

    #[test]
    fn a_run_of_backspaces_is_one_undo_step() {
        let mut h = Harness::new("hello");
        for _ in 0..3 {
            h.backspace();
        }
        assert_eq!(h.text(), "he");
        assert_eq!(h.history.depth(), 1);

        assert!(h.undo());
        assert_eq!(h.text(), "hello", "undo restores all three characters");
    }

    #[test]
    fn a_newline_breaks_the_run() {
        let mut h = Harness::new("");
        h.type_char('a');
        let at = h.selection.head;
        h.edit(Transaction::insert(at, "\n"), at + 1);
        h.type_char('b');

        assert_eq!(h.history.depth(), 3, "the newline is its own entry");
        h.undo();
        assert_eq!(h.text(), "a\n");
    }

    #[test]
    fn break_run_prevents_the_next_edit_merging() {
        let mut h = Harness::new("");
        h.type_char('a');
        h.history.break_run();
        h.type_char('b');
        assert_eq!(h.history.depth(), 2);
    }

    #[test]
    fn typing_after_moving_the_caret_does_not_merge() {
        let mut h = Harness::new("hello world");
        h.selection = Selection::at(5);
        h.type_char('X');
        // Jump elsewhere and type again: the offsets are not contiguous.
        h.selection = Selection::at(0);
        h.type_char('Y');
        assert_eq!(
            h.history.depth(),
            2,
            "edits at unrelated offsets must not fold together"
        );
    }

    #[test]
    fn undo_and_redo_round_trip_through_many_edits() {
        let original = "the quick brown fox";
        let mut h = Harness::new(original);

        h.edit(Transaction::replace(4..9, "slow"), 8);
        h.history.break_run();
        h.edit(Transaction::insert(0, ">> "), 3);
        h.history.break_run();
        h.edit(Transaction::delete(0..3), 0);

        let after_all = h.text();
        let steps = h.history.depth();

        while h.undo() {}
        assert_eq!(
            h.text(),
            original,
            "undoing everything restores the original"
        );

        for _ in 0..steps {
            assert!(h.redo());
        }
        assert_eq!(
            h.text(),
            after_all,
            "redoing everything restores the result"
        );
    }

    #[test]
    fn undo_restores_the_cursor_to_where_the_edit_was() {
        let mut h = Harness::new("hello world");
        h.selection = Selection::at(5);
        h.type_char('!');
        assert_eq!(h.selection.head, 6);

        // Wander off, as the user would.
        h.selection = Selection::at(0);
        h.undo();
        assert_eq!(
            h.selection.head, 5,
            "undo must put the caret back where the change happened"
        );
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut h = Harness::new("");
        h.type_char('a');
        h.undo();
        assert!(h.history.can_redo());

        h.type_char('b');
        assert!(
            !h.history.can_redo(),
            "editing after undo must not leave a stale redo path"
        );
        assert_eq!(h.text(), "b");
    }

    #[test]
    fn undo_and_redo_are_no_ops_on_an_empty_history() {
        let mut h = Harness::new("text");
        assert!(!h.history.can_undo());
        assert!(!h.undo());
        assert!(!h.redo());
        assert_eq!(h.text(), "text");
    }

    #[test]
    fn coalescing_survives_multibyte_characters() {
        let mut h = Harness::new("");
        for c in "caf\u{e9}".chars() {
            h.type_char(c);
        }
        assert_eq!(h.text(), "caf\u{e9}");
        assert_eq!(h.history.depth(), 1);
        h.undo();
        assert_eq!(h.text(), "");
    }
}
