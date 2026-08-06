//! Finding file references in program output, so they can be clicked.
//!
//! Small feature, disproportionate payoff: a traceback you can click beats one
//! you have to read and then navigate to by hand. Three shapes cover almost
//! everything The Editor will see:
//!
//! * `--> src/main.rs:12:5` — rustc and clippy
//! * `File "src/main.py", line 12` — Python tracebacks
//! * `path/to/file.ext:12:5` or `:12` — near enough everything else
//!
//! Matching is hand-rolled rather than regex-based because it runs over every
//! output line as it streams in, and because the "generic" shape needs context
//! a regex handles clumsily — a Windows path contains a colon of its own.

use std::path::{Path, PathBuf};

/// A file reference found in a line of output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Byte range within the line, for underlining the clickable part.
    pub range: std::ops::Range<usize>,
    pub path: PathBuf,
    /// One-based, as printed.
    pub line: usize,
    /// One-based. `None` when the output gave only a line number.
    pub column: Option<usize>,
}

impl Link {
    /// Resolve against the working directory the program ran in.
    #[must_use]
    pub fn resolve(&self, cwd: &Path) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else {
            cwd.join(&self.path)
        }
    }
}

/// Find every file reference in one line of output.
#[must_use]
pub fn find(line: &str) -> Vec<Link> {
    if let Some(link) = python_traceback(line) {
        return vec![link];
    }
    generic(line)
}

/// `File "src/main.py", line 12, in <module>`
fn python_traceback(line: &str) -> Option<Link> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let rest = trimmed.strip_prefix("File \"")?;
    let quote = rest.find('"')?;
    let path = &rest[..quote];

    let after = rest[quote + 1..].strip_prefix(", line ")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let number = digits.parse().ok()?;

    // The clickable region is the whole `File "..." , line N` phrase.
    let start = indent;
    let end = indent + "File \"".len() + quote + 1 + ", line ".len() + digits.len();

    Some(Link {
        range: start..end.min(line.len()),
        path: PathBuf::from(path),
        line: number,
        column: None,
    })
}

/// `path:line:col` or `path:line`, anywhere in the line.
fn generic(line: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let bytes = line.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let Some(colon) = line[index..].find(':').map(|c| index + c) else {
            break;
        };

        // Numbers after the colon.
        let after = &line[colon + 1..];
        let line_digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if line_digits.is_empty() {
            index = colon + 1;
            continue;
        }

        // The path is the run of path-ish characters before the colon.
        let Some(start) = path_start(line, colon) else {
            index = colon + 1;
            continue;
        };
        let path = &line[start..colon];
        if !looks_like_path(path) {
            index = colon + 1;
            continue;
        }

        let mut end = colon + 1 + line_digits.len();
        let mut column = None;
        // An optional `:col` after the line number.
        if let Some(rest) = line.get(end..).and_then(|r| r.strip_prefix(':')) {
            let column_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !column_digits.is_empty() {
                end += 1 + column_digits.len();
                column = column_digits.parse().ok();
            }
        }

        if let Ok(number) = line_digits.parse() {
            links.push(Link {
                range: start..end,
                path: PathBuf::from(path),
                line: number,
                column,
            });
        }
        index = end;
    }

    links
}

/// Walk backwards from a colon to the start of the path.
fn path_start(line: &str, colon: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut start = colon;
    while start > 0 {
        let c = bytes[start - 1];
        // Stop at whitespace or characters that cannot appear in a path we
        // would want to open.
        if c.is_ascii_whitespace() || matches!(c, b'(' | b')' | b'"' | b'\'' | b'[' | b']' | b',') {
            break;
        }
        start -= 1;
    }

    // A Windows drive letter: `C:\path\file.rs:12` — keep the drive prefix,
    // whose colon is part of the path rather than a line separator.
    if start + 1 < colon
        && bytes.get(start + 1) == Some(&b':')
        && bytes[start].is_ascii_alphabetic()
    {
        // The colon we matched is the drive's, not a line number's.
        if start + 1 == colon {
            return None;
        }
    }

    (start < colon).then_some(start)
}

