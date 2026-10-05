//! Finding which diagnostics a place in a document is about.
//!
//! Plain functions over a document and a list, so the rules for "which
//! problem did the user point at" can be tested without a window.

use editor_config::settings::UnderlineDiagnostics;
use editor_core::document::Document;
use editor_lsp::diagnostics::{Diagnostic, Severity};
use std::ops::Range;
use std::path::Path;

/// A diagnostic's span as character offsets, never running backwards.
pub(super) fn diagnostic_range(doc: &Document, d: &Diagnostic) -> Range<usize> {
    let start = doc.offset_at(d.line as usize, d.column as usize);
    let end = doc.offset_at(d.end_line as usize, d.end_column as usize);
    start..end.max(start)
}

/// Whether the setting asks for this diagnostic to be squiggled in the text.
pub(super) fn is_underlined(d: &Diagnostic, level: UnderlineDiagnostics) -> bool {
    match level {
        UnderlineDiagnostics::All => true,
        UnderlineDiagnostics::Errors => d.severity == Severity::Error,
        UnderlineDiagnostics::None => false,
    }
}

/// The diagnostics whose span covers `offset`.
///
/// Inclusive of the end, so a pointer or caret just past the last character of
/// a squiggle still counts as on it — and so a zero-width diagnostic at the end
/// of a line can be found at all.
pub(super) fn problems_at(doc: &Document, all: &[Diagnostic], offset: usize) -> Vec<Diagnostic> {
    all.iter()
        .filter(|d| {
            let range = diagnostic_range(doc, d);
            range.start <= offset && offset <= range.end
        })
        .cloned()
        .collect()
}

/// The diagnostics that touch `line`, which is what its gutter marker stands for.
///
/// The same test the marker is drawn by, so hovering a marker never comes up
/// empty.
pub(super) fn problems_on_line(doc: &Document, all: &[Diagnostic], line: usize) -> Vec<Diagnostic> {
    let line_start = doc.line_start(line);
    let line_end = line_start + doc.line_len(line);
    all.iter()
        .filter(|d| {
            let range = diagnostic_range(doc, d);
            range.start <= line_end && range.end >= line_start
        })
        .cloned()
        .collect()
}

/// The diagnostics the caret is on, or failing that, the ones on its line.
///
/// The fallback is for a right-click on the gutter marker, which leaves the
/// caret at the start of the line rather than on the squiggle further along.
pub(super) fn problems_at_caret(
    doc: &Document,
    all: &[Diagnostic],
    caret: usize,
) -> Vec<Diagnostic> {
    let exact = problems_at(doc, all, caret);
    if exact.is_empty() {
        problems_on_line(doc, all, doc.line_of(caret))
    } else {
        exact
    }
}

/// A diagnostic in full, for the clipboard.
///
/// `path:line:column:` first, one-based, the shape every terminal and editor
/// recognises as a location; then the message whole, because the detail
/// basedpyright puts after the first line is usually the part that explains it.
pub(super) fn problem_report(path: &Path, d: &Diagnostic) -> String {
    let mut out = format!(
        "{}:{}:{}: {}",
        path.display(),
        d.line + 1,
        d.column + 1,
        d.severity.label()
    );
    if let Some(code) = &d.code {
        out.push_str(&format!(" [{code}]"));
    }
    out.push_str(&format!(" ({})\n{}", d.source, d.message));
    out
}
