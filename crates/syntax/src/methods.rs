//! The first parameter of a Python method.
//!
//! Typing `def name(` inside a class body almost always wants `self` next, and
//! typing it out is one of those small frictions that is invisible until it is
//! gone. `@classmethod` wants `cls`; `@staticmethod` wants neither.
//!
//! Worked out from the text rather than the parse tree, deliberately. At the
//! moment the `(` is typed the line reads `def name(` and the file does not
//! parse; a tree built from that is a tree full of ERROR nodes, and the answer
//! would depend on how the grammar happened to recover. Indentation and the
//! `def`/`class` keywords are unambiguous here and are exactly what a reader
//! uses to answer the same question.

use ropey::Rope;

/// What a new method's parameter list should start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstParameter {
    /// An ordinary method.
    SelfRef,
    /// Decorated with `@classmethod`.
    Class,
}

impl FirstParameter {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfRef => "self",
            Self::Class => "cls",
        }
    }
}

/// What to insert when `(` is typed at `offset`, if anything.
///
/// `offset` is where the `(` is about to go. Returns `None` unless the text
/// immediately before it is a `def` line's name, inside a class, and not
/// decorated `@staticmethod`.
#[must_use]
pub fn first_parameter(text: &Rope, offset: usize) -> Option<FirstParameter> {
    let offset = offset.min(text.len_chars());
    let line_index = text.char_to_line(offset);
    let line_start = text.line_to_char(line_index);
    let before: String = text.slice(line_start..offset).to_string();

    // The line so far has to be exactly a def and its name: `def foo`. Anything
    // else — a call, a tuple, a nested paren — is not a definition being
    // written, and guessing there would insert `self` into arithmetic.
    if !is_def_header(&before) {
        return None;
    }

    let indent = leading_spaces(&before);
    if !is_inside_class(text, line_index, indent) {
        return None;
    }

    Some(match decorator_above(text, line_index, indent) {
        Some(Decorator::Static) => return None,
        Some(Decorator::Class) => FirstParameter::Class,
        None => FirstParameter::SelfRef,
    })
}

