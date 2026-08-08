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
    // A run of typing, at one caret or at twenty: each insert has to land
    // exactly where the matching one from the previous revision ended.
    //
    // The multi-caret case is not the single case repeated. Caret `i` was
    // displaced by every insertion before it as well as by its own, so what has
    // to match is the *cumulative* length, and the undo transaction — which is
    // applied to the document as it exists after the redo — has to be written
    // in those shifted coordinates. Recording it in pre-edit coordinates makes
    // one-caret undo look right while corrupting every other case.
    if merge_typing(previous, redo, after) {
        return true;
    }

    let (Some(prev_redo), Some(new_redo)) = (previous.redo.as_single(), redo.as_single()) else {
        return false;
    };

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

/// Fold a keystroke into the previous one, however many carets are typing.
///
/// Returns false and changes nothing unless every edit on both sides is a
/// plain insertion and they line up caret for caret — so a paste, a newline, or
/// a change in the number of carets all start a fresh undo entry, which is
/// what you would want them to do anyway.
fn merge_typing(previous: &mut Revision, redo: &Transaction, after: Selection) -> bool {
    if previous.redo.edits.len() != redo.edits.len() || redo.edits.is_empty() {
        return false;
    }
    if !previous.redo.edits.iter().all(Edit::is_simple_insert)
        || !redo.edits.iter().all(Edit::is_simple_insert)
    {
        return false;
    }

    let mut before: Vec<&Edit> = previous.redo.edits.iter().collect();
    let mut now: Vec<&Edit> = redo.edits.iter().collect();
    before.sort_by_key(|e| e.range.start);
    now.sort_by_key(|e| e.range.start);

    // Each new insert must sit where its caret was left, which is its own
    // position plus every character inserted at or before it last time.
    let mut cumulative = 0usize;
    for (was, is) in before.iter().zip(&now) {
        cumulative += was.text.chars().count();
        if is.range.start != was.range.start + cumulative {
            return false;
        }
    }

    let mut merged_redo = Vec::with_capacity(before.len());
    let mut merged_undo = Vec::with_capacity(before.len());
    let mut shift = 0usize;
    for (was, is) in before.iter().zip(&now) {
        let text = format!("{}{}", was.text, is.text);
        let len = text.chars().count();
        let at = was.range.start;
        merged_redo.push(Edit::insert(at, text));
        // Post-redo coordinates: this insertion sits after everything inserted
        // before it.
        merged_undo.push(Edit::delete(at + shift..at + shift + len));
        shift += len;
    }

    previous.redo = Transaction::new(merged_redo);
    previous.undo = Transaction::new(merged_undo);
    previous.after = after;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{self, Transaction};

    /// The decisive check for multi-caret coalescing: type a word at several
    /// carets, undo once, and the document must be exactly what it started as.
    ///
    /// Written against the rope rather than against the transactions, because
    /// the bug this guards is arithmetic in the *undo* coordinates and only
    /// applying it can show that up.
    #[test]
    fn typing_a_word_at_several_carets_undoes_in_one_step() {
        let original = "aaa\nbbb\nccc\n";
        let mut rope = ropey::Rope::from_str(original);
        let mut history = History::default();

        // Carets at the start of each line, typing "xy" one character at a time.
        let mut carets = vec![0usize, 4, 8];
        for ch in ["x", "y"] {
            let tx = Transaction::new(
                carets
                    .iter()
                    .map(|at| Edit::insert(*at, ch.to_owned()))
                    .collect(),
            );
            let applied = edit::apply(&mut rope, &tx);
            let primary = *carets.first().expect("there are carets");
            history.push(
                tx.clone(),
                applied.inverse,
                Selection::at(primary),
                Selection::at(primary + 1),
            );
            // Where each caret ends up, which is where the next keystroke goes.
            carets = carets.iter().map(|at| edit::remap(&tx, *at)).collect();
        }
        assert_eq!(rope.to_string(), "xyaaa\nxybbb\nxyccc\n");
        assert_eq!(history.depth(), 1, "two keystrokes, one undo entry");

        let step = history.undo().expect("something to undo");
        edit::apply(&mut rope, &step.transaction);
        assert_eq!(
            rope.to_string(),
            original,
            "one undo must restore the document exactly"
        );

        let step = history.redo().expect("something to redo");
        edit::apply(&mut rope, &step.transaction);
        assert_eq!(
            rope.to_string(),
            "xyaaa\nxybbb\nxyccc\n",
            "and redo puts it back"
        );
    }

    /// Carets appearing or disappearing between keystrokes must start a new
    /// entry rather than being folded into a mismatched one.
    #[test]
    fn a_change_in_the_number_of_carets_breaks_the_run() {
        let mut rope = ropey::Rope::from_str("aaa\nbbb\n");
        let mut history = History::default();

        let one = Transaction::new(vec![Edit::insert(0, "x"), Edit::insert(4, "x")]);
        let applied = edit::apply(&mut rope, &one);
        history.push(one, applied.inverse, Selection::at(0), Selection::at(1));

        // Now only one caret.
        let two = Transaction::single(Edit::insert(1, "y"));
        let applied = edit::apply(&mut rope, &two);
        history.push(two, applied.inverse, Selection::at(1), Selection::at(2));

        assert_eq!(history.depth(), 2, "different caret counts do not merge");
    }

    /// Two carets typing where the second is not where the previous keystroke
    /// left it -- a click in between, say -- must not be folded together.
    #[test]
    fn inserts_that_do_not_continue_the_run_start_a_new_entry() {
        let mut rope = ropey::Rope::from_str("aaaaaaaaaa\n");
        let mut history = History::default();

        let one = Transaction::new(vec![Edit::insert(0, "x"), Edit::insert(5, "x")]);
        let applied = edit::apply(&mut rope, &one);
        history.push(one, applied.inverse, Selection::at(0), Selection::at(1));

        // The run would continue at 1 and 7; this is somewhere else entirely.
        let two = Transaction::new(vec![Edit::insert(1, "y"), Edit::insert(9, "y")]);
        let applied = edit::apply(&mut rope, &two);
        history.push(two, applied.inverse, Selection::at(1), Selection::at(2));

        assert_eq!(history.depth(), 2);
    }

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
