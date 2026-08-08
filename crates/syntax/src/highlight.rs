//! Syntax highlighting.
//!
//! Grammars are compiled in and loaded once. Each open document owns a
//! [`Highlighter`] holding its parse tree; edits reparse **incrementally**, and
//! painting asks only for the spans inside the visible byte range. Both matter:
//! a full reparse per keystroke is affordable on a 500-line file and hopeless on
//! a 50,000-line one, and querying the whole document to paint 60 rows is the
//! same waste in the other direction.
//!
//! INI has no grammar on crates.io worth depending on, and does not need one —
//! it is strictly line-oriented, so it gets a small hand-written highlighter
//! through the same interface.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use editor_core::edit::Change;
use ropey::Rope;
use tree_sitter::{
    InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, TextProvider,
    Tree,
};

use crate::LanguageId;
use crate::theme::{Style, SyntaxTheme};

/// A styled run of text, in byte offsets.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

/// A loaded grammar and its highlight query.
struct Grammar {
    language: Language,
    query: Query,
}

/// Compiled grammars, loaded on first use of each language.
static GRAMMARS: OnceLock<HashMap<LanguageId, Grammar>> = OnceLock::new();

fn grammars() -> &'static HashMap<LanguageId, Grammar> {
    GRAMMARS.get_or_init(|| {
        let sources: [(LanguageId, Language, &str); 8] = [
            (
                LanguageId::Python,
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::Rust,
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::Json,
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::JavaScript,
                tree_sitter_javascript::LANGUAGE.into(),
                tree_sitter_javascript::HIGHLIGHT_QUERY,
            ),
            (
                LanguageId::Html,
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::Css,
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::Toml,
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            ),
            (
                LanguageId::Markdown,
                tree_sitter_md::LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            ),
        ];

        let mut map = HashMap::new();
        for (id, language, query_source) in sources {
            // A grammar whose query fails to compile is skipped rather than
            // fatal: that language falls back to plain text and everything else
            // keeps working. This can only happen if a grammar crate ships a
            // query its own parser rejects, but the editor must not refuse to
            // start over it.
            match Query::new(&language, query_source) {
                Ok(query) => {
                    map.insert(id, Grammar { language, query });
                }
                Err(e) => {
                    tracing::error!(?id, "highlight query failed to compile: {e}");
                }
            }
        }
        map
    })
}

/// True if this language is highlighted at all.
#[must_use]
pub fn is_supported(language: LanguageId) -> bool {
    language == LanguageId::Ini || grammars().contains_key(&language)
}

/// How long a reparse may take while somebody is typing.
///
/// Comfortably inside one frame at 60 Hz. An incremental reparse of an edit
/// that leaves the file parseable takes microseconds and never comes near
/// this; an edit that breaks the syntax can cost hundreds of milliseconds,
/// because tree-sitter then has almost nothing to reuse and pays for the
/// attempt on top of the reparse. Measured on a 10,000-line Rust file, a
/// character typed at the start of a line cost 117 ms against 39 ms for a
/// parse from scratch — a freeze per keystroke.
const TYPING_BUDGET: Duration = Duration::from_millis(8);

/// How long the catch-up reparse may take once typing has stopped.
///
/// Generous, because by this point nobody is waiting on a keystroke, and
/// giving up here would leave the highlighting wrong until the next edit.
const IDLE_BUDGET: Duration = Duration::from_secs(2);

/// Per-document highlighting state.
pub enum Highlighter {
    /// Grammar-backed, holding a parse tree.
    Tree(Box<TreeHighlighter>),
    /// Line-oriented fallback for INI.
    Ini,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tree(t) => f
                .debug_struct("Highlighter::Tree")
                .field("language", &t.language)
                .field("parsed", &t.tree.is_some())
                .field("stale", &t.stale)
                .finish(),
            Self::Ini => f.write_str("Highlighter::Ini"),
        }
    }
}

