//! Collecting diagnostics from several servers at once.
//!
//! Python is typically served by both `ruff` and a type checker, and each
//! publishes its own complete set for a file. Replacing the whole set on every
//! `publishDiagnostics` would mean each server erasing the other's findings, so
//! they are stored per source and merged on read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Ordered worst-first, so sorting puts errors at the top.
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warning",
            Self::Information => "Info",
            Self::Hint => "Hint",
        }
    }

    /// A glyph, so severity is not conveyed by colour alone — PLAN.md §3.11.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        // Every one of these is checked against the bundled fonts by
        // `editor_widgets::glyphs`. The two that are not here any more —
        // `\u{2717}` for an error and `\u{25cf}` for a hint — shipped as empty
        // boxes, because the fonts egui bundles do not have them.
        match self {
            Self::Error => "\u{2716}",
            Self::Warning => "\u{26a0}",
            Self::Information => "\u{2139}",
            Self::Hint => "\u{25aa}",
        }
    }

    #[must_use]
    pub fn from_lsp(severity: Option<lsp_types::DiagnosticSeverity>) -> Self {
        match severity {
            Some(lsp_types::DiagnosticSeverity::ERROR) => Self::Error,
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => Self::Information,
            Some(lsp_types::DiagnosticSeverity::HINT) => Self::Hint,
            // The protocol lets severity be omitted; a server that does so
            // means "this is a problem", so warning is the safe reading.
            _ => Self::Warning,
        }
    }
}

/// One problem in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Zero-based, as the protocol gives them.
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub message: String,
    /// A rule name like `F401`, when the server gives one.
    pub code: Option<String>,
    /// Which server said so, so `ruff` and `pyright` are distinguishable.
    pub source: String,
}

impl Diagnostic {
    /// `F401: 'os' imported but unused (ruff)`
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        if let Some(code) = &self.code {
            out.push_str(code);
            out.push_str(": ");
        }
        out.push_str(self.message.lines().next().unwrap_or(&self.message));
        out.push_str(" (");
        out.push_str(&self.source);
        out.push(')');
        out
    }

    #[must_use]
    pub fn from_lsp(diagnostic: &lsp_types::Diagnostic, source: &str) -> Self {
        Self {
            severity: Severity::from_lsp(diagnostic.severity),
            line: diagnostic.range.start.line,
            column: diagnostic.range.start.character,
            end_line: diagnostic.range.end.line,
            end_column: diagnostic.range.end.character,
            message: diagnostic.message.clone(),
            code: diagnostic.code.as_ref().map(|c| match c {
                lsp_types::NumberOrString::Number(n) => n.to_string(),
                lsp_types::NumberOrString::String(s) => s.clone(),
            }),
            // Prefer what the server calls itself; fall back to which server
            // sent it, so a diagnostic is never unattributed.
            source: diagnostic
                .source
                .clone()
                .unwrap_or_else(|| source.to_owned()),
        }
    }
}

/// How many of each severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub other: usize,
}

impl Counts {
    #[must_use]
    pub fn total(self) -> usize {
        self.errors + self.warnings + self.other
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// Every diagnostic currently known, keyed by file and then by server.
#[derive(Debug, Default)]
pub struct Store {
    /// `path -> server id -> that server's complete set for that file`.
    by_file: HashMap<PathBuf, HashMap<String, Vec<Diagnostic>>>,
}

impl Store {
    /// Replace one server's diagnostics for one file.
    ///
    /// Replacing rather than merging is what the protocol requires: each
    /// `publishDiagnostics` is that server's complete set for that file, and an
    /// empty list means "I have no complaints", which is how a fixed problem
    /// disappears.
    pub fn set(&mut self, path: &Path, server: &str, diagnostics: Vec<Diagnostic>) {
        let per_server = self.by_file.entry(path.to_path_buf()).or_default();
        if diagnostics.is_empty() {
            per_server.remove(server);
        } else {
            per_server.insert(server.to_owned(), diagnostics);
        }
        if per_server.is_empty() {
            self.by_file.remove(path);
        }
    }

    /// Forget everything one server said, for when it crashes or is stopped.
    ///
    /// Leaving its diagnostics behind would show stale problems from a server
    /// that is no longer running to correct them.
    pub fn clear_server(&mut self, server: &str) {
        self.by_file.retain(|_, per_server| {
            per_server.remove(server);
            !per_server.is_empty()
        });
    }

    /// Forget everything about one file, for when it is closed.
    pub fn clear_file(&mut self, path: &Path) {
        self.by_file.remove(path);
    }

    pub fn clear(&mut self) {
        self.by_file.clear();
    }

    /// Every diagnostic for a file, worst first, then by position.
    #[must_use]
    pub fn for_file(&self, path: &Path) -> Vec<Diagnostic> {
        let mut all: Vec<Diagnostic> = self
            .by_file
            .get(path)
            .map(|per_server| per_server.values().flatten().cloned().collect())
            .unwrap_or_default();
        all.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then(a.line.cmp(&b.line))
                .then(a.column.cmp(&b.column))
                .then_with(|| a.message.cmp(&b.message))
        });
        all
    }