/// True for `def name` / `async def name` and nothing else.
fn is_def_header(line_so_far: &str) -> bool {
    let rest = line_so_far.trim_start();
    let rest = rest.strip_prefix("async ").map_or(rest, str::trim_start);
    let Some(rest) = rest.strip_prefix("def") else {
        return false;
    };
    // `def` has to be a whole word, or `define(` would qualify.
    let Some(name) = rest.strip_prefix(|c: char| c == ' ' || c == '\t') else {
        return false;
    };
    let name = name.trim_start();
    // Exactly one identifier, already complete, with nothing after it — the
    // caret is where the `(` goes.
    !name.is_empty()
        && !name.ends_with(char::is_whitespace)
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// Whether the nearest enclosing block header is a `class`.
///
/// Walks up looking for the first non-blank line indented less than this one.
/// If that line opens a class, this is a method; if it opens anything else — a
/// `def`, an `if`, a `with` — it is a plain function that happens to be nested,
/// and those take no `self`.
fn is_inside_class(text: &Rope, line_index: usize, indent: usize) -> bool {
    for above in (0..line_index).rev() {
        let line = text.line(above).to_string();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let its_indent = leading_spaces(&line);
        if its_indent >= indent {
            continue;
        }
        // A decorator belongs to whatever follows it, not to the block.
        if trimmed.starts_with('@') {
            continue;
        }
        return trimmed.starts_with("class ") || trimmed.starts_with("class(");
    }
    false
}

enum Decorator {
    Static,
    Class,
}

/// The `@staticmethod` or `@classmethod` decorating this definition, if any.
///
/// Only decorators at the definition's own indentation count, and the run of
/// them stops at the first line that is not a decorator: a `@property` three
/// definitions up says nothing about this one.
fn decorator_above(text: &Rope, line_index: usize, indent: usize) -> Option<Decorator> {
    for above in (0..line_index).rev() {
        let line = text.line(above).to_string();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('@') || leading_spaces(&line) != indent {
            return None;
        }
        // `@staticmethod`, and also `@staticmethod  # comment`.
        let name = trimmed
            .trim_start_matches('@')
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .next()
            .unwrap_or("");
        match name {
            "staticmethod" => return Some(Decorator::Static),
            "classmethod" => return Some(Decorator::Class),
            _ => {}
        }
    }
    None
}

/// Width of the leading whitespace, counting a tab as one.
///
/// Comparing like with like is all this needs: every line in a Python file
/// that means to be at the same level is indented the same way, and a file
/// mixing tabs and spaces is one Python itself rejects.
fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a rope and return the offset just past the last `|` marker, which
    /// is where the `(` is about to be typed.
    fn at(source: &str) -> (Rope, usize) {
        let offset = source.find('|').expect("mark the caret with |");
        let text = source.replace('|', "");
        (Rope::from_str(&text), text[..offset].chars().count())
    }

    fn ask(source: &str) -> Option<FirstParameter> {
        let (text, offset) = at(source);
        first_parameter(&text, offset)
    }

    #[test]
    fn a_method_in_a_class_takes_self() {
        assert_eq!(
            ask("class A:\n    def greet|\n"),
            Some(FirstParameter::SelfRef)
        );
    }

    #[test]
    fn a_module_level_function_takes_nothing() {
        assert_eq!(ask("def greet|\n"), None);
    }

    /// A function nested inside a method is still a function.
    #[test]
    fn a_function_nested_inside_a_method_takes_nothing() {
        assert_eq!(
            ask("class A:\n    def outer(self):\n        def inner|\n"),
            None
        );
    }

    #[test]
    fn a_classmethod_takes_cls_and_a_staticmethod_takes_nothing() {
        assert_eq!(
            ask("class A:\n    @classmethod\n    def make|\n"),
            Some(FirstParameter::Class)
        );
        assert_eq!(ask("class A:\n    @staticmethod\n    def helper|\n"), None);
    }

    /// Decorators stack, and the one that matters may not be nearest.
    #[test]
    fn a_decorator_further_up_the_stack_still_counts() {
        assert_eq!(
            ask("class A:\n    @classmethod\n    @wraps(f)\n    def make|\n"),
            Some(FirstParameter::Class)
        );
    }

    /// A decorator on some earlier definition says nothing about this one.
    #[test]
    fn a_decorator_on_a_previous_method_is_not_borrowed() {
        assert_eq!(
            ask("class A:\n    @staticmethod\n    def a(x):\n        pass\n\n    def b|\n"),
            Some(FirstParameter::SelfRef)
        );
    }

    #[test]
    fn async_methods_are_recognised() {
        assert_eq!(
            ask("class A:\n    async def fetch|\n"),
            Some(FirstParameter::SelfRef)
        );
    }

    /// The trap this rule has to avoid: a `(` typed anywhere that is not a
    /// definition's parameter list must be left alone.
    #[test]
    fn an_ordinary_bracket_is_never_touched() {
        for source in [
            "class A:\n    def a(self):\n        return foo|\n",
            "class A:\n    def a(self):\n        x = (1 + 2)|\n",
            "class A:\n    default|\n",
            "class A:\n    def |\n",
            "class A:\n    def a(self, b|\n",
            "class A:\n    x = define|\n",
        ] {
            assert_eq!(ask(source), None, "should not fire: {source:?}");
        }
    }

    /// Blank lines and comments between the class and the method are ordinary.
    #[test]
    fn blank_lines_and_comments_do_not_hide_the_class() {
        assert_eq!(
            ask("class A:\n    # a note\n\n    def greet|\n"),
            Some(FirstParameter::SelfRef)
        );
    }

    /// A class nested in a function is still a class.
    #[test]
    fn a_class_nested_inside_a_function_still_gives_its_methods_self() {
        assert_eq!(
            ask("def outer():\n    class Inner:\n        def greet|\n"),
            Some(FirstParameter::SelfRef)
        );
    }

    #[test]
    fn a_name_cannot_start_with_a_digit() {
        assert_eq!(ask("class A:\n    def 2fast|\n"), None);
    }

    #[test]
    fn the_two_parameters_spell_themselves_correctly() {
        assert_eq!(FirstParameter::SelfRef.as_str(), "self");
        assert_eq!(FirstParameter::Class.as_str(), "cls");
    }
}
