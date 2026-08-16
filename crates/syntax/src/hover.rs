//! What can be said about a symbol with no language server running.
//!
//! The degradation ladder in PLAN.md §3.6: everything that can work without a
//! server should, and should say that is what it is doing. Go to Definition and
//! Find Uses already fall back to the parse tree; this is the same fall for
//! hover.
//!
//! What it can offer is the *declaration* — the `def`, the `fn`, the `class`
//! line — for a name declared in the file you are looking at, plus whatever
//! documentation sits immediately above or below it. What it cannot offer is a
//! type, anything from another file, or anything from a library. That is a
//! large gap and the answer is still worth having: the question a hover usually
//! answers is "what were the arguments again?", and the declaration line
//! answers exactly that.

use ropey::Rope;
use tree_sitter::Tree;

use crate::LanguageId;
use crate::symbols::{self, Outline, SymbolKind};

/// What the file itself can say about a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// The name asked about.
    pub name: String,
    /// The declaration as written — `def add(a, b):` or `pub fn add(a: i32)`.
    pub declaration: String,
    /// The docstring or doc comment attached to it, if there is one.
    pub documentation: String,
    pub kind: SymbolKind,
}

impl Local {
    /// The whole thing as text, ready to be shown.
    #[must_use]
    pub fn text(&self) -> String {
        if self.documentation.is_empty() {
            self.declaration.clone()
        } else {
            format!("{}\n\n{}", self.declaration, self.documentation)
        }
    }
}

/// What this file knows about whatever is at `offset`.
///
/// `None` when there is no identifier there, or when it is not declared in this
/// file — which is most names, and is why this is a fallback and not a feature.
#[must_use]
pub fn local(tree: &Tree, text: &Rope, offset: usize, language: LanguageId) -> Option<Local> {
    let symbol = symbols::identifier_at(tree, text, offset)?;
    let declarations = symbols::definitions(tree, text, &symbol.name);
    // The first declaration. More than one is legitimate — the same name in two
    // branches of an `if` — and picking one is better than showing both in a
    // window this size.
    let at = declarations.first()?.start;

    let line_number = text.char_to_line(at.min(text.len_chars()));
    let declaration = line_text(text, line_number);
    if declaration.is_empty() {
        return None;
    }

    let kind = kind_of(tree, text, &symbol.name);
    let documentation = documentation(text, line_number, language);

    Some(Local {
        name: symbol.name,
        declaration,
        documentation,
        kind,
    })
}

/// What sort of thing the outline says this name is.
fn kind_of(tree: &Tree, text: &Rope, name: &str) -> SymbolKind {
    symbols::outline(tree, text)
        .iter()
        .find(|item: &&Outline| item.name == name)
        .map_or(SymbolKind::Binding, |item| item.kind)
}

/// The documentation attached to a declaration on `line`.
///
/// Python puts it *below*, as the first string in the body; Rust and everything
/// else put it above, as `///` or `#` comments. Both are looked for, because
/// the alternative is knowing the language in two places.
fn documentation(text: &Rope, line: usize, language: LanguageId) -> String {
    /// Enough for a paragraph. A hover is a small window and a forty-line
    /// docstring in it is a wall, not an answer.
    const MOST: usize = 12;

    if language == LanguageId::Python
        && let Some(found) = python_docstring(text, line, MOST)
    {
        return found;
    }
    comments_above(text, line, MOST)
}

/// The `"""…"""` immediately below a `def` or `class`.
fn python_docstring(text: &Rope, line: usize, most: usize) -> Option<String> {
    let first = line_text(text, line + 1);
    let trimmed = first.trim_start();
    let quote = ["\"\"\"", "'''"]
        .into_iter()
        .find(|q| trimmed.starts_with(q))?;

    let mut out = Vec::new();
    // A one-line docstring opens and closes on the same line.
    let opened = trimmed.strip_prefix(quote).unwrap_or(trimmed);
    if let Some(single) = opened.strip_suffix(quote) {
        return Some(single.trim().to_owned());
    }
    out.push(opened.trim_end().to_owned());

    for number in (line + 2)..text.len_lines().min(line + 2 + most) {
        let body = line_text(text, number);
        match body.trim_end().strip_suffix(quote) {
            Some(last) => {
                out.push(last.to_owned());
                break;
            }
            None => out.push(body),
        }
    }

    Some(dedent(&out))
}

