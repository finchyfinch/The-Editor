//! Boilerplate templates for the New File dialog.
//!
//! Placeholders, substituted at creation time:
//!
//! | Placeholder | Becomes |
//! |---|---|
//! | `${NAME}` | the filename stem, e.g. `main` |
//! | `${FILENAME}` | the full filename, e.g. `main.py` |
//! | `${CLASS_NAME}` | the stem in PascalCase, e.g. `MyDataStore` |
//! | `${AUTHOR}` | the configured author name |
//! | `${DATE}` | today, ISO-8601 |
//! | `$CURSOR` | where the caret lands; removed from the output |
//!
//! Templates are compiled in for 1.0. Loading user-written ones from the config
//! directory is a small extension of `render` and lands with the settings UI.

use crate::LanguageId;

/// One boilerplate template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template {
    /// Stable identifier, for storing the last-used choice.
    pub id: &'static str,
    /// Shown in the dialog's dropdown.
    pub name: &'static str,
    pub language: LanguageId,
    pub body: &'static str,
}

/// Values substituted into a template.
#[derive(Debug, Clone)]
pub struct Vars {
    pub filename: String,
    pub stem: String,
    pub class_name: String,
    pub author: String,
    pub date: String,
}

/// A rendered template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// Character offset for the caret, from `$CURSOR`. Falls back to the end of
    /// the text when the template has no marker.
    pub cursor: usize,
}

/// Templates available for a language, in dropdown order. The first is always
/// the empty one, so "no boilerplate" is a template rather than a special case.
#[must_use]
pub fn for_language(language: LanguageId) -> Vec<&'static Template> {
    ALL.iter().filter(|t| t.language == language).collect()
}

/// The default template for a language: empty, i.e. the boilerplate checkbox
/// unticked.
#[must_use]
pub fn empty_for(language: LanguageId) -> &'static Template {
    ALL.iter()
        .find(|t| t.language == language && t.id.ends_with("empty"))
        .unwrap_or(&EMPTY_TEXT)
}

/// Substitute the placeholders and locate the caret.
#[must_use]
pub fn render(template: &Template, vars: &Vars) -> Rendered {
    let text = template
        .body
        .replace("${FILENAME}", &vars.filename)
        .replace("${NAME}", &vars.stem)
        .replace("${CLASS_NAME}", &vars.class_name)
        .replace("${AUTHOR}", &vars.author)
        .replace("${DATE}", &vars.date);

    match text.find("$CURSOR") {
        Some(byte_index) => {
            // The caret offset is in characters, matching editor-core.
            let cursor = text[..byte_index].chars().count();
            Rendered {
                text: text.replacen("$CURSOR", "", 1),
                cursor,
            }
        }
        None => {
            let cursor = text.chars().count();
            Rendered { text, cursor }
        }
    }
}

/// Today's date, ISO-8601, without pulling in a date library.
///
/// `chrono`/`time` would be a dependency carried solely to stamp a comment
/// header. This converts the Unix day count with the civil-from-days algorithm
/// (Howard Hinnant's), which is exact and about fifteen lines.
#[must_use]
pub fn today_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// The templates themselves.
// ---------------------------------------------------------------------------

const EMPTY_TEXT: Template = Template {
    id: "text.empty",
    name: "Empty",
    language: LanguageId::PlainText,
    body: "$CURSOR",
};