/// Filter out things that happen to contain a colon and a number.
fn looks_like_path(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > 4096 {
        return false;
    }
    // A URL is not a file to open.
    if candidate.ends_with("http") || candidate.ends_with("https") {
        return false;
    }
    // A bare number before a colon is a time or a ratio, not a file.
    if candidate.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Require something that looks like a filename: an extension, or a
    // separator. `warning:` and `error:` must not match.
    candidate.contains('.') || candidate.contains('/') || candidate.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Link {
        let found = find(line);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one link in {line:?}: {found:?}"
        );
        found.into_iter().next().expect("checked")
    }

    #[test]
    fn a_rustc_error_location_is_found() {
        let link = one("  --> src/main.rs:12:5");
        assert_eq!(link.path, PathBuf::from("src/main.rs"));
        assert_eq!(link.line, 12);
        assert_eq!(link.column, Some(5));
    }

    #[test]
    fn a_python_traceback_frame_is_found() {
        let link = one("  File \"src/main.py\", line 42, in <module>");
        assert_eq!(link.path, PathBuf::from("src/main.py"));
        assert_eq!(link.line, 42);
        assert_eq!(link.column, None);
    }

    #[test]
    fn a_python_traceback_with_a_windows_path_is_found() {
        let link = one(r#"  File "C:\project\src\main.py", line 7, in main"#);
        assert_eq!(link.path, PathBuf::from(r"C:\project\src\main.py"));
        assert_eq!(link.line, 7);
    }

    #[test]
    fn the_clickable_range_covers_the_reference_and_nothing_else() {
        let line = "  --> src/main.rs:12:5";
        let link = one(line);
        assert_eq!(&line[link.range.clone()], "src/main.rs:12:5");
    }

    #[test]
    fn a_line_number_without_a_column_is_accepted() {
        let link = one("src/lib.rs:99: warning: unused");
        assert_eq!(link.line, 99);
        assert_eq!(link.column, None);
    }

    #[test]
    fn several_references_on_one_line_are_all_found() {
        let found = find("a/one.py:1:2 and b/two.py:3:4");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, PathBuf::from("a/one.py"));
        assert_eq!(found[1].line, 3);
    }

    #[test]
    fn diagnostic_prefixes_are_not_mistaken_for_files() {
        // "error:" and "warning:" appear on almost every compiler line.
        assert!(find("error: something went wrong").is_empty());
        assert!(find("warning: 3 warnings emitted").is_empty());
        assert!(
            find("note: run with `RUST_BACKTRACE=1`").is_empty(),
            "a note line has no file reference"
        );
    }

    #[test]
    fn timestamps_and_ratios_are_not_files() {
        assert!(
            find("finished in 12:34").is_empty(),
            "a bare number before a colon is not a path"
        );
        assert!(find("ratio 3:4").is_empty());
    }

    #[test]
    fn urls_are_not_treated_as_files() {
        assert!(
            find("see https://example.com/page").is_empty(),
            "a URL is not a file to open"
        );
    }

    #[test]
    fn a_line_with_no_reference_yields_nothing() {
        assert!(find("Hello, world!").is_empty());
        assert!(find("").is_empty());
        assert!(find("::::").is_empty());
    }

    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let link = one("src/main.rs:1:1");
        assert_eq!(
            link.resolve(Path::new("/project")),
            PathBuf::from("/project/src/main.rs")
        );
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let link = one("/opt/thing/main.py:5");
        assert_eq!(
            link.resolve(Path::new("/project")),
            PathBuf::from("/opt/thing/main.py"),
            "an absolute path must not be joined onto the cwd"
        );
    }

    #[test]
    fn pathological_input_does_not_hang() {
        let long = format!("{}:1", "a/".repeat(5_000));
        let _ = find(&long);
        let colons = ":".repeat(10_000);
        let _ = find(&colons);
    }
}