/// Comment lines immediately above `line`.
fn comments_above(text: &Rope, line: usize, most: usize) -> String {
    let mut collected: Vec<String> = Vec::new();

    for number in (0..line).rev().take(most) {
        let body = line_text(text, number);
        let trimmed = body.trim_start();
        // An attribute between the doc comment and the item is ordinary Rust,
        // and stopping at it would lose every documented `#[derive]`d thing.
        if trimmed.starts_with("#[") || trimmed.starts_with("#!") {
            continue;
        }
        let stripped = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
            .or_else(|| trimmed.strip_prefix("# "))
            .or_else(|| (trimmed == "#").then_some(""));
        match stripped {
            Some(rest) => collected.push(rest.trim_start().to_owned()),
            None => break,
        }
    }

    collected.reverse();
    while collected.last().is_some_and(|l| l.trim().is_empty()) {
        collected.pop();
    }
    collected.join("\n")
}

/// Remove the indentation the lines share, so a docstring from four levels in
/// does not arrive with sixteen spaces on every line.
///
/// The first line is left out of the measurement. It follows the opening quotes
/// on their own line and so has no indentation of its own — counting it would
/// make the shared indent nought and dedent nothing.
fn dedent(lines: &[String]) -> String {
    let indent = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    let out: Vec<&str> = lines
        .iter()
        .enumerate()
        .map(|(at, l)| {
            if at > 0 && l.len() >= indent {
                &l[indent..]
            } else {
                l.as_str()
            }
        })
        .collect();
    out.join("\n").trim().to_owned()
}

