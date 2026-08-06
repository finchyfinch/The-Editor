//! Language-aware indentation, comment tokens and bracket pairs.
//!
//! Lives here rather than in `editor-core` because every rule is
//! language-specific and `LanguageId` is defined in this crate.
//!
//! Python is the case that has to be right. Getting a newline wrong in a
//! brace-delimited language is a cosmetic annoyance; getting it wrong in Python
//! changes what the program means. The rules implemented, from PLAN.md §3.4:
//!
//! * a line ending in `:` opens a block, so the next line indents one level;
//! * inside an unclosed bracket, continuation lines align to the column after
//!   the opener — unless the opener is the last thing on its line, in which case
//!   a hanging indent of one level is used, which is what PEP 8 asks for;
//! * `return`, `pass`, `raise`, `break` and `continue` end a block, so the next
//!   line dedents;
//! * typing `else`, `elif`, `except`, `finally` or `case` as the first word on a
//!   line dedents it to match its opener.

use ropey::Rope;

use crate::LanguageId;

/// How indentation is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndentOptions {
    pub tab_width: usize,
    pub insert_spaces: bool,
}

impl Default for IndentOptions {
    fn default() -> Self {
        Self {
            tab_width: 4,
            insert_spaces: true,
        }
    }
}

impl IndentOptions {
    /// One indent level, as text.
    #[must_use]
    pub fn one_level(self) -> String {
        if self.insert_spaces {
            " ".repeat(self.tab_width)
        } else {
            "\t".to_owned()
        }
    }

    /// `width` columns of indentation, as text.
    #[must_use]
    pub fn columns(self, width: usize) -> String {
        if self.insert_spaces {
            " ".repeat(width)
        } else {
            let tabs = width / self.tab_width;
            let spaces = width % self.tab_width;
            "\t".repeat(tabs) + &" ".repeat(spaces)
        }
    }
}

/// How far back to look for an unclosed bracket.
///
/// A bounded scan: real continuation lines are a handful of lines long, and an
/// unbounded backwards scan would make Enter cost O(file) in a long file.
const MAX_SCAN_LINES: usize = 200;

/// Statements after which the next line dedents, in Python.
const PYTHON_BLOCK_ENDERS: &[&str] = &["return", "pass", "raise", "break", "continue"];

/// Keywords that, typed at the start of a line, dedent it to match the block
/// they continue.
const PYTHON_DEDENT_KEYWORDS: &[&str] = &["else", "elif", "except", "finally", "case"];

/// The indentation a new line should start with, when Enter is pressed at
/// `offset`.
///
/// Returns the literal whitespace to insert after the newline.
#[must_use]
pub fn new_line_indent(
    text: &Rope,
    offset: usize,
    language: LanguageId,
    opts: IndentOptions,
) -> String {
    let offset = offset.min(text.len_chars());
    let line_index = text.char_to_line(offset);
    let line_start = text.line_to_char(line_index);
    let prefix: String = text.slice(line_start..offset).to_string();
    let current_indent = leading_width(&prefix, opts.tab_width);

    // Inside an unclosed bracket, alignment wins over everything else.
    if let Some(open) = unclosed_opener(text, offset, language) {
        return opts.columns(continuation_width(text, open, opts));
    }

    let code = strip_trailing_comment(&prefix, language);
    let trimmed = code.trim_end();

    let width = match language {
        LanguageId::Python => {
            if trimmed.ends_with(':') {
                current_indent + opts.tab_width
            } else if first_word(&code).is_some_and(|w| PYTHON_BLOCK_ENDERS.contains(&w)) {
                current_indent.saturating_sub(opts.tab_width)
            } else {
                current_indent
            }
        }
        LanguageId::Rust
        | LanguageId::JavaScript
        | LanguageId::Css
        | LanguageId::Json
        | LanguageId::Html => {
            if trimmed.ends_with(['{', '[', '(']) || opening_html_tag(trimmed, language) {
                current_indent + opts.tab_width
            } else {
                current_indent
            }
        }
        LanguageId::Ini | LanguageId::Toml | LanguageId::Markdown | LanguageId::PlainText => {
            current_indent
        }
    };

    opts.columns(width)
}

