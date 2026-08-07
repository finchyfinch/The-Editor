//! Syntax errors read straight out of the parse tree.
//!
//! Every language server is optional — PLAN.md §3.6 — but "no linter installed"
//! must not mean "obviously broken code looks fine". The tree-sitter grammar
//! that highlights a file is already parsing it on every keystroke, and its
//! error recovery marks exactly where it stopped making sense. Reporting that
//! costs one tree walk and needs nothing installed, so a Python file with
//! `if bob = kate` in it is flagged whether or not Ruff is on the machine.
//!
//! This is a *parser*, not a type checker: it finds what is not the language,
//! never what is merely wrong. Undefined names, bad arguments and type errors
//! remain the language server's job.

use ropey::Rope;
use tree_sitter::{Node, Tree, TreeCursor};

/// A place the parser could not make sense of.
///
/// Positions are zero-based lines and *character* columns, matching what the
/// rest of the editor works in. Tree-sitter's own `Point::column` is a byte
/// offset within the row, which is wrong the moment a line contains a non-ASCII
/// character, so it is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub message: String,
}

/// The most errors reported for one document.
///
/// A file being typed into from scratch, or one whose grammar has lost its
/// footing near the top, can produce hundreds of cascading errors that all
/// describe the same mistake. Past the first handful they stop being
/// information and start being noise that buries the language server's
/// findings.
const MAX_ERRORS: usize = 20;

/// Walk a parse tree and report where it broke.
///
/// Only the outermost error in any subtree is reported: the children of an
/// `ERROR` node are the parser's best guess at rubble, and reporting them is
/// how one missing colon turns into fifteen squiggles.
#[must_use]
pub fn from_tree(tree: &Tree, text: &Rope) -> Vec<SyntaxError> {
    let root = tree.root_node();
    if !root.has_error() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut cursor = root.walk();
    collect(&mut cursor, text, &mut out);
    out
}

