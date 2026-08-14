//! Finding what an identifier is and where else it appears, from the parse tree.
//!
//! This is the fallback for Go to Definition and Find Uses when no language
//! server is running. It is deliberately modest about what it claims: a parser
//! knows the shape of the code but nothing about scope, imports or types, so it
//! can find `def parse` in the open file and every place the word `parse`
//! appears as an identifier — and that is all. Anything involving another file,
//! a method on a type, or a name that is shadowed needs a real language server.
//!
//! Working from the tree rather than a text search is what makes it worth
//! having: `parse` inside a string literal or a comment is not a use of the
//! function, and a regex cannot tell the difference.
//!
//! Definitions are found by field name rather than by a per-language query.
//! Every grammar worth supporting names the thing being defined in a field
//! called `name` — Python's `function_definition` and `class_definition`, Rust's
//! `function_item`, `struct_item`, `enum_item`, `trait_item`, `mod_item` — so
//! one rule covers both languages and any grammar added later, instead of a
//! query per language that has to be written before the feature works at all.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Node, Tree};

/// An identifier and where it sits, in character offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub range: Range<usize>,
}

/// A place a symbol appears, in character offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub range: Range<usize>,
    /// True if this is where the name is defined rather than used.
    pub is_definition: bool,
}

/// Node kinds that count as an identifier across the grammars in use.
///
/// Rust distinguishes `type_identifier` and `field_identifier` from plain
/// `identifier`; Python uses `identifier` for everything.
fn is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "type_identifier" | "field_identifier" | "constant" | "property_identifier"
    )
}

/// The identifier under `offset`, if there is one.
///
/// Accepts a caret sitting at either end of the word, because that is where it
/// lands after double-clicking or arrowing to the end of a name.
#[must_use]
pub fn identifier_at(tree: &Tree, text: &Rope, offset: usize) -> Option<Symbol> {
    let len = text.len_chars();
    let offset = offset.min(len);
    let byte = text.char_to_byte(offset);

    // A caret just past the end of a word is still "on" it. Probe one byte back
    // as well, and prefer whichever probe lands on an identifier.
    // `continue`, not `?`: a probe that lands on punctuation must fall through
    // to the next one rather than abandoning the search.
    for probe in [byte, byte.saturating_sub(1)] {
        let Some(node) = tree
            .root_node()
            .named_descendant_for_byte_range(probe, probe)
            .and_then(nearest_identifier)
        else {
            continue;
        };
        let range = byte_range_to_chars(text, node.start_byte(), node.end_byte());
        let name: String = text.slice(range.clone()).chars().collect();
        if !name.is_empty() {
            return Some(Symbol { name, range });
        }
    }
    None
}

/// Climb at most a couple of levels looking for an identifier node.
///
/// The descendant at a byte offset can be a token inside a larger node; going
/// up more than a step or two would start matching whole statements.
fn nearest_identifier<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if is_identifier(node.kind()) {
        return Some(node);
    }
    let parent = node.parent()?;
    is_identifier(parent.kind()).then_some(parent)
}

/// Every place `name` appears as an identifier in this file, in order.
///
/// String and comment contents are excluded, because they are not identifier
/// nodes — the whole reason for using the tree rather than a text search.
#[must_use]
pub fn occurrences(tree: &Tree, text: &Rope, name: &str) -> Vec<Occurrence> {
    let mut out = Vec::new();
    let mut cursor = tree.root_node().walk();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        if is_identifier(node.kind()) {
            let range = byte_range_to_chars(text, node.start_byte(), node.end_byte());
            let matched: String = text.slice(range.clone()).chars().collect();
            if matched == name {
                out.push(Occurrence {
                    is_definition: defines(node),
                    range,
                });
            }
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

    out.sort_by_key(|o| o.range.start);
    out
}

/// Where `name` is defined in this file, in order.
///
/// Usually one result. More than one is legitimate — a name defined in two
/// branches of an `if`, or a method of that name on several types — so the
/// caller is given all of them rather than a guess.
#[must_use]
pub fn definitions(tree: &Tree, text: &Rope, name: &str) -> Vec<Range<usize>> {
    occurrences(tree, text, name)
        .into_iter()
        .filter(|o| o.is_definition)
        .map(|o| o.range)
        .collect()
}

/// What a locally-defined name turned out to be.
///
/// A semantic kind rather than the protocol's numbers, so this crate stays
/// unaware of LSP; the application maps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Class,
    Module,
    /// Defined here, but not as any of the above — a parameter, a loop
    /// variable, a `let`.
    Binding,
    /// Used here and defined somewhere this cannot see.
    Unknown,
}

