//! Word boundaries, for Ctrl+arrow motion and Ctrl+Backspace/Delete.
//!
//! Every editor implements this slightly differently and users notice
//! immediately when it is wrong, so the rules are stated rather than emergent:
//!
//! * Characters fall into three classes — word (alphanumeric or `_`),
//!   punctuation, and whitespace. `snake_case` is one word; `foo.bar` is three
//!   moves; `x += 1` steps over `+=` as a unit.
//! * A move consumes any run of spaces or tabs, then one run of a single class.
//!   So from the start of `print(i)` one press lands on `(` and the next on `i`.
//! * A line break is always its own stop. Crossing it takes a separate press,
//!   which is what makes Ctrl+Right predictable at the end of a line — a motion
//!   that silently jumped to the next line's second word would be worse than
//!   one that stops somewhere obvious.
//!
//! Classification uses `char::is_alphanumeric`, so accented letters and
//! non-Latin scripts are word characters rather than punctuation.

use ropey::Rope;

/// What kind of character this is, for grouping runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Punctuation,
    Space,
    Newline,
}

fn class_of(c: char) -> Class {
    if c == '\n' || c == '\r' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

/// The offset one word to the right of `from`.
///
/// Clamped to the end of the document; never moves backwards.
#[must_use]
pub fn next_boundary(text: &Rope, from: usize) -> usize {
    let len = text.len_chars();
    let mut i = from.min(len);
    if i >= len {
        return len;
    }

    // A line break is a stop in itself. `\r\n` counts as one.
    if class_of(text.char(i)) == Class::Newline {
        i += 1;
        if i < len && text.char(i - 1) == '\r' && text.char(i) == '\n' {
            i += 1;
        }
        return i;
    }

    while i < len && class_of(text.char(i)) == Class::Space {
        i += 1;
    }
    if i < len {
        let class = class_of(text.char(i));
        if class != Class::Newline {
            while i < len && class_of(text.char(i)) == class {
                i += 1;
            }
        }
    }
    i
}

/// The offset one word to the left of `from`.
///
/// Clamped to zero; never moves forwards.
#[must_use]
pub fn prev_boundary(text: &Rope, from: usize) -> usize {
    let mut i = from.min(text.len_chars());
    if i == 0 {
        return 0;
    }

    if class_of(text.char(i - 1)) == Class::Newline {
        i -= 1;
        if i > 0 && text.char(i) == '\n' && text.char(i - 1) == '\r' {
            i -= 1;
        }
        return i;
    }

    while i > 0 && class_of(text.char(i - 1)) == Class::Space {
        i -= 1;
    }
    if i > 0 {
        let class = class_of(text.char(i - 1));
        if class != Class::Newline {
            while i > 0 && class_of(text.char(i - 1)) == class {
                i -= 1;
            }
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// Walk right from 0 and collect every stop, which is the clearest way to
    /// state what the motion actually does.
    fn stops_right(s: &str) -> Vec<usize> {
        let text = rope(s);
        let mut out = Vec::new();
        let mut at = 0;
        while at < text.len_chars() {
            let next = next_boundary(&text, at);
            assert!(next > at, "no progress at {at} in {s:?}");
            out.push(next);
            at = next;
        }
        out
    }

    fn stops_left(s: &str) -> Vec<usize> {
        let text = rope(s);
        let mut out = Vec::new();
        let mut at = text.len_chars();
        while at > 0 {
            let prev = prev_boundary(&text, at);
            assert!(prev < at, "no progress at {at} in {s:?}");
            out.push(prev);
            at = prev;
        }
        out
    }

    #[test]
    fn a_snake_case_identifier_is_one_word() {
        // Stopping inside `my_long_name` would make Ctrl+Right useless for
        // Python and Rust, where that is what most identifiers look like.
        let text = rope("my_long_name = 1");
        assert_eq!(next_boundary(&text, 0), 12);
        assert_eq!(prev_boundary(&text, 12), 0);
    }

    #[test]
    fn a_call_steps_through_its_punctuation() {
        // print(i)
        // 01234567
        assert_eq!(stops_right("print(i)"), [5, 6, 7, 8]);
    }

    #[test]
    fn a_run_of_punctuation_moves_as_one_unit() {
        // `x += 1`: the operator is one stop, not two.
        assert_eq!(stops_right("x += 1"), [1, 4, 6]);
    }

    #[test]
    fn leading_whitespace_is_consumed_with_the_word_after_it() {
        // Landing on the first letter, not on the space before it.
        let text = rope("    print");
        assert_eq!(next_boundary(&text, 0), 9);
    }

    #[test]
    fn a_line_break_is_its_own_stop() {
        // Ctrl+Right at the end of a line lands at the start of the next one
        // rather than leaping into the middle of it.
        let text = rope("ab\ncd");
        assert_eq!(next_boundary(&text, 2), 3, "the newline is one move");
        assert_eq!(next_boundary(&text, 3), 5, "then the next word");
        assert_eq!(prev_boundary(&text, 3), 2, "and back over it the same way");
    }

    #[test]
    fn a_windows_line_break_counts_as_one_stop_not_two() {
        // Documents normalise to `\n`, but a rope built from raw text may still
        // hold `\r\n`, and stopping between the two would put the caret in a
        // place that cannot be rendered.
        let text = rope("ab\r\ncd");
        assert_eq!(next_boundary(&text, 2), 4);
        assert_eq!(prev_boundary(&text, 4), 2);
    }

    #[test]
    fn moving_right_then_left_returns_to_a_word_start() {
        let text = rope("alpha beta gamma");
        let right = next_boundary(&text, 0);
        assert_eq!(right, 5);
        let further = next_boundary(&text, right);
        assert_eq!(further, 10);
        assert_eq!(prev_boundary(&text, further), 6, "the start of `beta`");
    }

    #[test]
    fn the_ends_of_the_document_are_hard_stops() {
        let text = rope("word");
        assert_eq!(next_boundary(&text, 4), 4);
        assert_eq!(next_boundary(&text, 99), 4, "past the end is clamped");
        assert_eq!(prev_boundary(&text, 0), 0);
    }

    #[test]
    fn an_empty_document_does_not_move_or_panic() {
        let text = rope("");
        assert_eq!(next_boundary(&text, 0), 0);
        assert_eq!(prev_boundary(&text, 0), 0);
    }

    #[test]
    fn every_step_makes_progress_across_awkward_text() {
        // The loops in `stops_*` assert progress, so a motion that could stall
        // fails here rather than hanging the editor.
        for source in [
            "",
            "\n",
            "\n\n\n",
            "   ",
            "a",
            "a b",
            "  \n  \n",
            "def f(x): return x  # note",
            "self.items[0] += other.items[-1]",
            "\t\tif x == 1:\n\t\t\tpass\n",
        ] {
            let _ = stops_right(source);
            let _ = stops_left(source);
        }
    }

    /// The two directions deliberately do not mirror each other.
    ///
    /// Moving right stops at the *end* of each word; moving left stops at the
    /// *start*. This is what VS Code, Sublime and Visual Studio all do, and it
    /// is why Ctrl+Right followed by Ctrl+Left does not always return the caret
    /// to where it began. Written down because it looks like a bug otherwise.
    #[test]
    fn right_stops_at_word_ends_and_left_stops_at_word_starts() {
        //          0    5    10   15   20
        //          alpha beta_gamma delta
        let source = "alpha beta_gamma delta";
        assert_eq!(stops_right(source), [5, 16, 22], "ends of each word");

        let mut left = stops_left(source);
        left.reverse();
        assert_eq!(left, [0, 6, 17], "starts of each word");
    }

    #[test]
    fn a_motion_never_overshoots_the_text_it_was_given() {
        // Weaker than a round trip, but the property that actually matters: no
        // input can produce an offset outside the document, which would panic
        // the moment the caret was rendered.
        for source in ["", "a", "a b c", "  \n\tx\n"] {
            let text = rope(source);
            let len = text.len_chars();
            for i in 0..=len {
                assert!(next_boundary(&text, i) <= len);
                assert!(prev_boundary(&text, i) <= len);
            }
        }
    }

    #[test]
    fn accented_and_non_latin_letters_are_word_characters() {
        // `is_alphanumeric` rather than `is_ascii_alphanumeric`: treating these
        // as punctuation would step through a name one letter at a time.
        let text = rope("naïve café");
        assert_eq!(next_boundary(&text, 0), 5);
        let text = rope("переменная = 1");
        assert_eq!(next_boundary(&text, 0), 10);
    }

    #[test]
    fn a_blank_line_between_paragraphs_is_not_skipped_over() {
        // Two newlines are two stops, so Ctrl+Down-style navigation through a
        // file with blank lines stays predictable.
        let text = rope("a\n\nb");
        assert_eq!(next_boundary(&text, 1), 2);
        assert_eq!(next_boundary(&text, 2), 3);
    }
}