/// Depth-first, descending only into subtrees that contain an error.
///
/// `has_error` is a flag tree-sitter maintains on every node, so skipping the
/// clean parts of a large file is a pointer comparison rather than a search.
fn collect(cursor: &mut TreeCursor<'_>, text: &Rope, out: &mut Vec<SyntaxError>) {
    if out.len() >= MAX_ERRORS {
        return;
    }

    let node = cursor.node();

    // A missing node is zero-width: the parser inserted a token that was not
    // there so it could carry on. That is the most precise diagnostic a grammar
    // produces, and it is worth checking before `is_error`, since a MISSING node
    // is not itself an ERROR node.
    if node.is_missing() {
        out.push(describe_missing(&node, text));
        return;
    }

    if node.is_error() {
        out.push(describe_error(&node, text));
        // Do not descend: everything below is the recovery attempt, not
        // additional mistakes.
        return;
    }

    if !node.has_error() {
        return;
    }

    if cursor.goto_first_child() {
        loop {
            collect(cursor, text, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn describe_missing(node: &Node<'_>, text: &Rope) -> SyntaxError {
    // `kind` for a missing node is the token the grammar wanted, e.g. `:` or
    // `)`. Anonymous tokens read as themselves; named ones ("identifier") are
    // spelled out as prose.
    let wanted = node.kind();
    let message = if node.is_named() {
        format!("Expected {wanted}")
    } else {
        format!("Expected `{wanted}`")
    };

    // A zero-width range paints no squiggle. Widen it over the character the
    // token should have come before, or the one before it at end of line.
    let start = text.byte_to_char(node.start_byte().min(text.len_bytes()));
    let (from, to) = widen(start, text);
    span(from, to, text, message)
}

fn describe_error(node: &Node<'_>, text: &Rope) -> SyntaxError {
    let start = text.byte_to_char(node.start_byte().min(text.len_bytes()));
    let end = text.byte_to_char(node.end_byte().min(text.len_bytes()));

    // An error node can run to the end of a block when the parser gives up
    // early. Underlining twenty lines does not say where the mistake is, so the
    // squiggle stops at the end of the line the error starts on — and the
    // message quotes the same one line.
    let end = end.min(end_of_line(start, text)).max(start);

    // Quote what the parser could not read. Naming the error node's *first*
    // token instead reads as an accusation against the wrong word: for a `for`
    // statement missing its colon, the whole statement is the error node, and
    // "syntax error at `for`" points at the one part that was fine.
    let message = snippet(start, end, text).map_or_else(
        || "Syntax error".to_owned(),
        |quoted| format!("Syntax error: {quoted}"),
    );

    let (from, to) = if end > start {
        (start, end)
    } else {
        widen(start, text)
    };
    span(from, to, text, message)
}

/// The offending source, trimmed to fit one row of the Problems panel.
///
/// The panel is a list, not a document; a message that wraps to three lines is
/// worse than one that ends in an ellipsis.
const SNIPPET_CHARS: usize = 40;

fn snippet(start: usize, end: usize, text: &Rope) -> Option<String> {
    let raw: String = text.slice(start..end.max(start)).chars().collect();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let quoted = if trimmed.chars().count() > SNIPPET_CHARS {
        let head: String = trimmed.chars().take(SNIPPET_CHARS).collect();
        format!("{head}\u{2026}")
    } else {
        trimmed.to_owned()
    };
    Some(format!("`{quoted}`"))
}

/// The character offset of the end of `at`'s line, before its line break.
///
/// Including the break would put the end of the range on the *next* line, which
/// the editor then paints as a squiggle running into the left margin below.
fn end_of_line(at: usize, text: &Rope) -> usize {
    let line = text.char_to_line(at);
    let slice = text.line(line);
    let mut end = text.line_to_char(line) + slice.len_chars();
    for ch in ['\n', '\r'] {
        if end > 0 && text.char(end - 1) == ch {
            end -= 1;
        }
    }
    end
}

/// Grow a zero-width position into a one-character range.
fn widen(at: usize, text: &Rope) -> (usize, usize) {
    let len = text.len_chars();
    if at >= len {
        return (at.saturating_sub(1), at);
    }
    // Do not swallow the line break: a squiggle that wraps onto the next line
    // points at the wrong place.
    if text.char(at) == '\n' {
        return (at.saturating_sub(1), at);
    }
    (at, at + 1)
}

fn span(from: usize, to: usize, text: &Rope, message: String) -> SyntaxError {
    let (line, column) = line_column(from, text);
    let (end_line, end_column) = line_column(to, text);
    SyntaxError {
        line,
        column,
        end_line,
        end_column,
        message,
    }
}

fn line_column(offset: usize, text: &Rope) -> (u32, u32) {
    let offset = offset.min(text.len_chars());
    let line = text.char_to_line(offset);
    let column = offset - text.line_to_char(line);
    (line as u32, column as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageId;
    use crate::highlight::Highlighter;

    fn errors(language: LanguageId, source: &str) -> Vec<SyntaxError> {
        let rope = Rope::from_str(source);
        let highlighter = Highlighter::new(language, &rope).expect("grammar");
        highlighter.errors(&rope)
    }

    #[test]
    fn valid_code_reports_nothing() {
        assert!(errors(LanguageId::Python, "x = 1\nprint(x)\n").is_empty());
        assert!(errors(LanguageId::Rust, "fn main() { let x = 1; }\n").is_empty());
    }

    #[test]
    fn an_assignment_used_as_a_condition_is_reported() {
        // The case that started this: `=` where `==` was meant is not a type
        // error or a lint, it is not Python, and no server needs to be
        // installed for the parser to know that.
        let found = errors(
            LanguageId::Python,
            "bob = 1\nkate = 2\nif bob = kate:\n    pass\n",
        );
        assert!(
            !found.is_empty(),
            "an assignment in an `if` must be flagged"
        );
        assert_eq!(found[0].line, 2, "the error is on the `if` line");
    }

    #[test]
    fn a_missing_colon_is_reported_on_the_line_it_is_missing_from() {
        let found = errors(LanguageId::Python, "for i in data\n    print(i)\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 0);
        assert_eq!(
            found[0].end_line, 0,
            "the squiggle must not run onto line 1"
        );
    }

    #[test]
    fn the_message_quotes_the_source_rather_than_accusing_the_first_token() {
        // "Syntax error at `for`" points at the one part of `for i in data`
        // that was fine; the whole statement is the error node.
        let found = errors(LanguageId::Python, "for i in data\n    print(i)\n");
        assert_eq!(found[0].message, "Syntax error: `for i in data`");
    }

    #[test]
    fn a_long_error_is_summarised_rather_than_quoted_whole() {
        let long = "x".repeat(200);
        let found = errors(LanguageId::Python, &format!("if a = {long}:\n    pass\n"));
        assert!(!found.is_empty());
        for error in &found {
            assert!(
                error.message.chars().count() < 80,
                "the Problems panel is a list, not a document: {error:?}"
            );
        }
    }

    #[test]
    fn an_unclosed_bracket_is_reported() {
        let found = errors(LanguageId::Python, "print('hello'\n");
        assert!(!found.is_empty(), "an unclosed call must be flagged");
    }

    #[test]
    fn every_error_has_a_range_that_can_be_painted() {
        // A zero-width range draws no squiggle, so the user sees nothing.
        for source in [
            "for i in data\n    print(i)\n",
            "if x = 1:\n    pass\n",
            "def f(:\n    pass\n",
            "print('hello'\n",
        ] {
            for error in errors(LanguageId::Python, source) {
                let width = if error.line == error.end_line {
                    error.end_column.saturating_sub(error.column)
                } else {
                    1
                };
                assert!(width >= 1, "zero-width error in {source:?}: {error:?}");
            }
        }
    }

    #[test]
    fn positions_are_character_columns_not_byte_columns() {
        // Tree-sitter reports byte columns. With an emoji earlier on the line
        // those disagree, and a squiggle lands several characters to the right
        // of the mistake.
        let source = "s = '\u{1f600}\u{1f600}'\nif x = 1:\n    pass\n";
        let found = errors(LanguageId::Python, source);
        let rope = Rope::from_str(source);
        for error in &found {
            let line_start = rope.line_to_char(error.line as usize);
            let line_len = rope.line(error.line as usize).len_chars();
            assert!(
                (error.column as usize) <= line_len,
                "column {} is past the end of line {} ({line_len} chars)",
                error.column,
                error.line
            );
            let _ = line_start;
        }
    }

    #[test]
    fn cascading_errors_are_capped() {
        // A file being typed into can produce an error per line; past a handful
        // they describe the same mistake and bury everything else.
        let source = "def f(:\n".repeat(200);
        let found = errors(LanguageId::Python, &source);
        assert!(
            found.len() <= MAX_ERRORS,
            "reported {} errors, which is noise not information",
            found.len()
        );
    }

    #[test]
    fn one_mistake_does_not_become_a_wall_of_squiggles() {
        // Only the outermost error of a subtree is reported. Descending into an
        // ERROR node's children turns a single missing colon into a dozen.
        let found = errors(
            LanguageId::Python,
            "def main():\n    if bob = kate:\n        print('a')\n        print('b')\n",
        );
        assert!(
            found.len() <= 2,
            "one mistake produced {} diagnostics: {found:?}",
            found.len()
        );
    }

    #[test]
    fn rust_syntax_errors_are_found_too() {
        let found = errors(LanguageId::Rust, "fn main() { let x = ; }\n");
        assert!(!found.is_empty(), "the Rust grammar reports errors as well");
    }

    #[test]
    fn a_language_with_no_grammar_is_simply_not_checked() {
        let rope = Rope::from_str("anything at all\n");
        assert!(Highlighter::new(LanguageId::PlainText, &rope).is_none());
    }

    #[test]
    fn the_ini_fallback_reports_no_errors_rather_than_panicking() {
        // INI is highlighted line-by-line with no parse tree behind it.
        let rope = Rope::from_str("[section\nkey = value\n");
        let highlighter = Highlighter::new(LanguageId::Ini, &rope).expect("ini fallback");
        assert!(highlighter.errors(&rope).is_empty());
    }
}