/// A name that appears in this file, and what it looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSymbol {
    pub name: String,
    pub kind: SymbolKind,
}

/// Every distinct identifier in the file, for completion with no language
/// server running.
///
/// This is a word list with a little structure, not an understanding of the
/// code: it has no idea what is in scope where, and cannot see a single name
/// from another file or from the standard library. It is offered because a name
/// already written somewhere in the file you are editing is very often the one
/// you are typing, and because the alternative with no server is nothing at
/// all.
///
/// `skip` is the caret's own word, which would otherwise be offered back as a
/// suggestion for itself.
#[must_use]
pub fn identifiers(tree: &Tree, text: &Rope, skip: Option<Range<usize>>) -> Vec<LocalSymbol> {
    let mut found: Vec<LocalSymbol> = Vec::new();
    let mut cursor = tree.root_node().walk();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        if is_identifier(node.kind()) {
            let range = byte_range_to_chars(text, node.start_byte(), node.end_byte());
            let overlaps_caret = skip.as_ref().is_some_and(|s| *s == range);
            if !overlaps_caret {
                let name: String = text.slice(range.clone()).chars().collect();
                // A single character is never worth suggesting: it is shorter
                // to type than to choose.
                if name.chars().count() > 1 {
                    let kind = kind_of(node);
                    match found.iter_mut().find(|s| s.name == name) {
                        // A later sighting that knows more wins: the same name
                        // is usually a use before it is a definition.
                        Some(existing) if existing.kind == SymbolKind::Unknown => {
                            existing.kind = kind;
                        }
                        Some(_) => {}
                        None => found.push(LocalSymbol { name, kind }),
                    }
                }
            }
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

    // Definitions first, then alphabetical, so the list is stable between
    // keystrokes and the things declared in this file lead it.
    found.sort_by(|a, b| {
        let rank = |k: SymbolKind| u8::from(k == SymbolKind::Unknown);
        rank(a.kind).cmp(&rank(b.kind)).then(a.name.cmp(&b.name))
    });
    found
}

/// What kind of thing this identifier names, from the node that defines it.
/// One entry in a file's outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    pub name: String,
    pub kind: SymbolKind,
    /// Where the name itself is, so jumping to it puts the caret on the name
    /// rather than on the `def` keyword before it.
    pub range: Range<usize>,
    /// How deeply nested the declaration is, for indenting the list. A method
    /// inside a class is 1; a function inside that is 2.
    pub depth: usize,
}

/// Every declaration in the file, in the order they appear.
///
/// Functions, classes and modules only. A `documentSymbol` request would also
/// return constants and fields; those are the entries that make an outline
/// long enough that nobody reads it, and the reason to open one is almost
/// always to reach a function.
///
/// Nesting is counted from the declarations themselves rather than from
/// indentation, so it is right in a brace language and right in Python without
/// two rules.
#[must_use]
pub fn outline(tree: &Tree, text: &Rope) -> Vec<Outline> {
    let mut found = Vec::new();
    let mut cursor = tree.root_node().walk();
    walk_outline(&mut cursor, text, 0, &mut found);
    found.sort_by_key(|o| o.range.start);
    found
}