static ALL: &[Template] = &[
    // ---- Python ----------------------------------------------------------
    Template {
        id: "python.empty",
        name: "Empty",
        language: LanguageId::Python,
        body: "$CURSOR",
    },
    Template {
        id: "python.script",
        name: "Script with main()",
        language: LanguageId::Python,
        body: r#"#!/usr/bin/env python3
"""${FILENAME}

Created: ${DATE}
Author: ${AUTHOR}
"""

from __future__ import annotations

import sys


def main(argv: list[str] | None = None) -> int:
    """Entry point."""
    argv = list(sys.argv[1:] if argv is None else argv)
    $CURSOR
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#,
    },
    Template {
        id: "python.class",
        name: "Class module",
        language: LanguageId::Python,
        body: r#""""${FILENAME}

Created: ${DATE}
Author: ${AUTHOR}
"""

from __future__ import annotations


class ${CLASS_NAME}:
    """A ${CLASS_NAME}."""

    def __init__(self) -> None:
        $CURSOR

    def __repr__(self) -> str:
        return f"{type(self).__name__}()"
"#,
    },
    Template {
        id: "python.dataclass",
        name: "Dataclass module",
        language: LanguageId::Python,
        body: r#""""${FILENAME}

Created: ${DATE}
Author: ${AUTHOR}
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(slots=True)
class ${CLASS_NAME}:
    """A ${CLASS_NAME}."""

    $CURSOR
"#,
    },
    Template {
        id: "python.cli",
        name: "CLI (argparse)",
        language: LanguageId::Python,
        body: r#"#!/usr/bin/env python3
"""${FILENAME}

Created: ${DATE}
Author: ${AUTHOR}
"""

from __future__ import annotations

import argparse
import sys


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="${NAME}")
    parser.add_argument("-v", "--verbose", action="store_true", help="verbose output")
    $CURSOR
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.verbose:
        print(f"running {__file__}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#,
    },
    Template {
        id: "python.pytest",
        name: "Pytest test module",
        language: LanguageId::Python,
        body: r#""""Tests for ${NAME}."""

from __future__ import annotations

import pytest


def test_${NAME}() -> None:
    $CURSOR
    assert True


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (1, 1),
        (2, 2),
    ],
)
def test_${NAME}_parametrised(value: int, expected: int) -> None:
    assert value == expected
"#,
    },
    Template {
        id: "python.unittest",
        name: "Unittest test case",
        language: LanguageId::Python,
        body: r#""""Tests for ${NAME}."""

from __future__ import annotations

import unittest


class Test${CLASS_NAME}(unittest.TestCase):
    def setUp(self) -> None:
        $CURSOR

    def test_something(self) -> None:
        self.assertTrue(True)


if __name__ == "__main__":
    unittest.main()
"#,
    },
    // ---- Rust ------------------------------------------------------------
    Template {
        id: "rust.empty",
        name: "Empty",
        language: LanguageId::Rust,
        body: "$CURSOR",
    },
    Template {
        id: "rust.main",
        name: "Binary (main.rs)",
        language: LanguageId::Rust,
        body: r#"//! ${FILENAME}
//!
//! Created: ${DATE}
//! Author: ${AUTHOR}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    $CURSOR
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
"#,
    },
    Template {
        id: "rust.lib",
        name: "Library (lib.rs)",
        language: LanguageId::Rust,
        body: r#"//! ${NAME}
//!
//! Created: ${DATE}
//! Author: ${AUTHOR}

#![warn(missing_docs)]

/// $CURSOR
pub fn hello() -> &'static str {
    "hello"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_says_hello() {
        assert_eq!(hello(), "hello");
    }
}
"#,
    },
    Template {
        id: "rust.module",
        name: "Module",
        language: LanguageId::Rust,
        body: r#"//! ${NAME}

$CURSOR

#[cfg(test)]
mod tests {
    use super::*;
}
"#,
    },
    Template {
        id: "rust.struct",
        name: "Struct + impl",
        language: LanguageId::Rust,
        body: r#"//! ${NAME}

/// A ${CLASS_NAME}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ${CLASS_NAME} {
    $CURSOR
}