impl Highlighter {
    /// Create a highlighter for a language, or `None` if it has none.
    #[must_use]
    pub fn new(language: LanguageId, text: &Rope) -> Option<Self> {
        if language == LanguageId::Ini {
            return Some(Self::Ini);
        }
        let grammar = grammars().get(&language)?;
        let mut parser = Parser::new();
        parser.set_language(&grammar.language).ok()?;

        let mut this = TreeHighlighter {
            language,
            parser,
            tree: None,
            stale: false,
        };
        // Opening a file is not a keystroke, so it gets the generous budget.
        this.parse_within(text, IDLE_BUDGET);
        Some(Self::Tree(Box::new(this)))
    }

    /// True when the last reparse ran out of time and the tree is behind the
    /// text.
    ///
    /// The caller should paint anyway — an edited tree still reports sensible
    /// positions, so the colours are a moment stale rather than wrong — and
    /// call [`Self::catch_up`] once typing pauses.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        match self {
            Self::Tree(t) => t.stale,
            Self::Ini => false,
        }
    }

    /// Finish the reparse that typing did not have time for.
    ///
    /// Does nothing unless the tree is actually stale, so this is safe to call
    /// every idle frame. Returns true if it caught up, so the caller knows the
    /// highlighting and the syntax errors are worth recomputing.
    pub fn catch_up(&mut self, text: &Rope) -> bool {
        let Self::Tree(t) = self else { return false };
        if !t.stale {
            return false;
        }
        t.parse_within(text, IDLE_BUDGET);
        !t.stale
    }

    /// Update after an edit.
    ///
    /// Single-edit transactions — all ordinary typing — reparse incrementally.
    /// Multi-edit transactions (multi-cursor, replace-all) fall back to a full
    /// reparse: computing each edit's position in a document that the other
    /// edits are simultaneously shifting is easy to get subtly wrong, and those
    /// operations are rare enough that the cost does not matter.
    pub fn update(&mut self, changes: &[Change], text: &Rope) {
        let Self::Tree(t) = self else { return };
        match changes {
            [] => {}
            [change] => t.apply_change(change, text),
            _ => t.parse_within(text, TYPING_BUDGET),
        }
    }

    /// Recompute from scratch — after undo, redo, or a reload from disk.
    ///
    /// None of those are keystrokes, so this gets the generous budget: a
    /// reload that came out half-highlighted would stay that way.
    pub fn refresh(&mut self, text: &Rope) {
        if let Self::Tree(t) = self {
            t.tree = None;
            t.parse_within(text, IDLE_BUDGET);
        }
    }

    /// The parse tree, for callers that need to read structure rather than
    /// colour — `symbols`, and the error walk below.
    ///
    /// `None` for the INI fallback, which has no grammar behind it.
    #[must_use]
    pub fn tree(&self) -> Option<&tree_sitter::Tree> {
        match self {
            Self::Tree(t) => t.tree.as_ref(),
            Self::Ini => None,
        }
    }

    /// Where the parser lost the thread, from the tree it already holds.
    ///
    /// This is what makes broken code visible with no language server
    /// installed. The line-based INI fallback has no tree and reports nothing.
    #[must_use]
    pub fn errors(&self, text: &Rope) -> Vec<crate::errors::SyntaxError> {
        match self {
            Self::Tree(t) => t
                .tree
                .as_ref()
                .map(|tree| crate::errors::from_tree(tree, text))
                .unwrap_or_default(),
            Self::Ini => Vec::new(),
        }
    }

    /// Styled runs covering `byte_range`, in ascending order and non-
    /// overlapping. Bytes with no capture are omitted; the caller paints those
    /// in the theme's default colour.
    #[must_use]
    pub fn spans(
        &mut self,
        text: &Rope,
        byte_range: Range<usize>,
        theme: &SyntaxTheme,
    ) -> Vec<Span> {
        match self {
            Self::Tree(t) => t.spans(text, byte_range, theme),
            Self::Ini => ini_spans(text, byte_range, theme),
        }
    }
}

/// Grammar-backed highlighting for one document.
pub struct TreeHighlighter {
    language: LanguageId,
    parser: Parser,
    tree: Option<Tree>,
    /// Set when a reparse ran out of time, so the tree is behind the text.
    stale: bool,
}