fn walk_outline(
    cursor: &mut tree_sitter::TreeCursor<'_>,
    text: &Rope,
    depth: usize,
    found: &mut Vec<Outline>,
) {
    let node = cursor.node();
    // A declaration's own name node, when it has one worth listing.
    let listed = introduces_a_name(node.kind())
        .then(|| node.child_by_field_name("name"))
        .flatten()
        .map(|name| (name, declaration_kind(node.kind())))
        .filter(|(_, kind)| *kind != SymbolKind::Unknown);

    let mut child_depth = depth;
    if let Some((name, kind)) = listed {
        let range = byte_range_to_chars(text, name.start_byte(), name.end_byte());
        found.push(Outline {
            name: text.slice(range.clone()).chars().collect(),
            kind,
            range,
            depth,
        });
        child_depth = depth + 1;
    }

    if cursor.goto_first_child() {
        loop {
            walk_outline(cursor, text, child_depth, found);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// The kind of thing a declaration node declares, or `Unknown` for the ones an
/// outline should leave out.
fn declaration_kind(kind: &str) -> SymbolKind {
    match kind {
        "function_definition" | "function_item" | "function_signature_item" => SymbolKind::Function,
        "class_definition" | "struct_item" | "enum_item" | "trait_item" | "union_item"
        | "impl_item" => SymbolKind::Class,
        "mod_item" => SymbolKind::Module,
        _ => SymbolKind::Unknown,
    }
}

fn kind_of(node: Node<'_>) -> SymbolKind {
    if !defines(node) {
        return SymbolKind::Unknown;
    }
    let Some(parent) = node.parent() else {
        return SymbolKind::Unknown;
    };
    match parent.kind() {
        "function_definition" | "function_item" | "function_signature_item" => SymbolKind::Function,
        "class_definition" | "struct_item" | "enum_item" | "trait_item" | "union_item" => {
            SymbolKind::Class
        }
        "mod_item" => SymbolKind::Module,
        _ => SymbolKind::Binding,
    }
}

/// True if this identifier is the `name` field of its parent — that is, the
/// place the name is introduced rather than referred to.
fn defines(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if !introduces_a_name(parent.kind()) {
        return false;
    }
    parent
        .child_by_field_name("name")
        .is_some_and(|named| named.id() == node.id())
}

/// Whether a node of this kind is one that *introduces* a name.
///
/// The `name` field alone is not enough. Rust's `scoped_identifier` has one
/// too, so `word::next_boundary(x)` -- a call -- was reported as a definition
/// of `next_boundary`, and a project-wide search returned every call site
/// alongside the real declaration. Python's `attribute` and Rust's
/// `field_expression` are the same trap.
///
/// Matched on the suffix rather than a list of exact kinds, so this still
/// covers a grammar nobody has added yet: every one of them names its
/// declarations `..._definition`, `..._item` or `..._declaration`.
fn introduces_a_name(kind: &str) -> bool {
    kind.ends_with("_definition")
        || kind.ends_with("_item")
        || kind.ends_with("_declaration")
        || kind.ends_with("_specifier")
        || kind.ends_with("_parameter")
        || kind == "parameter"
}

fn byte_range_to_chars(text: &Rope, start: usize, end: usize) -> Range<usize> {
    let max = text.len_bytes();
    let start = text.byte_to_char(start.min(max));
    let end = text.byte_to_char(end.min(max));
    start..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageId;
    use crate::highlight::Highlighter;

    /// Parse a source file and run `f` against its tree.
    fn with_tree<T>(language: LanguageId, source: &str, f: impl Fn(&Tree, &Rope) -> T) -> T {
        let rope = Rope::from_str(source);
        let highlighter = Highlighter::new(language, &rope).expect("grammar");
        let tree = highlighter.tree().expect("a parse tree");
        f(tree, &rope)
    }

    fn text_of(source: &str, range: &Range<usize>) -> String {
        Rope::from_str(source).slice(range.clone()).to_string()
    }

    const PY: &str = "\
def parse(data):
    return data


class Reader:
    def read(self):
        return parse('parse')  # parse it


result = parse([1])
";

    #[test]
    fn the_identifier_under_the_caret_is_found() {
        // Caret in the middle of `parse` on the `def` line.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let symbol = identifier_at(tree, text, 6).expect("an identifier");
            assert_eq!(symbol.name, "parse");
            assert_eq!(text_of(PY, &symbol.range), "parse");
        });
    }

    #[test]
    fn a_caret_just_past_the_end_of_a_word_still_finds_it() {
        // Where the caret lands after double-clicking or pressing Ctrl+Right.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let symbol = identifier_at(tree, text, 9).expect("an identifier");
            assert_eq!(symbol.name, "parse");
        });
    }

    #[test]
    fn a_caret_on_whitespace_finds_nothing() {
        // Better than silently acting on whatever is nearby.
        with_tree(LanguageId::Python, "x = 1\n\n\n", |tree, text| {
            assert!(identifier_at(tree, text, 6).is_none());
        });
    }

    #[test]
    fn a_python_function_definition_is_found() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = definitions(tree, text, "parse");
            assert_eq!(found.len(), 1, "got {found:?}");
            assert_eq!(text_of(PY, &found[0]), "parse");
            // Line 0, right after `def `.
            assert_eq!(found[0].start, 4);
        });
    }

    #[test]
    fn a_python_class_definition_is_found() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            assert_eq!(definitions(tree, text, "Reader").len(), 1);
        });
    }

    /// The reason this works off the tree instead of a text search.
    #[test]
    fn a_name_inside_a_string_or_comment_is_not_a_use() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = occurrences(tree, text, "parse");
            // The definition, the call inside `read`, and the call at the end.
            // Not the `'parse'` argument and not the `# parse it` comment.
            assert_eq!(found.len(), 3, "got {found:?}");
            for occurrence in &found {
                let quoted = text_of(PY, &occurrence.range);
                assert_eq!(quoted, "parse");
            }
        });
    }

    #[test]
    fn the_definition_is_marked_and_the_uses_are_not() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = occurrences(tree, text, "parse");
            assert!(found[0].is_definition, "the first is the `def`");
            assert!(
                found[1..].iter().all(|o| !o.is_definition),
                "the rest are calls"
            );
        });
    }

    #[test]
    fn occurrences_come_back_in_document_order() {
        // Next/previous navigation walks this list, so the order is the feature.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = occurrences(tree, text, "parse");
            assert!(
                found
                    .windows(2)
                    .all(|w| w[0].range.start < w[1].range.start),
                "not in order: {found:?}"
            );
        });
    }

    const RS: &str = "\
fn parse(data: &str) -> usize {
    data.len()
}

