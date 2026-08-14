//! Matching brackets, and the ranges a file can be folded at.
//!
//! Both come from the parse tree rather than from counting characters. A
//! counter cannot tell a brace in a string from one in the code, so it goes
//! wrong on the first `print("}")` — and it goes wrong silently, which is
//! worse than not having the feature.
//!
//! Folding is derived from node extents rather than from a per-language query:
//! anything spanning more than one line and holding something is foldable, and
//! that covers a function body in Python, a block in Rust, a JSON object and an
//! HTML element without a line of configuration for any of them.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Node, Tree};

/// The two halves of a bracket pair, in character offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BracketPair {
    pub open: Range<usize>,
    pub close: Range<usize>,
}

/// A range that can be collapsed, in zero-based lines.
///
/// `first` is the line that stays visible and carries the fold marker; the
/// lines after it up to and including `last` are what disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FoldRange {
    pub first: usize,
    pub last: usize,
}

/// Bracket characters worth matching, as (open, close).
const PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];

/// The pair enclosing or adjacent to `offset`.
///
/// Checks the character before the caret as well as the one after it, because
/// a caret sitting just past a `)` is what you have after typing one, and that
/// is exactly when the match is worth seeing.
#[must_use]
pub fn match_at(tree: &Tree, text: &Rope, offset: usize) -> Option<BracketPair> {
    let len = text.len_chars();
    let offset = offset.min(len);

    for probe in [offset, offset.saturating_sub(1)] {
        if probe >= len {
            continue;
        }
        let c = text.char(probe);
        let is_bracket = PAIRS.iter().any(|(o, cl)| *o == c || *cl == c);
        if !is_bracket {
            continue;
        }
        if let Some(pair) = pair_around(tree, text, probe) {
            return Some(pair);
        }
    }
    None
}

/// Find the node whose first and last characters are the bracket at `offset`.
fn pair_around(tree: &Tree, text: &Rope, offset: usize) -> Option<BracketPair> {
    let byte = text.char_to_byte(offset);
    // The smallest node containing this byte is the bracket token itself or
    // its parent; the pair lives on whichever of them starts and ends with a
    // bracket.
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte + 1)?;

    for _ in 0..3 {
        if let Some(pair) = ends_are_brackets(&node, text) {
            let inside = pair.open.contains(&offset) || pair.close.contains(&offset);
            if inside {
                return Some(pair);
            }
        }
        node = node.parent()?;
    }
    None
}

/// A pair if this node begins with an opener and ends with its closer.
fn ends_are_brackets(node: &Node<'_>, text: &Rope) -> Option<BracketPair> {
    let max = text.len_bytes();
    let start = text.byte_to_char(node.start_byte().min(max));
    let end = text.byte_to_char(node.end_byte().min(max));
    if end <= start || end > text.len_chars() {
        return None;
    }

    let first = text.char(start);
    let last = text.char(end - 1);
    PAIRS
        .iter()
        .any(|(open, close)| *open == first && *close == last)
        .then(|| BracketPair {
            open: start..start + 1,
            close: end - 1..end,
        })
}