/// The indentation `line` should have, if typing has just made it a
/// continuation of an outer block. `None` means leave it alone.
///
/// Called after each character is typed, so `else` re-aligns as soon as the
/// word is complete rather than waiting for the colon.
#[must_use]
pub fn dedent_after_typing(
    text: &Rope,
    line_index: usize,
    language: LanguageId,
    opts: IndentOptions,
) -> Option<usize> {
    if line_index == 0 {
        return None;
    }
    let line = line_text(text, line_index);
    let body = line.trim_start();

    match language {
        LanguageId::Python => {
            let word = first_word(body)?;
            if !PYTHON_DEDENT_KEYWORDS.contains(&word) {
                return None;
            }
            // Only while the keyword is all that has been typed; re-indenting
            // mid-statement would fight the user.
            if body.trim_end() != word && !body.trim_end().ends_with(':') {
                return None;
            }

            let current = leading_width(&line, opts.tab_width);
            let target = python_dedent_target(text, line_index, word, opts)?;
            (target != current).then_some(target)
        }
        LanguageId::Rust | LanguageId::JavaScript | LanguageId::Css | LanguageId::Json => {
            if body.trim_end() != "}" && body.trim_end() != "]" && body.trim_end() != ")" {
                return None;
            }
            let current = leading_width(&line, opts.tab_width);
            let closer = body.trim_end().chars().next()?;
            let opener = matching_opener(closer)?;
            let line_start = text.line_to_char(line_index);
            let at = line_start + (line.len() - line.trim_start().len());
            let open = find_matching_open(text, at, opener, closer, language)?;
            let target = leading_width(&line_text(text, text.char_to_line(open)), opts.tab_width);
            (target != current).then_some(target)
        }
        _ => None,
    }
}

/// Indentation for a Python `else`/`elif`/`except`/`finally`/`case`: match the
/// nearest line above that opens the block it belongs to.
fn python_dedent_target(
    text: &Rope,
    line_index: usize,
    keyword: &str,
    opts: IndentOptions,
) -> Option<usize> {
    let openers: &[&str] = match keyword {
        "elif" | "else" => &["if", "elif", "for", "while", "try", "except"],
        "except" | "finally" => &["try", "except"],
        "case" => &["match", "case"],
        _ => return None,
    };

    let current = leading_width(&line_text(text, line_index), opts.tab_width);

    for above in (0..line_index).rev() {
        let line = line_text(text, above);
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let width = leading_width(&line, opts.tab_width);
        if width > current {
            continue; // deeper than us; not our opener
        }
        if let Some(word) = first_word(body)
            && openers.contains(&word)
        {
            return Some(width);
        }
        if width < current {
            // Reached a shallower line that is not an opener; stop rather than
            // dragging the keyword somewhere unrelated.
            return None;
        }
    }
    None
}

/// The column a continuation line should start at, given the offset of the
/// unclosed opening bracket.
fn continuation_width(text: &Rope, open: usize, opts: IndentOptions) -> usize {
    let line_index = text.char_to_line(open);
    let line = line_text(text, line_index);
    let line_start = text.line_to_char(line_index);
    let column = open - line_start;

    // Is the opener the last non-whitespace character on its line?
    let after: String = line.chars().skip(column + 1).collect();
    if after.trim().is_empty() {
        // Hanging indent: one level past the line that opened the bracket.
        // PEP 8 calls for this whenever no argument follows the opener.
        leading_width(&line, opts.tab_width) + opts.tab_width
    } else {
        // Visual alignment with the first argument.
        column_width(&line, column + 1, opts.tab_width)
    }
}

