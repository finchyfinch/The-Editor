//! Which test the caret is in.
//!
//! Worked out from the outline the syntax layer already builds, rather than
//! from a second set of tree-sitter queries. The outline records every
//! declaration with its name, its kind and how deeply it is nested, which is
//! exactly enough: the test is the nearest `def` or `fn` above the caret, and
//! whatever encloses it is whatever is above *that* at a shallower depth.
//!
//! The outline records where each *name* is, not where its body ends, so
//! "encloses the caret" is approximated by "is the nearest one above it". For
//! the question being asked — the caret is inside a test, which test is it? —
//! that is the same answer, and it stays right in a file that does not parse,
//! which is the state a test file spends much of its time in.

use editor_syntax::symbols::{Outline, SymbolKind};

use crate::Framework;

/// The test the caret is in, named the way its framework names it.
///
/// For pytest this is the part after the file — `TestThing::test_case` — and
/// the caller puts the path in front. For `cargo test` it is the module path,
/// which is what libtest filters on.
///
/// `None` when the caret is not inside anything that looks like a test.
#[must_use]
pub fn test_at(outline: &[Outline], caret: usize, framework: Framework) -> Option<String> {
    // The declaration the caret is in: the last one that starts at or before
    // it. Declarations are in source order.
    let index = outline.iter().rposition(|item| item.range.start <= caret)?;
    let item = &outline[index];
    if item.kind != SymbolKind::Function || !is_test_name(&item.name, framework) {
        return None;
    }

    // Everything enclosing it, from the outside in: walk back for each
    // successively shallower declaration.
    let mut parts = vec![item.name.clone()];
    let mut depth = item.depth;
    for above in outline[..index].iter().rev() {
        if above.depth >= depth {
            continue;
        }
        depth = above.depth;
        match framework {
            // pytest addresses a method as `Class::method`, and only classes
            // count — a test nested inside another function is not collectable
            // and naming it would produce a node id pytest rejects.
            Framework::Pytest if above.kind == SymbolKind::Class => {
                parts.push(above.name.clone());
            }
            Framework::Pytest => return None,
            // libtest's name is the module path, and a test inside a function
            // is not a test at all.
            Framework::CargoTest if above.kind == SymbolKind::Module => {
                parts.push(above.name.clone());
            }
            Framework::CargoTest => return None,
        }
        if depth == 0 {
            break;
        }
    }

    parts.reverse();
    Some(parts.join("::"))
}

/// Whether a name is one its framework would collect.
///
/// pytest's default is `test_*`, and while that is configurable this is only
/// deciding whether to *offer* to run it — being wrong means a menu entry that
/// finds nothing, not a wrong test run.
///
/// Rust has no naming convention at all: a test is one with `#[test]` above it,
/// and the outline does not record attributes. So every function qualifies, and
/// `cargo test --exact` on something that is not a test simply matches nothing.
fn is_test_name(name: &str, framework: Framework) -> bool {
    match framework {
        Framework::Pytest => name.starts_with("test"),
        Framework::CargoTest => true,
    }
}