/// One line of the rope, without its ending.
fn line_text(text: &Rope, line: usize) -> String {
    if line >= text.len_lines() {
        return String::new();
    }
    text.line(line)
        .to_string()
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Highlighter;

    fn parsed(source: &str, language: LanguageId) -> (Tree, Rope) {
        let text = Rope::from_str(source);
        let highlighter = Highlighter::new(language, &text).expect("a grammar");
        let tree = highlighter.tree().expect("a tree").clone();
        (tree, text)
    }

    /// The offset of the first character of `needle`'s `nth` occurrence.
    fn find(source: &str, needle: &str, nth: usize) -> usize {
        source.match_indices(needle).nth(nth).expect("occurrence").0
    }

    #[test]
    fn a_python_function_gives_its_def_line_and_its_docstring() {
        let source =
            "def add(a, b):\n    \"\"\"Add two numbers.\"\"\"\n    return a + b\n\n\nadd(1, 2)\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "add", 1);

        let local = local(&tree, &text, at, LanguageId::Python).expect("something to say");
        assert_eq!(local.name, "add");
        assert_eq!(local.declaration, "def add(a, b):");
        assert_eq!(local.documentation, "Add two numbers.");
        assert_eq!(local.kind, SymbolKind::Function);
        assert_eq!(local.text(), "def add(a, b):\n\nAdd two numbers.");
    }

    #[test]
    fn a_multi_line_docstring_comes_back_whole_and_undented() {
        let source = "def add(a, b):\n    \"\"\"Add two numbers.\n\n    And say so.\n    \"\"\"\n    return a + b\n\nadd(1, 2)\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "add", 1);

        let local = local(&tree, &text, at, LanguageId::Python).expect("something");
        assert_eq!(local.documentation, "Add two numbers.\n\nAnd say so.");
    }

    #[test]
    fn a_single_quoted_docstring_works_too() {
        let source = "def add(a, b):\n    '''Add them.'''\n    return a + b\n\nadd(1, 2)\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "add", 1);
        let local = local(&tree, &text, at, LanguageId::Python).expect("something");
        assert_eq!(local.documentation, "Add them.");
    }

    #[test]
    fn a_function_with_no_docstring_gives_just_its_declaration() {
        let source = "def add(a, b):\n    return a + b\n\nadd(1, 2)\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "add", 1);
        let local = local(&tree, &text, at, LanguageId::Python).expect("something");
        assert_eq!(local.documentation, "");
        assert_eq!(local.text(), "def add(a, b):");
    }

    #[test]
    fn a_rust_function_gives_its_signature_and_its_doc_comment() {
        let source = "/// Adds two numbers.\n/// And says so.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn main() {\n    add(1, 2);\n}\n";
        let (tree, text) = parsed(source, LanguageId::Rust);
        let at = find(source, "add", 1);

        let local = local(&tree, &text, at, LanguageId::Rust).expect("something");
        assert_eq!(local.declaration, "pub fn add(a: i32, b: i32) -> i32 {");
        assert_eq!(local.documentation, "Adds two numbers.\nAnd says so.");
        assert_eq!(local.kind, SymbolKind::Function);
    }

    /// A doc comment separated from its item by an attribute is ordinary Rust,
    /// and stopping at the attribute would lose it.
    #[test]
    fn an_attribute_between_the_comment_and_the_item_is_stepped_over() {
        let source = "/// A thing.\n#[derive(Debug)]\npub struct Thing;\n\nfn main() {\n    let _: Thing;\n}\n";
        let (tree, text) = parsed(source, LanguageId::Rust);
        let at = find(source, "Thing", 1);
        let local = local(&tree, &text, at, LanguageId::Rust).expect("something");
        assert_eq!(local.documentation, "A thing.");
    }

    #[test]
    fn a_name_that_is_not_declared_here_has_nothing_to_say() {
        let source = "import os\n\nos.getcwd()\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "getcwd", 0);
        assert_eq!(local(&tree, &text, at, LanguageId::Python), None);
    }

    #[test]
    fn a_position_with_no_identifier_has_nothing_to_say() {
        let source = "x = 1 + 2\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "+", 0);
        assert_eq!(local(&tree, &text, at, LanguageId::Python), None);
    }

    /// A hover is a small window, and a forty-line docstring in it is a wall.
    #[test]
    fn a_very_long_docstring_is_cut_rather_than_shown_whole() {
        let body: String = (0..40).map(|n| format!("    line {n}\n")).collect();
        let source = format!(
            "def add(a, b):\n    \"\"\"Start.\n{body}    \"\"\"\n    return 1\n\nadd(1, 2)\n"
        );
        let (tree, text) = parsed(&source, LanguageId::Python);
        let at = find(&source, "add", 1);

        let local = local(&tree, &text, at, LanguageId::Python).expect("something");
        let lines = local.documentation.lines().count();
        assert!(lines <= 14, "kept {lines} lines");
        assert!(local.documentation.starts_with("Start."));
    }

    #[test]
    fn a_class_is_recognised_as_one() {
        let source = "class Thing:\n    \"\"\"A thing.\"\"\"\n    pass\n\nThing()\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "Thing", 1);
        let local = local(&tree, &text, at, LanguageId::Python).expect("something");
        assert_eq!(local.kind, SymbolKind::Class);
        assert_eq!(local.declaration, "class Thing:");
    }

    /// The fallback covers *declarations* — functions, classes, structs — and
    /// not plain assignments. That is deliberate rather than a gap here:
    /// `introduces_a_name` excludes them, because counting every assignment as
    /// a definition is what made a project-wide search return every call site.
    #[test]
    fn a_plain_assignment_is_not_a_declaration_and_has_nothing_to_say() {
        let source = "total = 1 + 2\n\nprint(total)\n";
        let (tree, text) = parsed(source, LanguageId::Python);
        let at = find(source, "total", 1);
        assert_eq!(local(&tree, &text, at, LanguageId::Python), None);
    }

    #[test]
    fn comments_that_are_not_documentation_are_not_collected() {
        let source = "// just a note\npub fn add(a: i32) -> i32 { a }\n\nfn main() { add(1); }\n";
        let (tree, text) = parsed(source, LanguageId::Rust);
        let at = find(source, "add", 1);
        let local = local(&tree, &text, at, LanguageId::Rust).expect("something");
        assert_eq!(
            local.documentation, "",
            "`//` is a note to the author, not documentation"
        );
    }
}