impl std::fmt::Debug for TreeHighlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Parser` is not Debug, and dumping a parse tree is never what anyone
        // wants in a log line.
        f.debug_struct("TreeHighlighter")
            .field("language", &self.language)
            .field("parsed", &self.tree.is_some())
            .finish()
    }
}

impl TreeHighlighter {
    /// Parse, giving up if it takes longer than `budget`.
    ///
    /// On giving up, whatever tree we already have is kept and `stale` is set.
    /// Throwing it away would be worse than keeping a slightly out-of-date one:
    /// an edited tree still reports positions that line up with the text, so
    /// the colours lag by a keystroke, whereas no tree at all means no colours,
    /// no bracket matching and no structure until the next successful parse.
    fn parse_within(&mut self, text: &Rope, budget: Duration) {
        let deadline = Instant::now() + budget;
        // Called by tree-sitter as it works; the only thing it can do is say
        // "stop". Checking the clock each time is cheap next to the parsing
        // between calls.
        let mut out_of_time = move |_: &tree_sitter::ParseState| {
            if Instant::now() < deadline {
                std::ops::ControlFlow::Continue(())
            } else {
                std::ops::ControlFlow::Break(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut out_of_time);

        match parse(&mut self.parser, text, self.tree.as_ref(), Some(options)) {
            Some(tree) => {
                self.tree = Some(tree);
                self.stale = false;
            }
            None => {
                self.stale = true;
                tracing::debug!(
                    language = ?self.language,
                    bytes = text.len_bytes(),
                    "reparse ran out of time; highlighting is a keystroke behind"
                );
            }
        }
    }

    fn apply_change(&mut self, change: &Change, text: &Rope) {
        let Some(tree) = self.tree.as_mut() else {
            self.parse_within(text, TYPING_BUDGET);
            return;
        };

        // Every offset below is derived from the *new* rope. That is valid
        // because everything before the edit is byte-identical in both, so the
        // start position is the same in either; the two end positions are then
        // reached by walking the removed and inserted text.
        let start_byte = text.char_to_byte(change.range.start.min(text.len_chars()));
        let old_end_byte = start_byte + change.removed.len();
        let new_end_byte = start_byte + change.inserted.len();

        let start_position = point_at(text, start_byte);
        let edit = InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position: advance(start_position, &change.removed),
            new_end_position: advance(start_position, &change.inserted),
        };

        tree.edit(&edit);
        self.parse_within(text, TYPING_BUDGET);
    }

    fn spans(&mut self, text: &Rope, byte_range: Range<usize>, theme: &SyntaxTheme) -> Vec<Span> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let Some(grammar) = grammars().get(&self.language) else {
            return Vec::new();
        };

        let end = byte_range.end.min(text.len_bytes());
        let start = byte_range.start.min(end);
        if start == end {
            return Vec::new();
        }

        // Resolve each capture index to a style once, rather than per match.
        let styles: Vec<Style> = grammar
            .query
            .capture_names()
            .iter()
            .map(|name| theme.style_for(name))
            .collect();
        let covered: Vec<bool> = grammar
            .query
            .capture_names()
            .iter()
            .map(|name| theme.covers(name))
            .collect();

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start..end);

        // Collect every capture before deciding anything, because the order the
        // cursor emits them in is *not* the precedence order.
        //
        // Matches arrive sorted by the start of the matched node. A pattern
        // like `(function_definition name: (identifier) @function)` matches a
        // node starting at `def`, so it is emitted before the bare
        // `(identifier) @variable` that matches the name itself — and painting
        // in arrival order therefore lets the catch-all win. Every function
        // name in a Python file came out as a plain variable.
        //
        // The two rules that actually apply:
        //   * a smaller node wins over a larger one containing it, so an escape
        //     sequence keeps its colour inside a string;
        //   * for the same node, the *later* pattern in the query wins, which
        //     is the convention every highlights.scm is written to — general
        //     rules first, specific ones after.
        let mut captures: Vec<(Range<usize>, usize, u32)> = Vec::new();
        let mut matches = cursor.matches(&grammar.query, tree.root_node(), RopeProvider(text));
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if !covered
                    .get(capture.index as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    continue;
                }
                captures.push((capture.node.byte_range(), m.pattern_index, capture.index));
            }
        }

        // Paint widest first, then by ascending pattern index, so the last
        // write for any byte is the narrowest and highest-priority capture.
        captures.sort_by_key(|(range, pattern, _)| {
            (std::cmp::Reverse(range.end - range.start), *pattern)
        });

        // One slot per byte in the window, holding the winning capture.
        let width = end - start;
        let mut owner: Vec<Option<u32>> = vec![None; width];

        for (range, _, capture_index) in captures {
            let from = range.start.clamp(start, end) - start;
            let to = range.end.clamp(start, end) - start;
            for slot in owner.iter_mut().take(to).skip(from) {
                *slot = Some(capture_index);
            }
        }

        // Compress equal neighbours into runs.
        let mut spans: Vec<Span> = Vec::new();
        let mut run_start = 0usize;
        while run_start < width {
            let current = owner[run_start];
            let mut run_end = run_start + 1;
            while run_end < width && owner[run_end] == current {
                run_end += 1;
            }
            if let Some(index) = current
                && let Some(style) = styles.get(index as usize)
            {
                spans.push(Span {
                    range: start + run_start..start + run_end,
                    style: *style,
                });
            }
            run_start = run_end;
        }
        spans
    }
}

/// Parse a rope without copying it into a contiguous `String`.
fn parse(
    parser: &mut Parser,
    text: &Rope,
    old: Option<&Tree>,
    options: Option<tree_sitter::ParseOptions<'_>>,
) -> Option<Tree> {
    parser.parse_with_options(
        &mut |byte, _| {
            if byte >= text.len_bytes() {
                return &[][..];
            }
            let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        },
        old,
        options,
    )
}

/// Feeds node text to query predicates straight from the rope.
struct RopeProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&str) -> &[u8]>;

    fn text(&mut self, node: Node<'_>) -> Self::I {
        let range = node.byte_range();
        let end = range.end.min(self.0.len_bytes());
        let start = range.start.min(end);
        self.0.byte_slice(start..end).chunks().map(str::as_bytes)
    }
}

fn point_at(text: &Rope, byte: usize) -> Point {
    let byte = byte.min(text.len_bytes());
    let line = text.byte_to_line(byte);
    Point::new(line, byte - text.line_to_byte(line))
}

/// Where `start` ends up after `text` is written at it.
fn advance(start: Point, text: &str) -> Point {
    match text.rfind('\n') {
        Some(last) => Point::new(
            start.row + text.matches('\n').count(),
            text.len() - last - 1,
        ),
        None => Point::new(start.row, start.column + text.len()),
    }
}

/// Line-oriented highlighting for INI files.
///
/// Comments (`;` or `#`), `[sections]`, and `key = value`. That is the whole
/// format, so a parser would buy nothing.
fn ini_spans(text: &Rope, byte_range: Range<usize>, theme: &SyntaxTheme) -> Vec<Span> {
    let comment = theme.style_for("comment");
    let section = theme.style_for("type");
    let key = theme.style_for("property");
    let operator = theme.style_for("operator");
    let value = theme.style_for("string");

    let end = byte_range.end.min(text.len_bytes());
    let start = byte_range.start.min(end);
    if start == end {
        return Vec::new();
    }

    let first_line = text.byte_to_line(start);
    let last_line = text.byte_to_line(end.saturating_sub(1));
    let mut spans = Vec::new();

    for line_index in first_line..=last_line.min(text.len_lines().saturating_sub(1)) {
        let line_start = text.line_to_byte(line_index);
        let line = text.line(line_index).to_string();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let indent = trimmed.len() - trimmed.trim_start().len();
        let body = trimmed.trim_start();

        if body.is_empty() {
            continue;
        }

        let base = line_start + indent;

        if body.starts_with(';') || body.starts_with('#') {
            spans.push(Span {
                range: base..base + body.len(),
                style: comment,
            });
        } else if body.starts_with('[') {
            spans.push(Span {
                range: base..base + body.len(),
                style: section,
            });
        } else if let Some(eq) = body.find(['=', ':']) {
            spans.push(Span {
                range: base..base + eq,
                style: key,
            });
            spans.push(Span {
                range: base + eq..base + eq + 1,
                style: operator,
            });
            if eq + 1 < body.len() {
                spans.push(Span {
                    range: base + eq + 1..base + body.len(),
                    style: value,
                });
            }
        }
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_config::theme::ResolvedTheme;

    fn theme() -> SyntaxTheme {
        SyntaxTheme::for_ui(ResolvedTheme::Dark)
    }

    /// The captured text of every span, for readable assertions.
    fn styled(source: &str, language: LanguageId) -> Vec<(String, Style)> {
        let rope = Rope::from_str(source);
        let mut h = Highlighter::new(language, &rope).expect("language is supported");
        h.spans(&rope, 0..rope.len_bytes(), &theme())
            .into_iter()
            .map(|s| (source[s.range].to_owned(), s.style))
            .collect()
    }

    fn text_with_style(source: &str, language: LanguageId, capture: &str) -> Vec<String> {
        let want = theme().style_for(capture);
        styled(source, language)
            .into_iter()
            .filter(|(_, style)| *style == want)
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn every_shipped_language_either_has_a_grammar_or_is_deliberately_plain() {
        for language in LanguageId::ALL {
            let supported = is_supported(language);
            match language {
                LanguageId::PlainText => {
                    assert!(!supported, "plain text must not be highlighted");
                }
                other => assert!(supported, "{other:?} has no highlighting"),
            }
        }
    }

    #[test]
    fn python_keywords_strings_and_comments_are_distinguished() {
        let source = "def greet(name):\n    # say hello\n    return f\"hi {name}\"\n";

        let keywords = text_with_style(source, LanguageId::Python, "keyword");
        assert!(keywords.contains(&"def".to_owned()), "got {keywords:?}");
        assert!(keywords.contains(&"return".to_owned()), "got {keywords:?}");

        let comments = text_with_style(source, LanguageId::Python, "comment");
        assert!(
            comments.iter().any(|c| c.contains("say hello")),
            "got {comments:?}"
        );
    }

    #[test]
    fn python_function_names_are_styled_separately_from_keywords() {
        let source = "def greet(name):\n    pass\n";
        let functions = text_with_style(source, LanguageId::Python, "function");
        assert!(functions.contains(&"greet".to_owned()), "got {functions:?}");
    }

    #[test]
    fn rust_is_highlighted() {
        let source = "fn main() {\n    // hello\n    let x = 42;\n}\n";

        let keywords = text_with_style(source, LanguageId::Rust, "keyword");
        assert!(keywords.contains(&"fn".to_owned()), "got {keywords:?}");
        assert!(keywords.contains(&"let".to_owned()), "got {keywords:?}");

        let comments = text_with_style(source, LanguageId::Rust, "comment");
        assert!(
            comments.iter().any(|c| c.contains("hello")),
            "got {comments:?}"
        );
    }

    #[test]
    fn json_html_css_and_toml_all_produce_spans() {
        for (source, language) in [
            (r#"{"key": "value", "n": 1}"#, LanguageId::Json),
            ("<p class=\"x\">hi</p>", LanguageId::Html),
            ("body { color: red; }", LanguageId::Css),
            ("[package]\nname = \"x\"\n", LanguageId::Toml),
            ("const x = 1; // note", LanguageId::JavaScript),
            ("# Heading\n\ntext\n", LanguageId::Markdown),
        ] {
            let spans = styled(source, language);
            assert!(!spans.is_empty(), "{language:?} produced no highlighting");
        }
    }

    #[test]
    fn spans_are_ordered_and_never_overlap() {
        let source = "def f(a, b=1):\n    \"\"\"doc\"\"\"\n    return a + b  # sum\n";
        let rope = Rope::from_str(source);
        let mut h = Highlighter::new(LanguageId::Python, &rope).expect("python");
        let spans = h.spans(&rope, 0..rope.len_bytes(), &theme());

        let mut previous_end = 0;
        for span in &spans {
            assert!(span.range.start >= previous_end, "spans overlap: {spans:?}");
            assert!(span.range.start < span.range.end, "empty span");
            previous_end = span.range.end;
        }
    }

    #[test]
    fn only_the_requested_window_is_returned() {
        // A long file, highlighted through a narrow window: nothing outside it
        // may come back, or painting cost would scale with file size.
        let source = "def f():\n    pass\n".repeat(500);
        let rope = Rope::from_str(&source);
        let mut h = Highlighter::new(LanguageId::Python, &rope).expect("python");

        let window = 100..400;
        let spans = h.spans(&rope, window.clone(), &theme());
        assert!(!spans.is_empty());
        for span in spans {
            assert!(
                span.range.start >= window.start && span.range.end <= window.end,
                "{span:?} escapes the requested window"
            );
        }
    }

    #[test]
    fn an_incremental_edit_gives_the_same_result_as_a_full_reparse() {
        let before = "def f():\n    return 1\n";
        let after = "def f():\n    return 100\n";

        let rope_after = Rope::from_str(after);

        // Incremental: parse `before`, then apply the edit.
        let mut incremental =
            Highlighter::new(LanguageId::Python, &Rope::from_str(before)).expect("python");
        let insert_at = before.find('1').expect("digit present");
        incremental.update(
            &[Change {
                range: insert_at + 1..insert_at + 1,
                removed: String::new(),
                inserted: "00".to_owned(),
            }],
            &rope_after,
        );

        // Full: parse `after` from scratch.
        let mut full = Highlighter::new(LanguageId::Python, &rope_after).expect("python");

        assert_eq!(
            incremental.spans(&rope_after, 0..rope_after.len_bytes(), &theme()),
            full.spans(&rope_after, 0..rope_after.len_bytes(), &theme()),
            "incremental reparse drifted from a clean parse"
        );
    }

    #[test]
    fn an_edit_spanning_lines_reparses_correctly() {
        let before = "def f():\n    pass\n";
        let after = "def f():\n    if True:\n        pass\n";
        let rope_after = Rope::from_str(after);

        let mut incremental =
            Highlighter::new(LanguageId::Python, &Rope::from_str(before)).expect("python");
        // Replace "    pass" with "    if True:\n        pass".
        incremental.update(
            &[Change {
                range: 9..17,
                removed: "    pass".to_owned(),
                inserted: "    if True:\n        pass".to_owned(),
            }],
            &rope_after,
        );

        let mut full = Highlighter::new(LanguageId::Python, &rope_after).expect("python");
        assert_eq!(
            incremental.spans(&rope_after, 0..rope_after.len_bytes(), &theme()),
            full.spans(&rope_after, 0..rope_after.len_bytes(), &theme()),
        );
    }

    #[test]
    fn multibyte_text_does_not_shift_the_highlighting() {
        // Byte and character offsets diverge here; getting this wrong colours
        // half an identifier.
        let source = "# caf\u{e9} \u{1f600}\ndef f():\n    return \"na\u{ef}ve\"\n";
        let rope = Rope::from_str(source);
        let mut h = Highlighter::new(LanguageId::Python, &rope).expect("python");
        let spans = h.spans(&rope, 0..rope.len_bytes(), &theme());

        for span in &spans {
            assert!(
                source.is_char_boundary(span.range.start)
                    && source.is_char_boundary(span.range.end),
                "{span:?} splits a character"
            );
        }
        let keywords = text_with_style(source, LanguageId::Python, "keyword");
        assert!(keywords.contains(&"def".to_owned()), "got {keywords:?}");
    }

    #[test]
    fn ini_highlights_comments_sections_and_keys() {
        let source = "; a comment\n[section]\nkey = value\n";
        let rope = Rope::from_str(source);
        let mut h = Highlighter::new(LanguageId::Ini, &rope).expect("ini");
        let spans = h.spans(&rope, 0..rope.len_bytes(), &theme());

        let text_of = |s: &Span| source[s.range.clone()].to_owned();
        let t = theme();

        assert!(
            spans
                .iter()
                .any(|s| s.style == t.style_for("comment") && text_of(s).contains("a comment")),
            "got {spans:?}"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.style == t.style_for("type") && text_of(s) == "[section]"),
            "got {spans:?}"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.style == t.style_for("property") && text_of(s).trim() == "key"),
            "got {spans:?}"
        );
    }

    #[test]
    fn ini_ignores_blank_lines_without_panicking() {
        let source = "\n\n[a]\n\n\nk=v\n\n";
        let rope = Rope::from_str(source);
        let mut h = Highlighter::new(LanguageId::Ini, &rope).expect("ini");
        let spans = h.spans(&rope, 0..rope.len_bytes(), &theme());
        assert!(!spans.is_empty());
    }

    #[test]
    fn an_empty_document_yields_no_spans_and_does_not_panic() {
        for language in [LanguageId::Python, LanguageId::Rust, LanguageId::Ini] {
            let rope = Rope::new();
            let mut h = Highlighter::new(language, &rope).expect("supported");
            assert!(h.spans(&rope, 0..0, &theme()).is_empty());
        }
    }

    #[test]
    fn a_window_beyond_the_end_of_the_document_is_clamped() {
        let rope = Rope::from_str("x = 1\n");
        let mut h = Highlighter::new(LanguageId::Python, &rope).expect("python");
        let spans = h.spans(&rope, 0..999_999, &theme());
        for span in spans {
            assert!(span.range.end <= rope.len_bytes());
        }
    }

    #[test]
    fn syntactically_broken_code_still_highlights_what_it_can() {
        // Half-typed code is the normal state of a file being edited; the
        // highlighter must degrade rather than go blank.
        let source = "def f(:\n    return\n";
        let spans = styled(source, LanguageId::Python);
        assert!(!spans.is_empty(), "a parse error blanked the whole file");
    }

    #[test]
    fn point_advance_handles_single_and_multi_line_text() {
        let start = Point::new(3, 5);
        assert_eq!(advance(start, "abc"), Point::new(3, 8));
        assert_eq!(advance(start, "abc\ndef"), Point::new(4, 3));
        assert_eq!(advance(start, "\n"), Point::new(4, 0));
        assert_eq!(advance(start, ""), start);
    }

    /// A big file that takes real work to parse, so a zero budget genuinely
    /// runs out rather than finishing before the parser looks at the clock.
    fn big_rust() -> Rope {
        let block =
            "fn f(a: u32) -> u32 {\n    a + 1 // note\n}\n\nstruct S {\n    x: String,\n}\n\n";
        Rope::from_str(&block.repeat(4_000))
    }

    /// The point of the budget: a reparse that runs out of time keeps the tree
    /// it already has rather than throwing it away. No tree means no colours,
    /// no bracket matching and no structure — far worse than colours that are
    /// a keystroke behind.
    #[test]
    fn a_reparse_that_runs_out_of_time_keeps_the_previous_tree() {
        let text = big_rust();
        let mut highlighter =
            Highlighter::new(LanguageId::Rust, &text).expect("Rust has a grammar");
        assert!(!highlighter.is_stale(), "the first parse had time");
        assert!(highlighter.tree().is_some());

        let Highlighter::Tree(inner) = &mut highlighter else {
            panic!("expected a tree-backed highlighter");
        };
        inner.parse_within(&text, Duration::ZERO);

        assert!(highlighter.is_stale(), "no time, so it gave up");
        assert!(
            highlighter.tree().is_some(),
            "and kept the tree it already had"
        );
        assert!(
            !highlighter
                .spans(
                    &text,
                    0..200,
                    &SyntaxTheme::for_ui(editor_config::theme::ResolvedTheme::Dark)
                )
                .is_empty(),
            "a stale tree still highlights"
        );
    }

    #[test]
    fn catching_up_clears_the_staleness() {
        let text = big_rust();
        let mut highlighter =
            Highlighter::new(LanguageId::Rust, &text).expect("Rust has a grammar");
        let Highlighter::Tree(inner) = &mut highlighter else {
            panic!("expected a tree-backed highlighter");
        };
        inner.parse_within(&text, Duration::ZERO);
        assert!(highlighter.is_stale());

        assert!(highlighter.catch_up(&text), "it caught up");
        assert!(!highlighter.is_stale());
        // Safe to call every idle frame: nothing to do, and it says so.
        assert!(!highlighter.catch_up(&text), "nothing left to catch up on");
    }

    /// The fallback highlighter has no parser, so it can never fall behind.
    #[test]
    fn the_ini_fallback_is_never_stale() {
        let mut ini = Highlighter::Ini;
        assert!(!ini.is_stale());
        assert!(!ini.catch_up(&Rope::from_str("[a]\nb = 1\n")));
    }
}