/// The pytest node id for a test, given where its file is.
///
/// `path` must be relative to wherever the run will start, because that is what
/// pytest resolves node ids against.
#[must_use]
pub fn node_id(path: &str, name: &str) -> String {
    format!("{}::{name}", path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    fn item(name: &str, kind: SymbolKind, at: usize, depth: usize) -> Outline {
        Outline {
            name: name.to_owned(),
            kind,
            range: Range {
                start: at,
                end: at + name.len(),
            },
            depth,
            // Not what these tests are about: `test_at` reads the order and
            // the depth, never the span.
            first_line: 0,
            last_line: 0,
        }
    }

    /// A file of plain test functions.
    fn flat_python() -> Vec<Outline> {
        vec![
            item("test_one", SymbolKind::Function, 10, 0),
            item("helper", SymbolKind::Function, 100, 0),
            item("test_two", SymbolKind::Function, 200, 0),
        ]
    }

    #[test]
    fn the_caret_inside_a_test_finds_it() {
        let outline = flat_python();
        assert_eq!(
            test_at(&outline, 50, Framework::Pytest).as_deref(),
            Some("test_one")
        );
        assert_eq!(
            test_at(&outline, 250, Framework::Pytest).as_deref(),
            Some("test_two")
        );
    }

    #[test]
    fn the_caret_on_the_name_itself_finds_it() {
        let outline = flat_python();
        assert_eq!(
            test_at(&outline, 10, Framework::Pytest).as_deref(),
            Some("test_one")
        );
    }

    #[test]
    fn the_caret_in_a_helper_finds_nothing() {
        let outline = flat_python();
        assert_eq!(test_at(&outline, 150, Framework::Pytest), None);
    }

    #[test]
    fn the_caret_above_everything_finds_nothing() {
        let outline = flat_python();
        assert_eq!(test_at(&outline, 0, Framework::Pytest), None);
        assert_eq!(test_at(&[], 0, Framework::Pytest), None);
    }

    #[test]
    fn a_method_is_named_with_its_class() {
        let outline = vec![
            item("TestThing", SymbolKind::Class, 10, 0),
            item("test_case", SymbolKind::Function, 40, 1),
        ];
        assert_eq!(
            test_at(&outline, 50, Framework::Pytest).as_deref(),
            Some("TestThing::test_case")
        );
    }

    /// A test nested inside another function is not collectable, and naming it
    /// would produce a node id pytest rejects.
    #[test]
    fn a_test_inside_a_function_is_not_offered() {
        let outline = vec![
            item("outer", SymbolKind::Function, 10, 0),
            item("test_inner", SymbolKind::Function, 40, 1),
        ];
        assert_eq!(test_at(&outline, 50, Framework::Pytest), None);
    }

    #[test]
    fn a_rust_test_is_named_with_its_modules() {
        let outline = vec![
            item("tests", SymbolKind::Module, 10, 0),
            item("a_test", SymbolKind::Function, 40, 1),
        ];
        assert_eq!(
            test_at(&outline, 50, Framework::CargoTest).as_deref(),
            Some("tests::a_test")
        );
    }

    #[test]
    fn nested_rust_modules_all_appear_in_the_name() {
        let outline = vec![
            item("outer", SymbolKind::Module, 10, 0),
            item("inner", SymbolKind::Module, 30, 1),
            item("a_test", SymbolKind::Function, 60, 2),
        ];
        assert_eq!(
            test_at(&outline, 70, Framework::CargoTest).as_deref(),
            Some("outer::inner::a_test")
        );
    }

    /// Rust has no naming convention — a test is one with `#[test]` above it,
    /// which the outline does not record — so every function is offered.
    #[test]
    fn any_rust_function_is_offered_because_the_attribute_is_invisible_here() {
        let outline = vec![item("does_a_thing", SymbolKind::Function, 10, 0)];
        assert_eq!(
            test_at(&outline, 20, Framework::CargoTest).as_deref(),
            Some("does_a_thing")
        );
    }

    #[test]
    fn a_python_function_not_called_test_is_not_offered() {
        let outline = vec![item("does_a_thing", SymbolKind::Function, 10, 0)];
        assert_eq!(test_at(&outline, 20, Framework::Pytest), None);
    }

    #[test]
    fn a_node_id_puts_the_file_in_front_with_forward_slashes() {
        assert_eq!(
            node_id("tests\\test_a.py", "TestB::test_c"),
            "tests/test_a.py::TestB::test_c"
        );
        assert_eq!(
            node_id("tests/test_a.py", "test_b"),
            "tests/test_a.py::test_b"
        );
    }

    /// Declarations after the caret must not be picked up.
    #[test]
    fn a_test_below_the_caret_is_not_the_one_it_is_in() {
        let outline = vec![
            item("helper", SymbolKind::Function, 10, 0),
            item("test_later", SymbolKind::Function, 500, 0),
        ];
        assert_eq!(test_at(&outline, 100, Framework::Pytest), None);
    }
}