    /// Counts for a file, for the status bar.
    #[must_use]
    pub fn counts_for(&self, path: &Path) -> Counts {
        count(
            self.by_file
                .get(path)
                .into_iter()
                .flat_map(HashMap::values)
                .flatten(),
        )
    }

    /// Counts across everything, for the Problems panel header.
    #[must_use]
    pub fn total_counts(&self) -> Counts {
        count(self.by_file.values().flat_map(HashMap::values).flatten())
    }

    /// Every file with problems, sorted by path, each with its diagnostics.
    #[must_use]
    pub fn all(&self) -> Vec<(PathBuf, Vec<Diagnostic>)> {
        let mut files: Vec<PathBuf> = self.by_file.keys().cloned().collect();
        files.sort();
        files
            .into_iter()
            .map(|path| {
                let diagnostics = self.for_file(&path);
                (path, diagnostics)
            })
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }
}

fn count<'a>(diagnostics: impl Iterator<Item = &'a Diagnostic>) -> Counts {
    let mut counts = Counts::default();
    for diagnostic in diagnostics {
        match diagnostic.severity {
            Severity::Error => counts.errors += 1,
            Severity::Warning => counts.warnings += 1,
            _ => counts.other += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(severity: Severity, line: u32, message: &str, source: &str) -> Diagnostic {
        Diagnostic {
            severity,
            line,
            column: 0,
            end_line: line,
            end_column: 5,
            message: message.to_owned(),
            code: None,
            source: source.to_owned(),
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("/project/main.py")
    }

    #[test]
    fn diagnostics_from_two_servers_are_both_kept() {
        // The reason the store is keyed by server at all: ruff and pyright each
        // publish a complete set, and a naive store lets each erase the other.
        let mut store = Store::default();
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Warning, 1, "unused import", "ruff")],
        );
        store.set(
            &path(),
            "pyright",
            vec![diagnostic(Severity::Error, 5, "undefined name", "pyright")],
        );

        let all = store.for_file(&path());
        assert_eq!(all.len(), 2, "one server erased the other's findings");
        assert_eq!(store.counts_for(&path()).errors, 1);
        assert_eq!(store.counts_for(&path()).warnings, 1);
    }

    #[test]
    fn a_server_republishing_replaces_only_its_own_set() {
        let mut store = Store::default();
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Warning, 1, "a", "ruff")],
        );
        store.set(
            &path(),
            "pyright",
            vec![diagnostic(Severity::Error, 2, "b", "pyright")],
        );

        // Ruff publishes again with a different problem.
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Warning, 9, "c", "ruff")],
        );

        let all = store.for_file(&path());
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|d| d.message == "c"));
        assert!(all.iter().any(|d| d.message == "b"), "pyright's survived");
        assert!(!all.iter().any(|d| d.message == "a"), "ruff's old one went");
    }

    #[test]
    fn an_empty_publication_clears_that_servers_diagnostics() {
        // This is how a fixed problem disappears: the server publishes an empty
        // set for the file.
        let mut store = Store::default();
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Error, 1, "problem", "ruff")],
        );
        assert!(!store.is_empty());

        store.set(&path(), "ruff", Vec::new());
        assert!(store.is_empty(), "a fixed problem must disappear");
        assert!(store.counts_for(&path()).is_empty());
    }

    #[test]
    fn clearing_a_server_leaves_the_others_alone() {
        // A crashed server's diagnostics are stale: it is not running to
        // correct them.
        let mut store = Store::default();
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Warning, 1, "a", "ruff")],
        );
        store.set(
            &path(),
            "pyright",
            vec![diagnostic(Severity::Error, 2, "b", "pyright")],
        );

        store.clear_server("pyright");
        let all = store.for_file(&path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].source, "ruff");
    }

    #[test]
    fn clearing_the_last_server_removes_the_file_entirely() {
        let mut store = Store::default();
        store.set(
            &path(),
            "ruff",
            vec![diagnostic(Severity::Error, 1, "x", "ruff")],
        );
        store.clear_server("ruff");
        assert!(store.is_empty());
        assert!(store.all().is_empty());
    }

    #[test]
    fn diagnostics_are_sorted_worst_first_then_by_position() {
        let mut store = Store::default();
        store.set(
            &path(),
            "s",
            vec![
                diagnostic(Severity::Hint, 1, "hint", "s"),
                diagnostic(Severity::Error, 50, "late error", "s"),
                diagnostic(Severity::Warning, 2, "warning", "s"),
                diagnostic(Severity::Error, 10, "early error", "s"),
            ],
        );

        let all = store.for_file(&path());
        let messages: Vec<&str> = all.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            ["early error", "late error", "warning", "hint"],
            "errors first, and within a severity, by line"
        );
    }

    #[test]
    fn counts_are_reported_per_file_and_in_total() {
        let mut store = Store::default();
        let other = PathBuf::from("/project/other.py");
        store.set(
            &path(),
            "s",
            vec![
                diagnostic(Severity::Error, 1, "a", "s"),
                diagnostic(Severity::Warning, 2, "b", "s"),
            ],
        );
        store.set(&other, "s", vec![diagnostic(Severity::Error, 1, "c", "s")]);

        assert_eq!(store.counts_for(&path()).errors, 1);
        assert_eq!(store.counts_for(&other).errors, 1);

        let total = store.total_counts();
        assert_eq!(total.errors, 2);
        assert_eq!(total.warnings, 1);
        assert_eq!(total.total(), 3);
    }

    #[test]
    fn a_file_with_no_diagnostics_reports_empty_rather_than_missing() {
        let store = Store::default();
        assert!(store.for_file(&path()).is_empty());
        assert!(store.counts_for(&path()).is_empty());
    }

    #[test]
    fn closing_a_file_forgets_it() {
        let mut store = Store::default();
        store.set(&path(), "s", vec![diagnostic(Severity::Error, 1, "x", "s")]);
        store.clear_file(&path());
        assert!(store.is_empty());
    }

    #[test]
    fn files_are_listed_in_a_stable_order() {
        let mut store = Store::default();
        for name in ["z.py", "a.py", "m.py"] {
            store.set(
                Path::new(name),
                "s",
                vec![diagnostic(Severity::Error, 1, "x", "s")],
            );
        }
        let listed: Vec<String> = store
            .all()
            .into_iter()
            .map(|(p, _)| p.display().to_string())
            .collect();
        assert_eq!(listed, ["a.py", "m.py", "z.py"]);
    }

    #[test]
    fn a_summary_names_the_rule_and_the_server() {
        let mut d = diagnostic(Severity::Warning, 1, "'os' imported but unused", "Ruff");
        d.code = Some("F401".to_owned());
        assert_eq!(d.summary(), "F401: 'os' imported but unused (Ruff)");
    }

    #[test]
    fn a_multi_line_message_is_summarised_to_its_first_line() {
        // rust-analyzer's messages often run to a paragraph; the Problems panel
        // is a list, not a document.
        let d = diagnostic(
            Severity::Error,
            1,
            "mismatched types\nexpected `u32`, found `String`",
            "rustc",
        );
        assert_eq!(d.summary(), "mismatched types (rustc)");
    }

    #[test]
    fn a_diagnostic_with_no_severity_is_treated_as_a_warning() {
        // The protocol allows it, and a server that omits severity still means
        // "this is a problem".
        assert_eq!(Severity::from_lsp(None), Severity::Warning);
        assert_eq!(
            Severity::from_lsp(Some(lsp_types::DiagnosticSeverity::ERROR)),
            Severity::Error
        );
    }

    #[test]
    fn severity_is_conveyed_by_glyph_as_well_as_by_name() {
        let mut glyphs = std::collections::HashSet::new();
        for severity in [
            Severity::Error,
            Severity::Warning,
            Severity::Information,
            Severity::Hint,
        ] {
            assert!(
                glyphs.insert(severity.glyph()),
                "{severity:?} shares a glyph, so colour would be the only cue"
            );
        }
    }

    #[test]
    fn a_diagnostic_is_attributed_even_when_the_server_gives_no_source() {
        let raw = lsp_types::Diagnostic {
            range: lsp_types::Range::default(),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: "something".to_owned(),
            source: None,
            ..lsp_types::Diagnostic::default()
        };
        let converted = Diagnostic::from_lsp(&raw, "pyright");
        assert_eq!(converted.source, "pyright");

        // ...and what the server calls itself wins when it says.
        let named = lsp_types::Diagnostic {
            source: Some("Ruff".to_owned()),
            ..raw
        };
        assert_eq!(Diagnostic::from_lsp(&named, "ruff").source, "Ruff");
    }
}