/// The innermost bracket opened before `offset` and not yet closed.
///
/// Scans forward from a plausible statement start, tracking string and comment
/// state, because scanning backwards cannot tell a quote that opens a string
/// from one that closes it.
fn unclosed_opener(text: &Rope, offset: usize, language: LanguageId) -> Option<usize> {
    let line_index = text.char_to_line(offset);
    let from_line = statement_start_line(text, line_index);
    let start = text.line_to_char(from_line);

    let mut stack: Vec<usize> = Vec::new();
    let mut scanner = Scanner::new(language);

    for (i, c) in text.slice(start..offset).chars().enumerate() {
        match scanner.step(c) {
            Token::Code('(' | '[' | '{') => stack.push(start + i),
            Token::Code(')' | ']' | '}') => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.pop()
}

/// Walk back to a line that plausibly starts a statement, bounded by
/// [`MAX_SCAN_LINES`].
fn statement_start_line(text: &Rope, line_index: usize) -> usize {
    let floor = line_index.saturating_sub(MAX_SCAN_LINES);
    let mut candidate = line_index;
    for above in (floor..=line_index).rev() {
        let line = line_text(text, above);
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        candidate = above;
        // A line starting at column zero with real content is a statement
        // start unless it is itself a closing bracket.
        if line.starts_with(|c: char| !c.is_whitespace()) && !body.starts_with([')', ']', '}']) {
            return above;
        }
    }
    candidate.max(floor)
}

/// Find the opener matching a closer at `at`, scanning backwards by depth.
fn find_matching_open(
    text: &Rope,
    at: usize,
    opener: char,
    closer: char,
    language: LanguageId,
) -> Option<usize> {
    let line_index = text.char_to_line(at);
    let from_line = statement_start_line(text, line_index).min(line_index);
    let start = text.line_to_char(from_line.saturating_sub(MAX_SCAN_LINES));

    let mut stack = Vec::new();
    let mut scanner = Scanner::new(language);
    for (i, c) in text.slice(start..at).chars().enumerate() {
        match scanner.step(c) {
            Token::Code(c) if c == opener => stack.push(start + i),
            Token::Code(c) if c == closer => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.pop()
}

fn matching_opener(closer: char) -> Option<char> {
    match closer {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        _ => None,
    }
}

/// What a character turned out to be once string and comment state is applied.
enum Token {
    Code(char),
    Other,
}

/// Minimal lexer: enough to tell brackets in code from brackets in strings and
/// comments. Not a parser — it only has to be right about quoting.
struct Scanner {
    /// The language's line-comment token, as characters. Handles both the
    /// one-character kind (`#`, `;`) and the two-character kind (`//`); without
    /// the latter, a Rust line ending `// note {` would indent the next line.
    comment: Option<Vec<char>>,
    in_string: Option<char>,
    escaped: bool,
    in_line_comment: bool,
    /// How much of a multi-character comment token has matched so far.
    comment_progress: usize,
}

impl Scanner {
    fn new(language: LanguageId) -> Self {
        Self {
            comment: line_comment_token(language).map(|t| t.chars().collect()),
            in_string: None,
            escaped: false,
            in_line_comment: false,
            comment_progress: 0,
        }
    }

    fn step(&mut self, c: char) -> Token {
        if c == '\n' {
            self.in_line_comment = false;
            // An unterminated single-quoted string does not survive a newline.
            self.in_string = None;
            self.escaped = false;
            self.comment_progress = 0;
            return Token::Other;
        }
        if self.in_line_comment {
            return Token::Other;
        }
        if let Some(quote) = self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if c == '\\' {
                self.escaped = true;
            } else if c == quote {
                self.in_string = None;
            }
            return Token::Other;
        }
        if c == '"' || c == '\'' {
            self.in_string = Some(c);
            self.comment_progress = 0;
            return Token::Other;
        }

        if let Some(token) = &self.comment {
            if token.get(self.comment_progress) == Some(&c) {
                self.comment_progress += 1;
                if self.comment_progress == token.len() {
                    self.in_line_comment = true;
                    self.comment_progress = 0;
                }
                // Not code: a partial match might still complete. If it does
                // not, the character was punctuation like `/`, which is not a
                // bracket, so treating it as non-code costs nothing.
                return Token::Other;
            }
            self.comment_progress = 0;
        }

        Token::Code(c)
    }
}

/// The token that starts a line comment, if the language has one.
#[must_use]
pub fn line_comment_token(language: LanguageId) -> Option<&'static str> {
    match language {
        LanguageId::Python | LanguageId::Toml => Some("#"),
        LanguageId::Rust | LanguageId::JavaScript => Some("//"),
        LanguageId::Ini => Some(";"),
        // CSS, JSON, HTML and Markdown have no line comment; JSON has no
        // comment at all.
        _ => None,
    }
}

/// The tokens that open and close a block comment, if the language has one.
#[must_use]
pub fn block_comment_tokens(language: LanguageId) -> Option<(&'static str, &'static str)> {
    match language {
        LanguageId::Rust | LanguageId::JavaScript | LanguageId::Css => Some(("/*", "*/")),
        LanguageId::Html | LanguageId::Markdown => Some(("<!--", "-->")),
        _ => None,
    }
}

/// The closing character to insert when `open` is typed, if any.
#[must_use]
pub fn auto_close(language: LanguageId, open: char) -> Option<char> {
    let pair = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    };
    // Single quotes are apostrophes far more often than string delimiters in
    // Markdown and plain text, and auto-closing them there is infuriating.
    if open == '\''
        && matches!(
            language,
            LanguageId::Markdown | LanguageId::PlainText | LanguageId::Ini
        )
    {
        return None;
    }
    if open == '`' && !matches!(language, LanguageId::JavaScript | LanguageId::Markdown) {
        return None;
    }
    Some(pair)
}

/// True if `close` is the closing half of a pair, for type-over.
#[must_use]
pub fn is_closing(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
}

// ---- helpers -------------------------------------------------------------

fn line_text(text: &Rope, line_index: usize) -> String {
    if line_index >= text.len_lines() {
        return String::new();
    }
    text.line(line_index)
        .to_string()
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

/// Visual width of a line's leading whitespace, expanding tabs.
fn leading_width(line: &str, tab_width: usize) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += tab_width - (width % tab_width),
            _ => break,
        }
    }
    width
}