/// Every range in the file that can be folded, outermost first.
///
/// A node qualifies when it spans more than one line and has children — which
/// is a function body, a block, an object, an element, without naming any of
/// them.
///
/// Three refinements, all learned by looking at the output. The root is
/// excluded, or the first line of every file offers to fold the whole file
/// away. Where several nodes start on one line the *narrowest* wins: a `def`
/// line begins both the definition and, in Python, the body block that runs
/// past the end of the `if` inside it, and folding at `if True:` should
/// collapse the `if`, not everything after it.
///
/// And Python's `block` is skipped, because it is the body of the `def` or
/// `if` above it and starts on the body's first statement: folding it would
/// leave a chevron beside `x = 1` and hide only what came after. Fold the
/// header instead.
#[must_use]
pub fn fold_ranges(tree: &Tree, text: &Rope) -> Vec<FoldRange> {
    let mut found: Vec<FoldRange> = Vec::new();
    let mut cursor = tree.root_node().walk();
    let mut stack = vec![tree.root_node()];
    let max = text.len_bytes();

    let root = tree.root_node().id();
    while let Some(node) = stack.pop() {
        let start = text.byte_to_char(node.start_byte().min(max));
        let end = text.byte_to_char(node.end_byte().min(max));
        let first = text.char_to_line(start.min(text.len_chars()));
        let last = text.char_to_line(end.saturating_sub(1).min(text.len_chars()));

        // Python's `block` is the body of the `def`, `if` or `for` above it,
        // and it starts on the body's first statement rather than on the
        // header. Folding it would put a chevron beside `x = 1` and hide only
        // what came after, so the header is the fold anyone means.
        //
        // Named rather than inferred. The obvious inference — "starts after
        // its parent and ends with it" — is wrong, because the *last* child of
        // any block also ends where the block ends: it dropped the fold for
        // every final method in a class. In Rust a `block` shares its header's
        // line and is deduplicated anyway, so naming it costs nothing there.
        let is_a_body = node.kind() == "block";

        if last > first && node.child_count() > 0 && node.id() != root && !is_a_body {
            found.push(FoldRange { first, last });
        }

        cursor.reset(node);
        if cursor.goto_first_child() {
            loop {
                stack.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Narrowest first, then one per starting line.
    found.sort_by(|a, b| a.first.cmp(&b.first).then(a.last.cmp(&b.last)));
    found.dedup_by_key(|r| r.first);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageId;
    use crate::highlight::Highlighter;

    fn with_tree<T>(language: LanguageId, source: &str, f: impl Fn(&Tree, &Rope) -> T) -> T {
        let rope = Rope::from_str(source);
        let highlighter = Highlighter::new(language, &rope).expect("grammar");
        f(highlighter.tree().expect("tree"), &rope)
    }

    #[test]
    fn a_caret_on_an_opening_bracket_finds_its_partner() {
        //           0123456789
        let source = "f(a, b)\n";
        with_tree(LanguageId::Python, source, |tree, text| {
            let pair = match_at(tree, text, 1).expect("a pair");
            assert_eq!(pair.open, 1..2);
            assert_eq!(pair.close, 6..7);
        });
    }

    #[test]
    fn a_caret_just_past_a_closing_bracket_finds_it_too() {
        // Where the caret is the moment after typing the closer, which is when
        // seeing the match is most useful.
        with_tree(LanguageId::Python, "f(a, b)\n", |tree, text| {
            let pair = match_at(tree, text, 7).expect("a pair");
            assert_eq!(pair.open, 1..2);
            assert_eq!(pair.close, 6..7);
        });
    }

    #[test]
    fn nested_brackets_match_their_own_partner() {
        //           0123456789012
        let source = "f(g(x), y)\n";
        with_tree(LanguageId::Python, source, |tree, text| {
            let inner = match_at(tree, text, 3).expect("inner");
            assert_eq!(inner.open, 3..4);
            assert_eq!(inner.close, 5..6);
        });
    }

    #[test]
    fn a_bracket_inside_a_string_is_not_matched_against_the_code() {
        // The reason this works from the tree. A counter pairs the `)` in the
        // string with the call's `(` and highlights nonsense.
        with_tree(LanguageId::Python, "print(\")\")\n", |tree, text| {
            let pair = match_at(tree, text, 5).expect("the call's own pair");
            assert_eq!(pair.open, 5..6);
            assert_eq!(
                pair.close,
                9..10,
                "the real closer, not the one in the string"
            );
        });
    }

    #[test]
    fn a_caret_away_from_any_bracket_matches_nothing() {
        with_tree(LanguageId::Python, "x = 1\n", |tree, text| {
            assert!(match_at(tree, text, 2).is_none());
        });
    }

    #[test]
    fn an_unclosed_bracket_has_no_partner() {
        with_tree(LanguageId::Python, "f(a\n", |tree, text| {
            let found = match_at(tree, text, 1);
            assert!(found.is_none(), "got {found:?}");
        });
    }

    #[test]
    fn rust_braces_match() {
        with_tree(LanguageId::Rust, "fn f() { g(); }\n", |tree, text| {
            let pair = match_at(tree, text, 7).expect("the block");
            assert_eq!(pair.open, 7..8);
            assert_eq!(pair.close, 14..15);
        });
    }

    const PY: &str = "\
def outer():
    if True:
        return 1
    return 2


class A:
    pass
";

    #[test]
    fn a_function_body_is_foldable() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            assert!(
                folds.iter().any(|f| f.first == 0 && f.last == 3),
                "the def should fold lines 0 to 3: {folds:?}"
            );
        });
    }

    #[test]
    fn a_nested_block_folds_separately() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            assert!(
                folds.iter().any(|f| f.first == 1 && f.last == 2),
                "the if should fold from line 1: {folds:?}"
            );
        });
    }

    #[test]
    fn one_marker_per_line_even_where_several_nodes_start_there() {
        // A `def` line begins the definition *and* its parameters and body;
        // three markers on one line is three ways to do the same thing.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            let mut firsts: Vec<usize> = folds.iter().map(|f| f.first).collect();
            let before = firsts.len();
            firsts.sort_unstable();
            firsts.dedup();
            assert_eq!(firsts.len(), before, "two folds share a first line");
        });
    }

    #[test]
    fn the_narrowest_fold_on_a_line_is_the_one_kept() {
        // A `def` line starts the definition and, in Python, the body block
        // that reaches past the `if` inside it. Folding at `if True:` should
        // collapse the `if`, not everything below it.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            let on_one = folds.iter().find(|f| f.first == 1).expect("line 1");
            assert_eq!(on_one.last, 2, "folded past the end of the `if`");
        });
    }

    #[test]
    fn the_whole_file_is_never_offered_as_a_fold() {
        // The root node spans every line, so without excluding it the first
        // line of every file offers to fold the file away.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            let lines = text.len_lines();
            assert!(
                !folds.iter().any(|f| f.first == 0 && f.last >= lines - 1),
                "the root is foldable: {folds:?}"
            );
        });
    }

    #[test]
    fn a_single_line_file_folds_nowhere() {
        with_tree(LanguageId::Python, "x = 1\n", |tree, text| {
            assert!(fold_ranges(tree, text).is_empty());
        });
    }

    #[test]
    fn json_and_rust_fold_without_a_query_each() {
        // The point of deriving this from node extents rather than naming the
        // constructs of every language.
        with_tree(
            LanguageId::Json,
            "{\n  \"a\": [\n    1\n  ]\n}\n",
            |tree, text| {
                assert!(!fold_ranges(tree, text).is_empty());
            },
        );
        with_tree(LanguageId::Rust, "fn f() {\n    g();\n}\n", |tree, text| {
            assert!(fold_ranges(tree, text).iter().any(|f| f.first == 0));
        });
    }

    #[test]
    fn folds_come_back_in_document_order() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let folds = fold_ranges(tree, text);
            assert!(
                folds.windows(2).all(|w| w[0].first <= w[1].first),
                "{folds:?}"
            );
        });
    }
}
