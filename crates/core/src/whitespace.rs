//! What to tidy when a file is written.
//!
//! Two policies, both off by default and both worth having:
//!
//! * trailing whitespace on a line is invisible, meaningless, and shows up as
//!   noise in every diff of the file forever after;
//! * a file with no final newline makes `git diff` say "\ No newline at end of
//!   file" and makes some tools drop the last line entirely.
//!
//! They are applied as a [`Transaction`] rather than by rewriting the text, so
//! the tidy lands in the undo history like any other edit. Saving and then
//! pressing undo gets the whitespace back, which is the behaviour anyone who
//! did it deliberately would expect.
//!
//! The caret is deliberately not moved. Someone whose caret sits in the middle
//! of the trailing spaces they just typed has it clamped by the edit itself;
//! trying to be cleverer than that moves it about for no reason.

use ropey::Rope;

use crate::edit::{Edit, Transaction};

/// What to do to a file on the way to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OnSave {
    pub trim_trailing_whitespace: bool,
    pub ensure_final_newline: bool,
}

impl OnSave {
    #[must_use]
    pub fn is_noop(self) -> bool {
        !self.trim_trailing_whitespace && !self.ensure_final_newline
    }
}

/// The edits that apply `policy` to `text`, or `None` if there is nothing to do.
///
/// Edits are produced in descending order of position so they can be applied
/// without any of them shifting the others' offsets.
#[must_use]
pub fn tidy(text: &Rope, policy: OnSave) -> Option<Transaction> {
    if policy.is_noop() {
        return None;
    }
    let mut edits = Vec::new();

    if policy.ensure_final_newline {
        let len = text.len_chars();
        // An empty file stays empty: adding a newline to nothing invents a
        // line the user never wrote.
        if len > 0 && text.char(len - 1) != '\n' {
            edits.push(Edit::insert(len, "\n"));
        }
    }

    if policy.trim_trailing_whitespace {
        // Descending, so each deletion is stated in coordinates the earlier
        // ones have not disturbed.
        for line in (0..text.len_lines()).rev() {
            let start = text.line_to_char(line);
            let slice = text.line(line);
            let content: String = slice.chars().collect();
            let body = content
                .strip_suffix('\n')
                .map_or(content.as_str(), |b| b.strip_suffix('\r').unwrap_or(b));

            let trimmed = body.trim_end_matches([' ', '\t']);
            if trimmed.len() == body.len() {
                continue;
            }
            let keep = trimmed.chars().count();
            let had = body.chars().count();
            edits.push(Edit::delete(start + keep..start + had));
        }
    }

    (!edits.is_empty()).then(|| Transaction::new(edits))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(source: &str, policy: OnSave) -> String {
        let mut text = Rope::from_str(source);
        if let Some(tx) = tidy(&text, policy) {
            crate::edit::apply(&mut text, &tx);
        }
        text.to_string()
    }

    const TRIM: OnSave = OnSave {
        trim_trailing_whitespace: true,
        ensure_final_newline: false,
    };
    const NEWLINE: OnSave = OnSave {
        trim_trailing_whitespace: false,
        ensure_final_newline: true,
    };
    const BOTH: OnSave = OnSave {
        trim_trailing_whitespace: true,
        ensure_final_newline: true,
    };

    #[test]
    fn trailing_spaces_and_tabs_go() {
        assert_eq!(applied("a   \nb\t\t\nc\n", TRIM), "a\nb\nc\n");
    }

    #[test]
    fn a_line_of_only_whitespace_is_emptied() {
        assert_eq!(applied("a\n    \nb\n", TRIM), "a\n\nb\n");
    }

    #[test]
    fn leading_and_interior_whitespace_is_untouched() {
        // Indentation is the point of Python; only the *trailing* run goes.
        assert_eq!(
            applied("    if x:\n        y = 1   \n", TRIM),
            "    if x:\n        y = 1\n"
        );
    }

    #[test]
    fn the_last_line_is_trimmed_even_without_a_newline_after_it() {
        assert_eq!(applied("a\nb   ", TRIM), "a\nb");
    }

    #[test]
    fn a_missing_final_newline_is_added() {
        assert_eq!(applied("a\nb", NEWLINE), "a\nb\n");
    }

    #[test]
    fn a_file_that_already_ends_in_one_is_left_alone() {
        assert_eq!(applied("a\nb\n", NEWLINE), "a\nb\n");
        assert!(tidy(&Rope::from_str("a\n"), NEWLINE).is_none());
    }

    #[test]
    fn an_empty_file_stays_empty() {
        // Adding a newline to nothing invents a line nobody wrote.
        assert_eq!(applied("", NEWLINE), "");
        assert_eq!(applied("", BOTH), "");
    }

    #[test]
    fn both_policies_together_do_not_disturb_each_others_offsets() {
        // The newline is appended at the old end while the trims shorten lines
        // before it. Stating the edits in the wrong order corrupts the file.
        assert_eq!(applied("a   \nb\t", BOTH), "a\nb\n");
    }

    #[test]
    fn a_file_needing_nothing_produces_no_transaction() {
        // So saving an already-tidy file does not push an empty undo step.
        assert!(tidy(&Rope::from_str("a\nb\n"), BOTH).is_none());
    }

    #[test]
    fn doing_nothing_is_the_default() {
        assert!(OnSave::default().is_noop());
        assert!(tidy(&Rope::from_str("a   \n"), OnSave::default()).is_none());
    }

    #[test]
    fn a_windows_line_ending_is_not_mistaken_for_trailing_whitespace() {
        // The rope holds `\n`, but a stray `\r` before it must not be eaten as
        // if it were spaces, or the line ending is silently changed.
        let text = Rope::from_str("a\r\nb\r\n");
        let out = tidy(&text, TRIM);
        assert!(out.is_none(), "nothing to trim: {out:?}");
    }

    #[test]
    fn many_lines_are_all_trimmed_in_one_transaction() {
        let source = "x   \n".repeat(50);
        let out = applied(&source, TRIM);
        assert_eq!(out, "x\n".repeat(50));
    }
}
