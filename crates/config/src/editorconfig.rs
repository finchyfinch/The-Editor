//! `.editorconfig`, which lets a project state its own house style.
//!
//! A small, well-defined format that every editor supports, and the reason two
//! people working on the same repository do not fight over tabs in the diff.
//! A project's file wins over The Editor's settings, because it is a statement
//! about *that code* rather than about this user.
//!
//! Implemented directly rather than with a crate. The specification is short,
//! the half of it anyone uses is shorter still, and the glob syntax is the one
//! part with any real content — this is less code than vetting a dependency for
//! it would be reading.
//!
//! What is honoured: `indent_style`, `indent_size`/`tab_width`,
//! `trim_trailing_whitespace`, `insert_final_newline`, `end_of_line`, and
//! `root`. `charset` and `max_line_length` are parsed and ignored, because
//! acting on them means re-encoding a file or wrapping it, and doing either
//! silently on save would be worse than not supporting them.

use std::path::Path;

/// Settings that apply to one file, with anything unstated left `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileStyle {
    /// True for spaces, false for tabs.
    pub insert_spaces: Option<bool>,
    pub indent_width: Option<usize>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
    /// `"lf"` or `"crlf"`, lowercased.
    pub end_of_line: Option<&'static str>,
}

impl FileStyle {
    /// Fill anything this does not state from `other`.
    ///
    /// Nearer files are applied last and win, which is what the specification
    /// requires: a section in a subdirectory overrides the same key higher up.
    fn fill_from(&mut self, other: Self) {
        self.insert_spaces = self.insert_spaces.or(other.insert_spaces);
        self.indent_width = self.indent_width.or(other.indent_width);
        self.trim_trailing_whitespace = self
            .trim_trailing_whitespace
            .or(other.trim_trailing_whitespace);
        self.insert_final_newline = self.insert_final_newline.or(other.insert_final_newline);
        self.end_of_line = self.end_of_line.or(other.end_of_line);
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// Read every `.editorconfig` from `file`'s directory upwards and combine them.
///
/// Stops at a file declaring `root = true`, as the specification says, so a
/// project cannot be affected by a stray `.editorconfig` in someone's home
/// directory.
#[must_use]
pub fn style_for(file: &Path) -> FileStyle {
    let mut style = FileStyle::default();
    let mut directory = file.parent().map(Path::to_path_buf);

    while let Some(dir) = directory {
        let candidate = dir.join(".editorconfig");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            let (found, is_root) = parse(&text, file, &dir);
            // Nearer files were applied first, so they keep what they set.
            style.fill_from(found);
            if is_root {
                break;
            }
        }
        directory = dir.parent().map(Path::to_path_buf);
    }
    style
}

/// Parse one file. Returns what applies to `file`, and whether this is a root.
fn parse(text: &str, file: &Path, base: &Path) -> (FileStyle, bool) {
    let mut style = FileStyle::default();
    let mut is_root = false;
    // Sections are applied in order and later ones win, so a `[*.py]` after a
    // `[*]` overrides it.
    let mut section_matches = false;
    let mut in_preamble = true;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(pattern) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_preamble = false;
            section_matches = matches(pattern, file, base);
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();

        if in_preamble {
            if key == "root" {
                is_root = value == "true";
            }
            continue;
        }
        if !section_matches {
            continue;
        }

        match key.as_str() {
            "indent_style" => match value.as_str() {
                "space" => style.insert_spaces = Some(true),
                "tab" => style.insert_spaces = Some(false),
                _ => {}
            },
            // `indent_size = tab` means "whatever tab_width says", which is
            // the default anyway, so it is simply not a width.
            "indent_size" | "tab_width" => {
                if let Ok(width) = value.parse::<usize>()
                    && (1..=16).contains(&width)
                {
                    style.indent_width = Some(width);
                }
            }
            "trim_trailing_whitespace" => style.trim_trailing_whitespace = Some(value == "true"),
            "insert_final_newline" => style.insert_final_newline = Some(value == "true"),
            "end_of_line" => match value.as_str() {
                "lf" => style.end_of_line = Some("lf"),
                "crlf" => style.end_of_line = Some("crlf"),
                _ => {}
            },
            _ => {}
        }
    }

    (style, is_root)
}

/// Whether an editorconfig section pattern matches a file.
///
/// The subset that matters: `*`, `**`, `?`, `{a,b}` and character classes are
/// the documented syntax; `*`, `**`, `?` and braces cover essentially every
/// real file. A pattern with no separator matches on the file name alone,
/// which is what makes the near-universal `[*.py]` work.
fn matches(pattern: &str, file: &Path, base: &Path) -> bool {
    // Braces first: `{py,pyi}` is one alternation, and expanding it into
    // separate patterns is much simpler than matching it in place.
    if let Some(open) = pattern.find('{')
        && let Some(close) = pattern[open..].find('}').map(|i| i + open)
    {
        let (head, rest) = (&pattern[..open], &pattern[close + 1..]);
        return pattern[open + 1..close]
            .split(',')
            .any(|option| matches(&format!("{head}{option}{rest}"), file, base));
    }

    let subject = if pattern.contains('/') {
        // Anchored at the directory holding the .editorconfig.
        match file.strip_prefix(base) {
            Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
            Err(_) => return false,
        }
    } else {
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);

    glob(pattern.as_bytes(), subject.as_bytes())
}