struct Reader;

fn main() {
    let n = parse(\"parse\");
    // parse again
    println!(\"{n}\");
}
";

    #[test]
    fn every_distinct_identifier_is_offered_once() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = identifiers(tree, text, None);
            let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"parse"));
            assert!(names.contains(&"Reader"));
            assert!(names.contains(&"result"));
            // `parse` appears three times in the source and must be listed once.
            assert_eq!(
                names.iter().filter(|n| **n == "parse").count(),
                1,
                "a name repeated in the list is a name you scroll past twice"
            );
        });
    }

    #[test]
    fn a_name_defined_here_is_labelled_by_what_defines_it() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = identifiers(tree, text, None);
            let kind = |name: &str| found.iter().find(|s| s.name == name).map(|s| s.kind);
            assert_eq!(kind("parse"), Some(SymbolKind::Function));
            assert_eq!(kind("Reader"), Some(SymbolKind::Class));
            assert_eq!(kind("read"), Some(SymbolKind::Function));
        });
    }

    #[test]
    fn a_definition_is_recognised_even_when_the_use_is_seen_first() {
        // The walk order is not source order, and a name is usually used before
        // it is defined in the traversal. Whichever sighting knows more wins.
        let source = "result = parse(1)


def parse(x):
    return x
";
        with_tree(LanguageId::Python, source, |tree, text| {
            let found = identifiers(tree, text, None);
            let parse = found.iter().find(|s| s.name == "parse").expect("parse");
            assert_eq!(parse.kind, SymbolKind::Function);
        });
    }

    #[test]
    fn definitions_lead_the_list() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            let found = identifiers(tree, text, None);
            let first_unknown = found.iter().position(|s| s.kind == SymbolKind::Unknown);
            let last_known = found.iter().rposition(|s| s.kind != SymbolKind::Unknown);
            if let (Some(first), Some(last)) = (first_unknown, last_known) {
                assert!(last < first, "names defined here must come first");
            }
        });
    }

    #[test]
    fn the_word_being_typed_is_not_offered_back_to_itself() {
        // Without this, typing `par` suggests `par`.
        with_tree(LanguageId::Python, PY, |tree, text| {
            let caret = identifier_at(tree, text, 6).expect("an identifier");
            let with_skip = identifiers(tree, text, Some(caret.range.clone()));
            // `parse` appears elsewhere too, so it is still listed once; the
            // point is that the caret's own occurrence did not add a second.
            assert_eq!(with_skip.iter().filter(|s| s.name == "parse").count(), 1);
        });
    }

    #[test]
    fn a_lone_letter_is_not_worth_suggesting() {
        // Choosing from a list is slower than typing one character.
        with_tree(
            LanguageId::Python,
            "x = 1
yy = 2
",
            |tree, text| {
                let names: Vec<String> = identifiers(tree, text, None)
                    .into_iter()
                    .map(|s| s.name)
                    .collect();
                assert!(!names.contains(&"x".to_owned()));
                assert!(names.contains(&"yy".to_owned()));
            },
        );
    }

    #[test]
    fn rust_kinds_come_out_of_the_same_walk() {
        with_tree(LanguageId::Rust, RS, |tree, text| {
            let found = identifiers(tree, text, None);
            let kind = |name: &str| found.iter().find(|s| s.name == name).map(|s| s.kind);
            assert_eq!(kind("parse"), Some(SymbolKind::Function));
            assert_eq!(kind("Reader"), Some(SymbolKind::Class));
        });
    }

    #[test]
    fn names_in_strings_and_comments_are_not_offered() {
        with_tree(LanguageId::Rust, RS, |tree, text| {
            let names: Vec<String> = identifiers(tree, text, None)
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert!(names.contains(&"parse".to_owned()));
            // `again` only appears in `// parse again`.
            assert!(!names.contains(&"again".to_owned()));
        });
    }

    /// The bug a project-wide search turned up: `word::next_boundary(x)` was
    /// reported as *defining* `next_boundary`, because Rust's
    /// `scoped_identifier` also has a `name` field. Every call site came back
    /// as a definition.
    #[test]
    fn a_qualified_call_is_not_a_definition() {
        let source = "fn caller() {
    let n = word::next_boundary(text, 0);
    let m = other::next_boundary(text, 1);
}
";
        with_tree(LanguageId::Rust, source, |tree, text| {
            assert!(
                definitions(tree, text, "next_boundary").is_empty(),
                "a call through a path is not a declaration"
            );
            assert_eq!(
                occurrences(tree, text, "next_boundary").len(),
                2,
                "they are still uses"
            );
        });
    }

    #[test]
    fn an_attribute_access_is_not_a_definition() {
        // The same trap in Python: `self.parse` has a `name` field too.
        let source = "class A:
    def go(self):
        return self.parse()
";
        with_tree(LanguageId::Python, source, |tree, text| {
            assert!(definitions(tree, text, "parse").is_empty());
        });
    }

    #[test]
    fn rust_functions_and_structs_are_found_by_the_same_rule() {
        // No per-language query: both grammars name the defined thing in a
        // field called `name`, which is what makes one rule enough.
        with_tree(LanguageId::Rust, RS, |tree, text| {
            assert_eq!(definitions(tree, text, "parse").len(), 1);
            assert_eq!(definitions(tree, text, "Reader").len(), 1);
            assert_eq!(definitions(tree, text, "main").len(), 1);
        });
    }

    #[test]
    fn rust_uses_exclude_strings_and_comments_too() {
        with_tree(LanguageId::Rust, RS, |tree, text| {
            let found = occurrences(tree, text, "parse");
            assert_eq!(found.len(), 2, "the definition and one call: {found:?}");
        });
    }

    #[test]
    fn a_name_that_appears_nowhere_yields_nothing() {
        with_tree(LanguageId::Python, PY, |tree, text| {
            assert!(occurrences(tree, text, "nonexistent").is_empty());
            assert!(definitions(tree, text, "nonexistent").is_empty());
        });
    }

    #[test]
    fn a_used_but_undefined_name_has_uses_and_no_definition() {
        // The common case for an import from another module: uses are findable
        // here, the definition is not, and the caller has to say so.
        with_tree(
            LanguageId::Python,
            "import os\n\nos.getcwd()\n",
            |tree, text| {
                assert!(!occurrences(tree, text, "os").is_empty());
                assert!(
                    definitions(tree, text, "os").is_empty(),
                    "an import is not a definition this can see"
                );
            },
        );
    }

    #[test]
    fn multibyte_text_does_not_shift_the_ranges() {
        // Tree-sitter works in bytes and the editor in characters; getting the
        // conversion wrong puts the jump several columns off.
        let source = "s = '\u{1f600}\u{1f600}'\n\n\ndef parse():\n    pass\n";
        with_tree(LanguageId::Python, source, |tree, text| {
            let found = definitions(tree, text, "parse");
            assert_eq!(found.len(), 1);
            let quoted: String = text.slice(found[0].clone()).chars().collect();
            assert_eq!(quoted, "parse");
        });
    }

    #[test]
    fn a_broken_file_still_yields_what_it_can() {
        // Go to Definition must not stop working the moment there is a typo
        // somewhere else in the file.
        let source = "def parse():\n    pass\n\nif bob = kate\n";
        with_tree(LanguageId::Python, source, |tree, text| {
            assert_eq!(definitions(tree, text, "parse").len(), 1);
        });
    }

    fn outline_of(language: crate::LanguageId, source: &str) -> Vec<Outline> {
        let text = Rope::from_str(source);
        let highlighter = crate::highlight::Highlighter::new(language, &text).expect("grammar");
        let tree = highlighter.tree().expect("tree").clone();
        outline(&tree, &text)
    }

    #[test]
    fn a_python_outline_lists_classes_and_their_methods_in_order() {
        let got = outline_of(
            crate::LanguageId::Python,
            "import os\n\n\nclass Widget:\n    def __init__(self):\n        pass\n\n    \
             def scaled(self, f):\n        def helper():\n            pass\n        \
             return helper\n\n\ndef main():\n    pass\n",
        );
        let names: Vec<&str> = got.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            ["Widget", "__init__", "scaled", "helper", "main"],
            "in the order they appear"
        );

        assert_eq!(got[0].kind, SymbolKind::Class);
        assert_eq!(got[1].kind, SymbolKind::Function);
        // Depth is counted from the declarations, not from indentation.
        assert_eq!(got[0].depth, 0, "the class is top level");
        assert_eq!(got[1].depth, 1, "a method is inside it");
        assert_eq!(got[3].depth, 2, "a function inside a method");
        assert_eq!(got[4].depth, 0, "and back out again");
    }

    #[test]
    fn a_rust_outline_lists_items() {
        let got = outline_of(
            crate::LanguageId::Rust,
            "struct Widget {\n    size: u32,\n}\n\nimpl Widget {\n    \
             fn scaled(&self) -> u32 {\n        self.size\n    }\n}\n\nfn main() {}\n",
        );
        let names: Vec<&str> = got.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"Widget"), "got {names:?}");
        assert!(names.contains(&"scaled"), "got {names:?}");
        assert!(names.contains(&"main"), "got {names:?}");
    }

    /// The range points at the name, so jumping lands the caret on it rather
    /// than on the keyword before it.
    #[test]
    fn the_range_covers_the_name_itself() {
        let source = "def greet():\n    pass\n";
        let got = outline_of(crate::LanguageId::Python, source);
        assert_eq!(got.len(), 1);
        assert_eq!(&source[got[0].range.clone()], "greet");
    }

    /// Constants and fields are what make an outline too long to read.
    #[test]
    fn variables_and_fields_are_left_out() {
        let got = outline_of(
            crate::LanguageId::Python,
            "TOTAL = 1\n\nclass A:\n    size = 0\n\n    def f(self):\n        x = 2\n",
        );
        let names: Vec<&str> = got.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["A", "f"], "got {names:?}");
    }

    #[test]
    fn a_file_with_no_declarations_has_an_empty_outline() {
        assert!(outline_of(crate::LanguageId::Python, "x = 1\ny = 2\n").is_empty());
    }
}