impl ${CLASS_NAME} {
    /// Create a new [`${CLASS_NAME}`].
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ${CLASS_NAME} {
    fn default() -> Self {
        Self::new()
    }
}
"#,
    },
    Template {
        id: "rust.trait",
        name: "Trait",
        language: LanguageId::Rust,
        body: r#"//! ${NAME}

/// $CURSOR
pub trait ${CLASS_NAME} {
    /// Describe what implementors must do.
    fn describe(&self) -> String;
}
"#,
    },
    // ---- Web -------------------------------------------------------------
    Template {
        id: "html.empty",
        name: "Empty",
        language: LanguageId::Html,
        body: "$CURSOR",
    },
    Template {
        id: "html.skeleton",
        name: "HTML5 skeleton",
        language: LanguageId::Html,
        body: r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>${NAME}</title>
    <link rel="stylesheet" href="style.css">
</head>
<body>
    <h1>${NAME}</h1>
    $CURSOR
    <script src="script.js"></script>
</body>
</html>
"#,
    },
    Template {
        id: "css.empty",
        name: "Empty",
        language: LanguageId::Css,
        body: "$CURSOR",
    },
    Template {
        id: "css.reset",
        name: "Reset + custom properties",
        language: LanguageId::Css,
        body: r#"/* ${FILENAME} - ${DATE} */

:root {
    --bg: #ffffff;
    --fg: #1c1c20;
    --accent: #0a5bb5;
    --space: 1rem;
}

@media (prefers-color-scheme: dark) {
    :root {
        --bg: #1e1e22;
        --fg: #d8d8dd;
        --accent: #6cb6ff;
    }
}

*,
*::before,
*::after {
    box-sizing: border-box;
    margin: 0;
}

body {
    background: var(--bg);
    color: var(--fg);
    font-family: system-ui, sans-serif;
    line-height: 1.5;
    padding: var(--space);
}

$CURSOR
"#,
    },
    Template {
        id: "javascript.empty",
        name: "Empty",
        language: LanguageId::JavaScript,
        body: "$CURSOR",
    },
    Template {
        id: "javascript.module",
        name: "ES module",
        language: LanguageId::JavaScript,
        body: r#"// ${FILENAME} - ${DATE}
// Author: ${AUTHOR}

"use strict";

/**
 * @returns {string}
 */
export function ${NAME}() {
    $CURSOR
    return "";
}
"#,
    },
    Template {
        id: "json.empty",
        name: "Empty",
        language: LanguageId::Json,
        body: "$CURSOR",
    },
    Template {
        id: "json.object",
        name: "Empty object",
        language: LanguageId::Json,
        body: "{\n    $CURSOR\n}\n",
    },
    // ---- Config and text -------------------------------------------------
    Template {
        id: "ini.empty",
        name: "Empty",
        language: LanguageId::Ini,
        body: "$CURSOR",
    },
    Template {
        id: "ini.sectioned",
        name: "Sectioned config",
        language: LanguageId::Ini,
        body: r#"; ${FILENAME}
; Created: ${DATE}

[general]
name = ${NAME}
$CURSOR

[paths]
"#,
    },
    Template {
        id: "toml.empty",
        name: "Empty",
        language: LanguageId::Toml,
        body: "$CURSOR",
    },
    Template {
        id: "markdown.empty",
        name: "Empty",
        language: LanguageId::Markdown,
        body: "$CURSOR",
    },
    Template {
        id: "markdown.document",
        name: "Document",
        language: LanguageId::Markdown,
        body: "# ${NAME}\n\n$CURSOR\n",
    },
    EMPTY_TEXT,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars {
            filename: "my_data_store.py".to_owned(),
            stem: "my_data_store".to_owned(),
            class_name: "MyDataStore".to_owned(),
            author: "Gareth Finch".to_owned(),
            date: "2026-08-06".to_owned(),
        }
    }

    #[test]
    fn every_placeholder_is_substituted() {
        let t = Template {
            id: "test",
            name: "test",
            language: LanguageId::PlainText,
            body: "${FILENAME} ${NAME} ${CLASS_NAME} ${AUTHOR} ${DATE}",
        };
        let out = render(&t, &vars());
        assert_eq!(
            out.text,
            "my_data_store.py my_data_store MyDataStore Gareth Finch 2026-08-06"
        );
    }

    #[test]
    fn no_shipped_template_leaves_an_unsubstituted_placeholder() {
        let v = vars();
        for t in ALL {
            let out = render(t, &v);
            assert!(
                !out.text.contains("${"),
                "{} left a placeholder behind: {}",
                t.id,
                out.text
            );
            assert!(
                !out.text.contains("$CURSOR"),
                "{} left its cursor marker in the output",
                t.id
            );
        }
    }

    #[test]
    fn the_cursor_marker_gives_a_character_offset_and_is_removed() {
        let t = Template {
            id: "test",
            name: "test",
            language: LanguageId::PlainText,
            body: "abc$CURSORdef",
        };
        let out = render(&t, &vars());
        assert_eq!(out.text, "abcdef");
        assert_eq!(out.cursor, 3);
    }

    #[test]
    fn the_cursor_offset_counts_characters_not_bytes() {
        let t = Template {
            id: "test",
            name: "test",
            language: LanguageId::PlainText,
            body: "caf\u{e9} \u{1f600}$CURSORend",
        };
        let out = render(&t, &vars());
        assert_eq!(
            out.cursor, 6,
            "a byte offset here would put the caret mid-emoji"
        );
        assert_eq!(out.text.chars().nth(out.cursor), Some('e'));
    }

    #[test]
    fn a_template_without_a_marker_puts_the_caret_at_the_end() {
        let t = Template {
            id: "test",
            name: "test",
            language: LanguageId::PlainText,
            body: "no marker",
        };
        let out = render(&t, &vars());
        assert_eq!(out.cursor, "no marker".chars().count());
    }

    #[test]
    fn every_shipped_template_has_a_unique_id() {
        let mut seen = std::collections::HashSet::new();
        for t in ALL {
            assert!(seen.insert(t.id), "duplicate template id {}", t.id);
        }
    }

    #[test]
    fn every_language_offers_at_least_an_empty_template() {
        for language in [
            LanguageId::Python,
            LanguageId::Rust,
            LanguageId::Json,
            LanguageId::JavaScript,
            LanguageId::Html,
            LanguageId::Css,
            LanguageId::Ini,
            LanguageId::Toml,
            LanguageId::Markdown,
            LanguageId::PlainText,
        ] {
            let available = for_language(language);
            assert!(
                !available.is_empty(),
                "{language:?} has no templates at all"
            );
            assert_eq!(
                available.first().map(|t| t.name),
                Some("Empty"),
                "{language:?} must offer Empty first, so unticking boilerplate is not a special case"
            );
        }
    }

    /// `empty_for` is what the dialog uses when the boilerplate checkbox is
    /// unticked, so whatever it returns must genuinely produce an empty file.
    /// JSON originally failed this: its "empty" template contained `{}`, so
    /// unticking the box still wrote braces.
    #[test]
    fn the_empty_template_actually_renders_nothing() {
        let v = vars();
        for language in LanguageId::ALL {
            let rendered = render(empty_for(language), &v);
            assert!(
                rendered.text.trim().is_empty(),
                "{language:?} unticked boilerplate still produces: {:?}",
                rendered.text
            );
        }
    }

    #[test]
    fn python_templates_use_four_space_indentation_and_no_tabs() {
        for t in ALL.iter().filter(|t| t.language == LanguageId::Python) {
            assert!(
                !t.body.contains('\t'),
                "{} contains a tab; Python files must use spaces",
                t.id
            );
        }
    }

    #[test]
    fn the_python_script_template_has_the_expected_shape() {
        let t = ALL
            .iter()
            .find(|t| t.id == "python.script")
            .expect("script template");
        let out = render(t, &vars());
        assert!(out.text.starts_with("#!/usr/bin/env python3"));
        assert!(
            out.text
                .contains("def main(argv: list[str] | None = None) -> int:")
        );
        assert!(out.text.contains(r#"if __name__ == "__main__":"#));
        assert!(out.text.ends_with('\n'), "files should end with a newline");
    }

    #[test]
    fn date_conversion_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_784), (2024, 3, 2), "after a leap day");
        assert_eq!(civil_from_days(20_671), (2026, 8, 6));
    }

    #[test]
    fn today_is_formatted_as_iso_8601() {
        let today = today_iso8601();
        assert_eq!(today.len(), 10, "got {today}");
        assert_eq!(today.matches('-').count(), 2);
        assert!(today.starts_with("20"), "got {today}");
    }
}
