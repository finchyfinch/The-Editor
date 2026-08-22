//! Python docstrings, written from the definition they belong to.
//!
//! Typing `"""` on the first line of a `def` or `class` body is a statement of
//! intent: what follows is a docstring, and its skeleton is already determined
//! by the signature above it. The parameters are known, their annotations and
//! defaults are known, and whether the body returns or yields or raises is
//! there to be read. Typing all of that out again by hand is transcription,
//! and transcription is what an editor is for.
//!
//! Read from the text rather than from the parse tree, for the same reason
//! [`crate::methods`] is: at the moment the third quote is typed the string is
//! unterminated, so the file does not parse, and the answer would depend on
//! how the grammar happened to recover. A `def` header and the indentation
//! under it are unambiguous, and are exactly what a reader uses to answer the
//! same question.
//!
//! Three layouts are offered because there is no winner: Google's is the most
//! readable, NumPy's is the convention across the scientific stack, and
//! Sphinx's reST is what older codebases and `autodoc` expect. The parsing is
//! shared and only the rendering differs.

use ropey::Rope;

/// How a generated docstring is laid out.
///
/// Defined in `editor-config`, with the rest of the vocabulary a settings file
/// uses, and re-exported here where the generating happens. One definition, so
/// a layout cannot be offered in the settings window and then not understood.
pub use editor_config::settings::DocstringStyle as Style;

/// What the placeholder text says where a human has to write something.
///
/// Deliberately ugly. A blank line reads as finished and gets left behind; a
/// word in underscores is visible in a diff, greppable before a release, and
/// impossible to mistake for prose somebody wrote.
const PLACEHOLDER: &str = "_description_";

/// One parameter of a definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// The name, keeping any `*` or `**`.
    pub name: String,
    /// The annotation, if it was written.
    pub annotation: Option<String>,
    /// The default, if it has one — which is what makes a parameter optional.
    pub default: Option<String>,
}

/// What a definition returns, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Returns {
    /// A `return` with a value somewhere in the body.
    Value(Option<String>),
    /// A `yield`, which is documented under its own heading.
    Yield(Option<String>),
}

/// A `def` or `class` header, parsed far enough to write about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub is_class: bool,
    pub params: Vec<Param>,
    pub returns: Option<Returns>,
    /// Exception types raised directly in the body, in the order found.
    pub raises: Vec<String>,
}

/// The definition a docstring typed on `line` would belong to.
///
/// `None` unless `line` really is the first statement of a `def` or `class`
/// body: everywhere else a triple quote is just a string, and filling one with
/// `Args:` would be vandalism.
#[must_use]
pub fn definition_above(text: &Rope, line: usize) -> Option<Definition> {
    let header_end = last_statement_before(text, line)?;
    let header_start = header_start_of(text, header_end)?;

    let header = joined(text, header_start, header_end);
    // The body has to be indented past its header, or this is a sibling
    // statement rather than the inside of anything.
    if indent_of(&line_text(text, line)) <= indent_of(&line_text(text, header_start)) {
        return None;
    }

    let (is_class, after_name) = split_keyword(&header)?;
    let params = match (is_class, params_of(after_name)) {
        // A class documents what it takes to build one, which is `__init__`'s
        // parameters rather than the base classes in its own header.
        (true, _) => {
            initialiser_params(text, header_end, indent_of(&line_text(text, header_start)))
        }
        (false, params) => params,
    };

    let body = body_of(text, header_end, indent_of(&line_text(text, header_start)));
    let returns = if is_class {
        None
    } else {
        returns_of(after_name, &body)
    };

    Some(Definition {
        is_class,
        params,
        returns,
        raises: raises_in(&body),
    })
}