/// `*` stops at a `/`, `**` does not, `?` is one character.
fn glob(pattern: &[u8], subject: &[u8]) -> bool {
    match pattern.first() {
        None => subject.is_empty(),
        Some(b'*') => {
            if pattern.get(1) == Some(&b'*') {
                let rest = &pattern[2..];
                // `**` spans separators, so try it against every suffix.
                (0..=subject.len()).any(|i| glob(rest, &subject[i..]))
            } else {
                let rest = &pattern[1..];
                (0..=subject.len())
                    .take_while(|i| !subject[..*i].contains(&b'/'))
                    .any(|i| glob(rest, &subject[i..]))
            }
        }
        Some(b'?') => {
            !subject.is_empty() && subject[0] != b'/' && glob(&pattern[1..], &subject[1..])
        }
        Some(c) => !subject.is_empty() && subject[0] == *c && glob(&pattern[1..], &subject[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(config: &str, file: &str) -> FileStyle {
        parse(config, Path::new(file), Path::new("/p")).0
    }

    #[test]
    fn a_star_section_applies_to_everything() {
        let s = style("[*]\nindent_style = space\nindent_size = 2\n", "/p/main.py");
        assert_eq!(s.insert_spaces, Some(true));
        assert_eq!(s.indent_width, Some(2));
    }

    #[test]
    fn a_later_section_overrides_an_earlier_one() {
        // The near-universal shape: a general rule, then one for Python.
        let s = style(
            "[*]\nindent_size = 2\n\n[*.py]\nindent_size = 4\n",
            "/p/main.py",
        );
        assert_eq!(s.indent_width, Some(4));
    }

    #[test]
    fn a_section_for_another_language_is_ignored() {
        let s = style("[*.js]\nindent_size = 2\n", "/p/main.py");
        assert_eq!(s.indent_width, None);
    }

    #[test]
    fn tabs_are_understood_as_well_as_spaces() {
        let s = style("[*]\nindent_style = tab\ntab_width = 8\n", "/p/main.go");
        assert_eq!(s.insert_spaces, Some(false));
        assert_eq!(s.indent_width, Some(8));
    }

    #[test]
    fn the_save_policies_are_read() {
        let s = style(
            "[*]\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n",
            "/p/main.py",
        );
        assert_eq!(s.trim_trailing_whitespace, Some(true));
        assert_eq!(s.insert_final_newline, Some(true));
    }

    #[test]
    fn false_is_honoured_as_well_as_true() {
        // A project turning a policy *off* has to be able to.
        let s = style("[*]\ntrim_trailing_whitespace = false\n", "/p/x.py");
        assert_eq!(s.trim_trailing_whitespace, Some(false));
    }

    #[test]
    fn braces_expand_into_alternatives() {
        for name in ["main.py", "types.pyi"] {
            let s = style("[*.{py,pyi}]\nindent_size = 4\n", &format!("/p/{name}"));
            assert_eq!(s.indent_width, Some(4), "{name}");
        }
        assert_eq!(
            style("[*.{py,pyi}]\nindent_size = 4\n", "/p/a.js").indent_width,
            None
        );
    }

    #[test]
    fn a_pattern_with_a_slash_is_anchored_to_the_config_directory() {
        let s = style("[src/*.py]\nindent_size = 3\n", "/p/src/main.py");
        assert_eq!(s.indent_width, Some(3));
        assert_eq!(
            style("[src/*.py]\nindent_size = 3\n", "/p/lib/main.py").indent_width,
            None
        );
    }

    #[test]
    fn a_single_star_does_not_cross_directories_but_a_double_one_does() {
        assert!(!glob(b"src/*.py", b"src/deep/main.py"));
        assert!(glob(b"src/**.py", b"src/deep/main.py"));
        assert!(glob(b"**/main.py", b"a/b/main.py"));
    }

    #[test]
    fn a_question_mark_matches_exactly_one_character() {
        assert!(glob(b"?.py", b"a.py"));
        assert!(!glob(b"?.py", b"ab.py"));
    }

    #[test]
    fn keys_and_values_are_case_insensitive() {
        // Real files in the wild are inconsistent about this.
        let s = style("[*]\nIndent_Style = SPACE\n", "/p/x.py");
        assert_eq!(s.insert_spaces, Some(true));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let s = style(
            "# a comment\n; another\n\n[*]\nindent_size = 2  \n",
            "/p/x.py",
        );
        assert_eq!(s.indent_width, Some(2));
    }

    #[test]
    fn root_is_recognised_only_in_the_preamble() {
        assert!(parse("root = true\n[*]\n", Path::new("/p/x.py"), Path::new("/p")).1);
        // A `root` inside a section is a key of that section, not a declaration.
        assert!(!parse("[*]\nroot = true\n", Path::new("/p/x.py"), Path::new("/p")).1);
    }

    #[test]
    fn a_nonsense_width_is_ignored_rather_than_applied() {
        // Guards against `indent_size = 0`, which would divide by zero, and
        // against a hand-typed 400.
        for value in ["0", "400", "four"] {
            let s = style(&format!("[*]\nindent_size = {value}\n"), "/p/x.py");
            assert_eq!(s.indent_width, None, "{value}");
        }
    }

    #[test]
    fn nearer_settings_win_over_further_ones() {
        let mut near = FileStyle {
            indent_width: Some(2),
            ..FileStyle::default()
        };
        let far = FileStyle {
            indent_width: Some(8),
            insert_spaces: Some(false),
            ..FileStyle::default()
        };
        near.fill_from(far);
        assert_eq!(
            near.indent_width,
            Some(2),
            "the nearer file keeps its value"
        );
        assert_eq!(near.insert_spaces, Some(false), "and inherits the rest");
    }

    #[test]
    fn an_empty_style_is_recognisable_so_settings_can_be_left_alone() {
        assert!(FileStyle::default().is_empty());
        assert!(!style("[*]\nindent_size = 2\n", "/p/x.py").is_empty());
    }

    #[test]
    fn a_project_with_no_editorconfig_yields_nothing() {
        assert!(style_for(Path::new("/definitely/not/here/x.py")).is_empty());
    }
}