/// Visual column of character index `index`, expanding tabs.
fn column_width(line: &str, index: usize, tab_width: usize) -> usize {
    let mut width = 0;
    for c in line.chars().take(index) {
        if c == '\t' {
            width += tab_width - (width % tab_width);
        } else {
            width += 1;
        }
    }
    width
}

/// The first whitespace-delimited word, stripped of trailing punctuation that
/// would stop it matching a keyword.
fn first_word(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    let end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(trimmed.len());
    (end > 0).then(|| &trimmed[..end])
}

/// Remove a trailing line comment so `# note:` does not look like a block
/// opener.
fn strip_trailing_comment(line: &str, language: LanguageId) -> String {
    let Some(token) = line_comment_token(language) else {
        return line.to_owned();
    };
    let mut scanner = Scanner::new(language);
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match scanner.step(c) {
            Token::Code(_) | Token::Other if !scanner.in_line_comment => out.push(c),
            _ => break,
        }
    }
    if out.len() == line.len() {
        line.to_owned()
    } else {
        out.trim_end_matches(token).to_owned()
    }
}

/// True for an HTML line ending in an opening tag that is not self-closing.
fn opening_html_tag(trimmed: &str, language: LanguageId) -> bool {
    language == LanguageId::Html
        && trimmed.ends_with('>')
        && !trimmed.ends_with("/>")
        && trimmed
            .rfind('<')
            .is_some_and(|i| !trimmed[i..].starts_with("</") && !trimmed[i..].starts_with("<!"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> IndentOptions {
        IndentOptions::default()
    }

    /// Indent produced by pressing Enter at the end of `source`.
    fn enter_at_end(source: &str, language: LanguageId) -> String {
        let rope = Rope::from_str(source);
        new_line_indent(&rope, rope.len_chars(), language, opts())
    }

    // ---- Python: the cases that change program meaning -------------------

    #[test]
    fn python_indents_after_a_colon() {
        assert_eq!(enter_at_end("def f():", LanguageId::Python), "    ");
        assert_eq!(enter_at_end("    if x:", LanguageId::Python), "        ");
        assert_eq!(
            enter_at_end("class A:", LanguageId::Python),
            "    ",
            "a class body indents like any other block"
        );
    }

    #[test]
    fn python_keeps_the_current_indent_on_an_ordinary_line() {
        assert_eq!(enter_at_end("    x = 1", LanguageId::Python), "    ");
        assert_eq!(enter_at_end("x = 1", LanguageId::Python), "");
    }

    #[test]
    fn python_dedents_after_a_statement_that_ends_a_block() {
        for ender in PYTHON_BLOCK_ENDERS {
            let source = format!("def f():\n    {ender}");
            assert_eq!(
                enter_at_end(&source, LanguageId::Python),
                "",
                "the line after `{ender}` should dedent"
            );
        }
    }

    #[test]
    fn python_does_not_dedent_after_a_word_merely_starting_with_a_block_ender() {
        assert_eq!(
            enter_at_end("def f():\n    returns = 1", LanguageId::Python),
            "    ",
            "`returns` is not `return`"
        );
        assert_eq!(
            enter_at_end("def f():\n    passenger = 1", LanguageId::Python),
            "    "
        );
    }

    #[test]
    fn python_aligns_continuation_lines_to_the_opening_bracket() {
        // PEP 8 visual alignment: the argument column, not a fixed indent.
        let source = "result = function(arg_one,";
        let indent = enter_at_end(source, LanguageId::Python);
        assert_eq!(
            indent.len(),
            source.find('(').expect("bracket") + 1,
            "continuation should line up under `arg_one`, got {indent:?}"
        );
    }

    #[test]
    fn python_uses_a_hanging_indent_when_nothing_follows_the_bracket() {
        // PEP 8: no argument on the opening line means a hanging indent.
        assert_eq!(
            enter_at_end("result = function(", LanguageId::Python),
            "    "
        );
        assert_eq!(
            enter_at_end("    result = function(", LanguageId::Python),
            "        "
        );
    }

    #[test]
    fn python_handles_nested_brackets() {
        let source = "x = foo(bar(1,";
        let indent = enter_at_end(source, LanguageId::Python);
        assert_eq!(
            indent.len(),
            source.rfind('(').expect("inner bracket") + 1,
            "should align to the innermost open bracket"
        );
    }

    #[test]
    fn python_ignores_brackets_inside_strings() {
        // The `(` is inside a string literal and opens nothing.
        assert_eq!(
            enter_at_end("x = \"a ( b\"", LanguageId::Python),
            "",
            "a bracket in a string must not trigger a continuation indent"
        );
        assert_eq!(enter_at_end("x = '('", LanguageId::Python), "");
    }

    #[test]
    fn python_ignores_brackets_and_colons_inside_comments() {
        assert_eq!(
            enter_at_end("x = 1  # note: (see below", LanguageId::Python),
            "",
            "a comment must not open a block or a bracket"
        );
    }

    #[test]
    fn python_closed_brackets_do_not_trigger_alignment() {
        assert_eq!(enter_at_end("x = f(1, 2)", LanguageId::Python), "");
        assert_eq!(enter_at_end("    x = f(1, 2)", LanguageId::Python), "    ");
    }

    #[test]
    fn python_dedents_else_to_match_its_if() {
        let rope = Rope::from_str("if x:\n    pass\n    else");
        assert_eq!(
            dedent_after_typing(&rope, 2, LanguageId::Python, opts()),
            Some(0),
            "`else` should align with `if`"
        );
    }

    #[test]
    fn python_dedents_except_to_match_its_try() {
        let rope = Rope::from_str("try:\n    risky()\n    except");
        assert_eq!(
            dedent_after_typing(&rope, 2, LanguageId::Python, opts()),
            Some(0)
        );
    }

    #[test]
    fn python_leaves_an_already_aligned_keyword_alone() {
        let rope = Rope::from_str("if x:\n    pass\nelse");
        assert_eq!(
            dedent_after_typing(&rope, 2, LanguageId::Python, opts()),
            None,
            "no change means no edit, so the caret does not jump"
        );
    }

    #[test]
    fn python_does_not_dedent_a_word_that_merely_starts_with_a_keyword() {
        let rope = Rope::from_str("if x:\n    pass\n    elsewhere = 1");
        assert_eq!(
            dedent_after_typing(&rope, 2, LanguageId::Python, opts()),
            None
        );
    }

    #[test]
    fn python_dedent_finds_a_nested_opener() {
        let source = "def f():\n    if x:\n        pass\n        else";
        let rope = Rope::from_str(source);
        assert_eq!(
            dedent_after_typing(&rope, 3, LanguageId::Python, opts()),
            Some(4),
            "`else` belongs to the inner `if`, at four columns"
        );
    }

    // ---- brace languages -------------------------------------------------

    #[test]
    fn rust_indents_after_an_opening_brace() {
        assert_eq!(enter_at_end("fn main() {", LanguageId::Rust), "    ");
        assert_eq!(enter_at_end("    if x {", LanguageId::Rust), "        ");
    }

    #[test]
    fn rust_dedents_a_closing_brace_to_match_its_opener() {
        let rope = Rope::from_str("fn main() {\n    let x = 1;\n        }");
        assert_eq!(
            dedent_after_typing(&rope, 2, LanguageId::Rust, opts()),
            Some(0)
        );
    }

    #[test]
    fn rust_aligns_continuation_inside_an_unclosed_call() {
        let source = "let x = foo(a,";
        let indent = enter_at_end(source, LanguageId::Rust);
        assert_eq!(indent.len(), source.find('(').expect("bracket") + 1);
    }

    #[test]
    fn json_and_css_indent_after_their_openers() {
        assert_eq!(enter_at_end("{", LanguageId::Json), "    ");
        assert_eq!(enter_at_end("body {", LanguageId::Css), "    ");
    }

    #[test]
    fn html_indents_after_an_opening_tag_but_not_a_closing_or_void_one() {
        assert_eq!(enter_at_end("<div>", LanguageId::Html), "    ");
        assert_eq!(enter_at_end("    <br/>", LanguageId::Html), "    ");
        assert_eq!(enter_at_end("</div>", LanguageId::Html), "");
    }

    #[test]
    fn plain_formats_keep_the_current_indent() {
        for language in [
            LanguageId::PlainText,
            LanguageId::Markdown,
            LanguageId::Ini,
            LanguageId::Toml,
        ] {
            assert_eq!(enter_at_end("    some text", language), "    ");
        }
    }

    // ---- tabs ------------------------------------------------------------

    #[test]
    fn tab_indentation_emits_tabs_not_spaces() {
        let tabs = IndentOptions {
            tab_width: 4,
            insert_spaces: false,
        };
        let rope = Rope::from_str("fn main() {");
        assert_eq!(
            new_line_indent(&rope, rope.len_chars(), LanguageId::Rust, tabs),
            "\t"
        );
    }

    #[test]
    fn existing_tab_indentation_is_measured_by_visual_width() {
        assert_eq!(leading_width("\tx", 4), 4);
        assert_eq!(leading_width("\t\tx", 4), 8);
        assert_eq!(leading_width("  \tx", 4), 4, "a tab completes the stop");
        assert_eq!(leading_width("    x", 4), 4);
    }

    #[test]
    fn a_non_multiple_width_still_round_trips_through_columns() {
        let tabs = IndentOptions {
            tab_width: 4,
            insert_spaces: false,
        };
        assert_eq!(tabs.columns(6), "\t  ", "four columns of tab, two of space");
        assert_eq!(tabs.columns(8), "\t\t");
        assert_eq!(opts().columns(6), "      ");
    }

    // ---- comment and bracket data ----------------------------------------

    #[test]
    fn comment_tokens_match_the_language() {
        assert_eq!(line_comment_token(LanguageId::Python), Some("#"));
        assert_eq!(line_comment_token(LanguageId::Rust), Some("//"));
        assert_eq!(line_comment_token(LanguageId::Ini), Some(";"));
        assert_eq!(
            line_comment_token(LanguageId::Json),
            None,
            "JSON has no comments at all"
        );
        assert_eq!(block_comment_tokens(LanguageId::Css), Some(("/*", "*/")));
        assert_eq!(
            block_comment_tokens(LanguageId::Html),
            Some(("<!--", "-->"))
        );
        assert_eq!(block_comment_tokens(LanguageId::Python), None);
    }

    #[test]
    fn every_language_can_be_commented_one_way_or_another_except_json() {
        for language in LanguageId::ALL {
            let has_any =
                line_comment_token(language).is_some() || block_comment_tokens(language).is_some();
            match language {
                LanguageId::Json | LanguageId::PlainText => {
                    assert!(!has_any, "{language:?} should have no comment syntax");
                }
                other => assert!(has_any, "{other:?} has no way to comment a line"),
            }
        }
    }

    #[test]
    fn brackets_and_quotes_auto_close() {
        assert_eq!(auto_close(LanguageId::Python, '('), Some(')'));
        assert_eq!(auto_close(LanguageId::Python, '['), Some(']'));
        assert_eq!(auto_close(LanguageId::Python, '{'), Some('}'));
        assert_eq!(auto_close(LanguageId::Python, '"'), Some('"'));
        assert_eq!(auto_close(LanguageId::Python, 'x'), None);
    }

    #[test]
    fn apostrophes_do_not_auto_close_in_prose() {
        assert_eq!(
            auto_close(LanguageId::Markdown, '\''),
            None,
            "typing \"don't\" must not produce \"don''t\""
        );
        assert_eq!(auto_close(LanguageId::PlainText, '\''), None);
        assert_eq!(auto_close(LanguageId::Python, '\''), Some('\''));
    }

    #[test]
    fn backticks_only_close_where_they_mean_something() {
        assert_eq!(auto_close(LanguageId::JavaScript, '`'), Some('`'));
        assert_eq!(auto_close(LanguageId::Markdown, '`'), Some('`'));
        assert_eq!(auto_close(LanguageId::Rust, '`'), None);
    }

    // ---- scanner ---------------------------------------------------------

    #[test]
    fn the_scanner_ignores_escaped_quotes() {
        // The string does not end at the escaped quote, so the bracket after it
        // is still inside the string.
        assert_eq!(
            enter_at_end("x = \"a \\\" ( b\"", LanguageId::Python),
            "",
            "an escaped quote must not end the string early"
        );
    }

    #[test]
    fn two_character_comment_tokens_are_recognised() {
        // Without multi-character comment handling, the `{` in this comment
        // would open a block and the next line would indent.
        assert_eq!(
            enter_at_end("    let x = 1; // note {", LanguageId::Rust),
            "    ",
            "a brace inside a // comment must not open a block"
        );
        assert_eq!(
            enter_at_end("    let x = 1; // see foo(", LanguageId::Rust),
            "    ",
            "nor should a bracket inside one start a continuation"
        );
    }

    #[test]
    fn a_lone_slash_is_not_a_comment() {
        let source = "let x = a / b * (c,";
        let indent = enter_at_end(source, LanguageId::Rust);
        assert_eq!(
            indent.len(),
            source.find('(').expect("bracket") + 1,
            "division must not be mistaken for the start of a comment"
        );
    }

    #[test]
    fn a_bracket_after_a_closed_string_still_counts() {
        let source = "x = f(\"text\", ";
        let indent = enter_at_end(source, LanguageId::Python);
        assert_eq!(indent.len(), source.find('(').expect("bracket") + 1);
    }

    #[test]
    fn deeply_nested_input_does_not_panic_or_hang() {
        let source = "f(".repeat(500);
        let _ = enter_at_end(&source, LanguageId::Python);

        let long = "x = 1\n".repeat(5_000) + "def f():";
        assert_eq!(enter_at_end(&long, LanguageId::Python), "    ");
    }

    #[test]
    fn an_empty_document_is_handled() {
        let rope = Rope::new();
        assert_eq!(new_line_indent(&rope, 0, LanguageId::Python, opts()), "");
        assert_eq!(
            dedent_after_typing(&rope, 0, LanguageId::Python, opts()),
            None
        );
    }
}