/// The docstring to insert, and where in it the caret belongs.
///
/// `indent` is the whitespace every line of it starts with — the indentation
/// of the body it is being written into. `unit` is one further step of
/// indentation, for the entries nested under a heading.
///
/// The caret lands just past the opening quotes, on the summary line. PEP 257
/// puts the one-line summary there, the summary is the one part no signature
/// can supply, and it is the one part every docstring needs.
#[must_use]
pub fn render(definition: &Definition, style: Style, indent: &str, unit: &str) -> (String, usize) {
    let mut out = String::from("\"\"\"");
    let caret = out.chars().count();

    let body = match style {
        Style::Google => google(definition, unit),
        Style::Numpy => numpy(definition, unit),
        Style::Sphinx => sphinx(definition, unit),
    };
    out.push('\n');
    for line in &body {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(indent);
    out.push_str("\"\"\"");
    (out, caret)
}

// ---- layouts -------------------------------------------------------------

/// `a (int, optional)` — the parenthesised half of a Google `Args:` entry.
fn google_type(param: &Param) -> String {
    let mut parts = Vec::new();
    if let Some(annotation) = &param.annotation {
        parts.push(annotation.clone());
    }
    if param.default.is_some() {
        parts.push("optional".to_owned());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

fn google(definition: &Definition, unit: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if !definition.params.is_empty() {
        lines.push(String::new());
        lines.push("Args:".to_owned());
        for param in &definition.params {
            lines.push(format!(
                "{unit}{}{}: {PLACEHOLDER}",
                param.name,
                google_type(param)
            ));
        }
    }
    match &definition.returns {
        Some(Returns::Value(kind)) => {
            lines.push(String::new());
            lines.push("Returns:".to_owned());
            lines.push(match kind {
                Some(kind) => format!("{unit}{kind}: {PLACEHOLDER}"),
                None => format!("{unit}{PLACEHOLDER}"),
            });
        }
        Some(Returns::Yield(kind)) => {
            lines.push(String::new());
            lines.push("Yields:".to_owned());
            lines.push(match kind {
                Some(kind) => format!("{unit}{kind}: {PLACEHOLDER}"),
                None => format!("{unit}{PLACEHOLDER}"),
            });
        }
        None => {}
    }
    if !definition.raises.is_empty() {
        lines.push(String::new());
        lines.push("Raises:".to_owned());
        for raised in &definition.raises {
            lines.push(format!("{unit}{raised}: {PLACEHOLDER}"));
        }
    }
    lines
}

/// A NumPy heading is its own name over a row of dashes the same length.
fn numpy_heading(name: &str) -> [String; 2] {
    [name.to_owned(), "-".repeat(name.chars().count())]
}

fn numpy(definition: &Definition, unit: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if !definition.params.is_empty() {
        lines.push(String::new());
        lines.extend(numpy_heading("Parameters"));
        for param in &definition.params {
            let mut kind = param.annotation.clone().unwrap_or_default();
            if param.default.is_some() {
                if kind.is_empty() {
                    kind = "optional".to_owned();
                } else {
                    kind.push_str(", optional");
                }
            }
            lines.push(if kind.is_empty() {
                param.name.clone()
            } else {
                format!("{} : {kind}", param.name)
            });
            lines.push(format!("{unit}{PLACEHOLDER}"));
        }
    }
    match &definition.returns {
        Some(Returns::Value(kind)) => {
            lines.push(String::new());
            lines.extend(numpy_heading("Returns"));
            lines.push(kind.clone().unwrap_or_else(|| PLACEHOLDER.to_owned()));
            lines.push(format!("{unit}{PLACEHOLDER}"));
        }
        Some(Returns::Yield(kind)) => {
            lines.push(String::new());
            lines.extend(numpy_heading("Yields"));
            lines.push(kind.clone().unwrap_or_else(|| PLACEHOLDER.to_owned()));
            lines.push(format!("{unit}{PLACEHOLDER}"));
        }
        None => {}
    }
    if !definition.raises.is_empty() {
        lines.push(String::new());
        lines.extend(numpy_heading("Raises"));
        for raised in &definition.raises {
            lines.push(raised.clone());
            lines.push(format!("{unit}{PLACEHOLDER}"));
        }
    }
    lines
}

fn sphinx(definition: &Definition, _unit: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    for param in &definition.params {
        let optional = if param.default.is_some() {
            format!(
                ", defaults to {}",
                param.default.clone().unwrap_or_default()
            )
        } else {
            String::new()
        };
        lines.push(format!(":param {}: {PLACEHOLDER}{optional}", param.name));
        if let Some(annotation) = &param.annotation {
            let suffix = if param.default.is_some() {
                ", optional"
            } else {
                ""
            };
            lines.push(format!(":type {}: {annotation}{suffix}", param.name));
        }
    }
    for raised in &definition.raises {
        lines.push(format!(":raises {raised}: {PLACEHOLDER}"));
    }
    match &definition.returns {
        Some(Returns::Value(kind)) => {
            lines.push(format!(":return: {PLACEHOLDER}"));
            if let Some(kind) = kind {
                lines.push(format!(":rtype: {kind}"));
            }
        }
        Some(Returns::Yield(kind)) => {
            lines.push(format!(":yield: {PLACEHOLDER}"));
            if let Some(kind) = kind {
                lines.push(format!(":rtype: {kind}"));
            }
        }
        None => {}
    }
    lines
}

// ---- reading the definition ----------------------------------------------

fn line_text(text: &Rope, line: usize) -> String {
    if line >= text.len_lines() {
        return String::new();
    }
    text.line(line).to_string()
}

/// Columns of leading whitespace, counting a tab as one.
///
/// Only ever compared against another line's, so the unit does not matter as
/// long as it is the same one both times.
fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace() && *c != '\n')
        .count()
}

fn strip_comment(line: &str) -> &str {
    // Good enough for a header line: a `#` inside a string in a default value
    // is possible and vanishingly rare, and the cost of getting it wrong is a
    // docstring that is not offered.
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

/// The last line with anything on it before `line`.
///
/// Blank lines are skipped: a gap between a `def` and its docstring is
/// unusual, but the docstring is still the body's first statement.
fn last_statement_before(text: &Rope, line: usize) -> Option<usize> {
    (0..line)
        .rev()
        .find(|candidate| !line_text(text, *candidate).trim().is_empty())
}

/// Walk up from the line that ends a header to the line that starts it.
///
/// A signature can be spread over as many lines as it likes, so the `:` and
/// the `def` are not always the same line.
fn header_start_of(text: &Rope, header_end: usize) -> Option<usize> {
    if !strip_comment(&line_text(text, header_end))
        .trim_end()
        .ends_with(':')
    {
        return None;
    }
    // A signature long enough to need this many lines is not a signature.
    const MAX_HEADER_LINES: usize = 60;
    (header_end.saturating_sub(MAX_HEADER_LINES)..=header_end)
        .rev()
        .find(|candidate| split_keyword(&line_text(text, *candidate)).is_some())
}

fn joined(text: &Rope, from: usize, to: usize) -> String {
    (from..=to)
        .map(|line| line_text(text, line).trim().to_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split `def name(...)` or `class Name(...)` into its kind and everything
/// after the name.
fn split_keyword(header: &str) -> Option<(bool, &str)> {
    let trimmed = header.trim_start();
    let trimmed = trimmed
        .strip_prefix("async ")
        .unwrap_or(trimmed)
        .trim_start();
    for (keyword, is_class) in [("def ", false), ("class ", true)] {
        if let Some(rest) = trimmed.strip_prefix(keyword) {
            let rest = rest.trim_start();
            // A name, then whatever follows it.
            let name_len = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .count();
            if name_len == 0 {
                return None;
            }
            return Some((is_class, &rest[name_len..]));
        }
    }
    None
}

/// The parameters in the first bracketed list of `after_name`.
fn params_of(after_name: &str) -> Vec<Param> {
    let Some(open) = after_name.find('(') else {
        return Vec::new();
    };
    let inside = match balanced_end(&after_name[open..]) {
        Some(close) => &after_name[open + 1..open + close],
        None => return Vec::new(),
    };

    let mut params: Vec<Param> = split_top_level(inside, ',')
        .into_iter()
        .filter_map(|piece| parse_param(piece.trim()))
        .collect();
    // `self` and `cls` are the machinery of a method, not something a caller
    // passes, and no convention documents them.
    if params
        .first()
        .is_some_and(|p| p.name == "self" || p.name == "cls")
    {
        params.remove(0);
    }
    params
}

fn parse_param(piece: &str) -> Option<Param> {
    // A bare `*` or `/` marks where positional and keyword arguments divide.
    // It is punctuation, not a parameter.
    if piece.is_empty() || piece == "*" || piece == "/" {
        return None;
    }
    let (left, default) = match split_once_top_level(piece, '=') {
        Some((left, right)) => (left, Some(right.trim().to_owned())),
        None => (piece, None),
    };
    let (name, annotation) = match split_once_top_level(left, ':') {
        Some((name, kind)) => (name, Some(kind.trim().to_owned())),
        None => (left, None),
    };
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(Param {
        name: name.to_owned(),
        annotation,
        default,
    })
}

/// What the body returns, from the annotation where there is one and from the
/// body itself where there is not.
fn returns_of(after_name: &str, body: &[String]) -> Option<Returns> {
    let annotation = arrow_annotation(after_name);
    let yields = body.iter().any(|line| starts_statement(line, "yield"));
    if yields {
        return Some(Returns::Yield(annotation));
    }
    // `-> None` is a promise that there is nothing to document.
    if annotation.as_deref() == Some("None") {
        return None;
    }
    // A bare `return` ends the function early; it does not produce a value,
    // and a `Returns:` heading over nothing is worse than no heading.
    let returns_value = body
        .iter()
        .any(|line| starts_statement(line, "return") && !line.trim().eq("return"));
    if annotation.is_some() || returns_value {
        return Some(Returns::Value(annotation));
    }
    None
}

fn arrow_annotation(after_name: &str) -> Option<String> {
    let open = after_name.find('(')?;
    let close = balanced_end(&after_name[open..])?;
    let rest = after_name[open + close + 1..].trim();
    let rest = rest.strip_prefix("->")?.trim();
    let rest = rest.trim_end().strip_suffix(':').unwrap_or(rest).trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// True when `line`'s first word is `keyword`.
fn starts_statement(line: &str, keyword: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.strip_prefix(keyword).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '_')
    })
}

/// The exception types raised directly in `body`.
fn raises_in(body: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in body {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("raise ") else {
            continue;
        };
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        // `raise` on its own re-raises whatever is being handled, which has no
        // name to write down.
        if !name.is_empty() && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

/// The lines of the body belonging to a header indented `header_indent`.
///
/// Nested definitions are skipped whole: an inner function's `return` says
/// nothing about the outer one, and a docstring claiming otherwise is worse
/// than one that is merely incomplete.
fn body_of(text: &Rope, header_end: usize, header_indent: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut skipping_from: Option<usize> = None;

    for line in (header_end + 1)..text.len_lines() {
        let raw = line_text(text, line);
        if raw.trim().is_empty() {
            continue;
        }
        let indent = indent_of(&raw);
        if indent <= header_indent {
            break;
        }
        match skipping_from {
            Some(nested) if indent > nested => continue,
            _ => skipping_from = None,
        }
        if split_keyword(&raw).is_some() {
            skipping_from = Some(indent);
            continue;
        }
        lines.push(raw);
    }
    lines
}

/// The parameters of the `__init__` in a class body, which are what somebody
/// building one actually passes.
fn initialiser_params(text: &Rope, header_end: usize, header_indent: usize) -> Vec<Param> {
    for line in (header_end + 1)..text.len_lines() {
        let raw = line_text(text, line);
        if raw.trim().is_empty() {
            continue;
        }
        if indent_of(&raw) <= header_indent {
            break;
        }
        let Some((false, after_name)) = split_keyword(&raw) else {
            continue;
        };
        if !raw.trim_start().contains("__init__") {
            continue;
        }
        // The signature may run past this line; join until the brackets close.
        let mut header = raw.trim().to_owned();
        let mut next = line + 1;
        while balanced_end(&header[header.find('(').unwrap_or(0)..]).is_none()
            && next < text.len_lines()
            && next < line + 60
        {
            header.push(' ');
            header.push_str(line_text(text, next).trim());
            next += 1;
        }
        let after_name = split_keyword(&header).map_or(after_name, |(_, rest)| rest);
        return params_of(after_name);
    }
    Vec::new()
}

// ---- bracket-aware string handling ---------------------------------------

/// The index of the bracket closing the one `text` starts with.
///
/// Quotes are honoured, so a bracket inside a default string value does not
/// count. Returns `None` if the brackets never balance, which is what an
/// unfinished signature looks like.
fn balanced_end(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, c) in text.char_indices() {
        match quote {
            Some(open) => {
                if c == open {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Split on `sep`, ignoring separators inside brackets or quotes.
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        match quote {
            Some(open) => {
                if c == open {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    pieces.push(&text[start..index]);
                    start = index + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    pieces.push(&text[start..]);
    pieces
}

/// Split at the first `sep` that is not inside brackets or quotes.
fn split_once_top_level(text: &str, sep: char) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, c) in text.char_indices() {
        match quote {
            Some(open) => {
                if c == open {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    return Some((&text[..index], &text[index + c.len_utf8()..]));
                }
                _ => {}
            },
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line a docstring is being typed on, marked with a `|` in the
    /// source so the tests read as Python rather than as line arithmetic.
    fn at_marker(source: &str) -> (Rope, usize) {
        let line = source
            .lines()
            .position(|l| l.contains('|'))
            .expect("a marked line");
        (Rope::from_str(&source.replace('|', "")), line)
    }

    fn found(source: &str) -> Definition {
        let (text, line) = at_marker(source);
        definition_above(&text, line).expect("a definition above")
    }

    fn names(definition: &Definition) -> Vec<&str> {
        definition.params.iter().map(|p| p.name.as_str()).collect()
    }

    // ---- what counts as a docstring ---------------------------------------

    #[test]
    fn the_first_line_of_a_def_body_is_a_docstring() {
        let definition = found("def f(a, b):\n    |\n");
        assert!(!definition.is_class);
        assert_eq!(names(&definition), ["a", "b"]);
    }

    #[test]
    fn an_ordinary_string_in_a_body_is_not() {
        let (text, line) = at_marker("def f(a):\n    x = 1\n    |\n");
        assert!(
            definition_above(&text, line).is_none(),
            "the first statement has already been written"
        );
    }

    #[test]
    fn a_string_at_the_top_of_a_file_is_not() {
        let (text, line) = at_marker("|\nimport os\n");
        assert!(definition_above(&text, line).is_none());
    }

    #[test]
    fn a_string_beside_a_def_rather_than_inside_it_is_not() {
        let (text, line) = at_marker("def f(a):\n    pass\n|\n");
        assert!(
            definition_above(&text, line).is_none(),
            "back at the outer indent, this is a sibling of the def"
        );
    }

    /// A gap between the header and the docstring is unusual, but the
    /// docstring is still the body's first statement.
    #[test]
    fn a_blank_line_between_the_def_and_the_docstring_is_allowed() {
        let definition = found("def f(a):\n\n    |\n");
        assert_eq!(names(&definition), ["a"]);
    }

    // ---- parameters -------------------------------------------------------

    #[test]
    fn self_and_cls_are_not_parameters_anybody_passes() {
        assert_eq!(
            names(&found("class A:\n    def f(self, a):\n        |\n")),
            ["a"]
        );
        assert_eq!(
            names(&found("class A:\n    def f(cls, a):\n        |\n")),
            ["a"]
        );
        assert_eq!(
            names(&found("def f(selfish, a):\n    |\n")),
            ["selfish", "a"],
            "only the parameter actually called self"
        );
    }

    #[test]
    fn annotations_and_defaults_are_kept() {
        let definition = found("def f(a: int, b: str = 'x', c=3):\n    |\n");
        assert_eq!(names(&definition), ["a", "b", "c"]);
        assert_eq!(definition.params[0].annotation.as_deref(), Some("int"));
        assert_eq!(definition.params[0].default, None);
        assert_eq!(definition.params[1].annotation.as_deref(), Some("str"));
        assert_eq!(definition.params[1].default.as_deref(), Some("'x'"));
        assert_eq!(definition.params[2].annotation, None);
        assert_eq!(definition.params[2].default.as_deref(), Some("3"));
    }

    /// A comma inside an annotation or a default is not a parameter boundary,
    /// which is why this is bracket-aware rather than a split.
    #[test]
    fn commas_inside_annotations_and_defaults_do_not_divide_parameters() {
        let definition = found("def f(a: Dict[str, int], b=(1, 2), c: str = 'x, y'):\n    |\n");
        assert_eq!(names(&definition), ["a", "b", "c"]);
        assert_eq!(
            definition.params[0].annotation.as_deref(),
            Some("Dict[str, int]")
        );
        assert_eq!(definition.params[1].default.as_deref(), Some("(1, 2)"));
        assert_eq!(definition.params[2].default.as_deref(), Some("'x, y'"));
    }

    #[test]
    fn star_args_keep_their_stars_and_bare_markers_are_dropped() {
        let definition = found("def f(a, *, b, *args, **kwargs):\n    |\n");
        assert_eq!(names(&definition), ["a", "b", "*args", "**kwargs"]);
    }

    #[test]
    fn a_signature_spread_over_several_lines_is_read_whole() {
        let definition = found("def f(\n    a: int,\n    b: str = 'x',\n) -> bool:\n    |\n");
        assert_eq!(names(&definition), ["a", "b"]);
        assert_eq!(
            definition.returns,
            Some(Returns::Value(Some("bool".to_owned())))
        );
    }

    #[test]
    fn a_definition_with_no_parameters_has_none() {
        assert!(names(&found("def f():\n    |\n")).is_empty());
    }

    // ---- what comes back out ----------------------------------------------

    #[test]
    fn an_annotated_return_is_taken_from_the_signature() {
        let definition = found("def f(a) -> int:\n    |\n    return a\n");
        assert_eq!(
            definition.returns,
            Some(Returns::Value(Some("int".to_owned())))
        );
    }

    #[test]
    fn an_unannotated_return_is_found_in_the_body() {
        let definition = found("def f(a):\n    |\n    return a + 1\n");
        assert_eq!(definition.returns, Some(Returns::Value(None)));
    }

    #[test]
    fn a_function_that_returns_nothing_gets_no_returns_section() {
        assert_eq!(found("def f(a):\n    |\n    print(a)\n").returns, None);
        assert_eq!(
            found("def f(a) -> None:\n    |\n    print(a)\n").returns,
            None,
            "-> None is a promise that there is nothing to document"
        );
        assert_eq!(
            found("def f(a):\n    |\n    if a:\n        return\n    print(a)\n").returns,
            None,
            "a bare return leaves early, it does not produce a value"
        );
    }

    #[test]
    fn a_generator_yields_rather_than_returns() {
        let definition = found("def f(a):\n    |\n    for x in a:\n        yield x\n");
        assert_eq!(definition.returns, Some(Returns::Yield(None)));
    }

    /// An inner function's `return` says nothing about the outer one.
    #[test]
    fn a_nested_definition_does_not_lend_its_return_to_its_parent() {
        let definition =
            found("def outer(a):\n    |\n    def inner():\n        return 1\n    print(inner)\n");
        assert_eq!(definition.returns, None);
    }

    #[test]
    fn raised_exceptions_are_collected_in_order_and_deduplicated() {
        let definition = found(
            "def f(a):\n    |\n    if a:\n        raise ValueError('no')\n    if not a:\n        raise KeyError\n    raise ValueError('again')\n",
        );
        assert_eq!(definition.raises, ["ValueError", "KeyError"]);
    }

    #[test]
    fn a_bare_reraise_has_no_name_to_write_down() {
        let definition =
            found("def f(a):\n    |\n    try:\n        g()\n    except E:\n        raise\n");
        assert!(definition.raises.is_empty());
    }

    // ---- classes ----------------------------------------------------------

    /// A class documents what building one takes, which is `__init__`'s
    /// parameters and not the base classes in its own header.
    #[test]
    fn a_class_takes_its_parameters_from_init() {
        let definition = found(
            "class Widget(Base, metaclass=Meta):\n    |\n    def __init__(self, size: int, colour='red'):\n        pass\n",
        );
        assert!(definition.is_class);
        assert_eq!(names(&definition), ["size", "colour"]);
        assert_eq!(definition.params[0].annotation.as_deref(), Some("int"));
        assert_eq!(definition.returns, None, "a class does not return anything");
    }

    #[test]
    fn a_class_with_no_init_has_no_parameters() {
        let definition = found("class A:\n    |\n    x = 1\n");
        assert!(definition.is_class);
        assert!(definition.params.is_empty());
    }

    // ---- rendering --------------------------------------------------------

    fn rendered(source: &str, style: Style) -> String {
        let (body, _) = render(&found(source), style, "    ", "    ");
        body
    }

    const EVERYTHING: &str = "def f(a: int, b='x') -> bool:\n    |\n    raise ValueError\n";

    #[test]
    fn google_puts_every_argument_under_one_heading() {
        let out = rendered(EVERYTHING, Style::Google);
        let expected = concat!(
            "\"\"\"\n",
            "\n",
            "    Args:\n",
            "        a (int): _description_\n",
            "        b (optional): _description_\n",
            "\n",
            "    Returns:\n",
            "        bool: _description_\n",
            "\n",
            "    Raises:\n",
            "        ValueError: _description_\n",
            "    \"\"\"",
        );
        assert_eq!(out, expected, "got:\n{out}");
    }

    /// PEP 257 puts the summary on the line the quotes open on, and so does
    /// every editor that generates these — so that is where the caret goes.
    #[test]
    fn the_caret_lands_where_the_summary_is_written() {
        let (body, caret) = render(&found("def f(a):\n    |\n"), Style::Google, "    ", "    ");
        assert_eq!(caret, 3, "just past the opening quotes");
        let before: String = body.chars().take(caret).collect();
        assert_eq!(before, "\"\"\"");
    }

    #[test]
    fn every_style_names_every_parameter_and_closes_its_quotes() {
        for style in Style::ALL {
            let out = rendered(EVERYTHING, style);
            assert!(out.starts_with("\"\"\""), "{style:?}: {out}");
            assert!(out.ends_with("\"\"\""), "{style:?}: {out}");
            for name in ["a", "b"] {
                assert!(out.contains(name), "{style:?} omits {name}:\n{out}");
            }
            assert!(
                out.contains("bool"),
                "{style:?} omits the return type:\n{out}"
            );
            assert!(
                out.contains("ValueError"),
                "{style:?} omits what it raises:\n{out}"
            );
        }
    }

    #[test]
    fn numpy_underlines_each_heading_to_its_own_width() {
        let out = rendered("def f(a) -> int:\n    |\n", Style::Numpy);
        assert!(out.contains("Parameters\n    ----------\n"), "{out}");
        assert!(out.contains("Returns\n    -------\n"), "{out}");
    }

    #[test]
    fn sphinx_writes_a_field_per_line() {
        let out = rendered("def f(a: int, b='x') -> bool:\n    |\n", Style::Sphinx);
        assert!(out.contains(":param a: _description_\n"), "{out}");
        assert!(out.contains(":type a: int\n"), "{out}");
        assert!(
            out.contains(":param b: _description_, defaults to 'x'\n"),
            "{out}"
        );
        assert!(out.contains(":rtype: bool\n"), "{out}");
    }

    /// A definition with nothing to say still gets its quotes and a line to
    /// write the summary on, which is the whole point of typing them.
    #[test]
    fn a_bare_definition_renders_an_empty_docstring() {
        assert_eq!(
            rendered("def f():\n    |\n", Style::Google),
            "\"\"\"\n    \"\"\""
        );
    }
}
