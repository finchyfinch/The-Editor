//! Filename validation for the New File dialog and Rename.
//!
//! Rejecting a bad name up front is much kinder than letting the OS fail the
//! write afterwards with an errno. Windows is the strict case and its rules are
//! surprising — `CON.py` is not a legal filename, and neither is `report.` —
//! so the same rules are enforced on every platform. A project created on Linux
//! should not become un-checkoutable on Windows.

use std::path::Path;

/// Characters no platform will accept in a filename.
const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Names reserved by Windows for DOS devices, in any letter case, with or
/// without an extension. `NUL.txt` is as invalid as `NUL`.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why a filename was rejected. Each variant's `Display` is shown directly to
/// the user, so the text explains the fix rather than naming the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    IllegalCharacter(char),
    ControlCharacter,
    Reserved(String),
    TrailingSpaceOrDot,
    TooLong(usize),
    Relative,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "Enter a file name"),
            Self::IllegalCharacter(c) => {
                write!(f, "A file name cannot contain {c:?}")
            }
            Self::ControlCharacter => write!(f, "A file name cannot contain control characters"),
            Self::Reserved(name) => write!(
                f,
                "{name} is a reserved device name on Windows and cannot be used"
            ),
            Self::TrailingSpaceOrDot => {
                write!(f, "A file name cannot end with a space or a dot")
            }
            Self::TooLong(n) => write!(f, "A file name cannot be longer than {n} characters"),
            Self::Relative => write!(f, "Enter a file name, not a path"),
        }
    }
}

impl std::error::Error for NameError {}

/// Longest filename most filesystems accept.
const MAX_LEN: usize = 255;

/// Check a single filename — not a path.
///
/// # Errors
/// Returns the first problem found, for display next to the input field.
pub fn validate(name: &str) -> Result<(), NameError> {
    if name.trim().is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > MAX_LEN {
        return Err(NameError::TooLong(MAX_LEN));
    }
    if name == "." || name == ".." {
        return Err(NameError::Relative);
    }

    for c in name.chars() {
        if ILLEGAL.contains(&c) {
            return Err(NameError::IllegalCharacter(c));
        }
        if c.is_control() {
            return Err(NameError::ControlCharacter);
        }
    }

    // Windows silently strips these, so `report.` becomes `report` and the file
    // is not where the user expects it.
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(NameError::TrailingSpaceOrDot);
    }

    // The reserved check applies to the stem, so `CON.py` is caught too.
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem.trim())) {
        return Err(NameError::Reserved(stem.to_uppercase()));
    }

    Ok(())
}

/// Convert a filename stem into a PascalCase identifier, for `${CLASS_NAME}`.
///
/// `my_data_store.py` becomes `MyDataStore`. A stem that cannot produce a legal
/// identifier — starting with a digit, or empty after filtering — falls back to
/// a usable default rather than generating code that will not compile.
#[must_use]
pub fn to_pascal_case(stem: &str) -> String {
    let out: String = stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();

    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        "Main".to_owned()
    } else {
        out
    }
}

/// The stem of a filename, without its extension.
#[must_use]
pub fn stem(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in [
            "main.py",
            "lib.rs",
            "my_module.py",
            "index.html",
            ".gitignore",
            "file.with.dots.txt",
            "\u{e9}t\u{e9}.txt",
        ] {
            assert!(validate(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn empty_and_whitespace_names_are_rejected() {
        assert_eq!(validate(""), Err(NameError::Empty));
        assert_eq!(validate("   "), Err(NameError::Empty));
    }

    #[test]
    fn path_separators_are_rejected_so_the_dialog_cannot_write_outside_the_folder() {
        assert_eq!(
            validate("sub/file.py"),
            Err(NameError::IllegalCharacter('/'))
        );
        assert_eq!(
            validate("..\\escape.py"),
            Err(NameError::IllegalCharacter('\\'))
        );
    }

    #[test]
    fn every_illegal_character_is_caught() {
        for c in ILLEGAL {
            let name = format!("bad{c}name.txt");
            assert!(
                matches!(validate(&name), Err(NameError::IllegalCharacter(_))),
                "{c:?} should be rejected"
            );
        }
    }

    #[test]
    fn windows_device_names_are_rejected_with_and_without_an_extension() {
        for name in ["CON", "con", "NUL.txt", "com1.py", "LPT9.rs", "Aux"] {
            assert!(
                matches!(validate(name), Err(NameError::Reserved(_))),
                "{name} is a reserved device name"
            );
        }
        // ...but names that merely start with those letters are fine.
        assert!(validate("console.py").is_ok());
        assert!(validate("nullable.rs").is_ok());
    }

    #[test]
    fn trailing_dots_and_spaces_are_rejected_because_windows_strips_them() {
        assert_eq!(validate("report."), Err(NameError::TrailingSpaceOrDot));
        assert_eq!(validate("report "), Err(NameError::TrailingSpaceOrDot));
    }

    #[test]
    fn relative_path_components_are_rejected() {
        assert_eq!(validate("."), Err(NameError::Relative));
        assert_eq!(validate(".."), Err(NameError::Relative));
    }

    #[test]
    fn control_characters_are_rejected() {
        assert_eq!(validate("bad\nname.txt"), Err(NameError::ControlCharacter));
        assert_eq!(validate("bad\0name.txt"), Err(NameError::ControlCharacter));
    }

    #[test]
    fn overlong_names_are_rejected() {
        let name = "a".repeat(300);
        assert_eq!(validate(&name), Err(NameError::TooLong(MAX_LEN)));
    }

    #[test]
    fn pascal_case_handles_the_naming_styles_people_actually_use() {
        assert_eq!(to_pascal_case("my_data_store"), "MyDataStore");
        assert_eq!(to_pascal_case("my-data-store"), "MyDataStore");
        assert_eq!(to_pascal_case("main"), "Main");
        assert_eq!(to_pascal_case("HTTPServer"), "HTTPServer");
    }

    #[test]
    fn pascal_case_never_produces_an_illegal_identifier() {
        assert_eq!(
            to_pascal_case("2fast"),
            "Main",
            "an identifier cannot start with a digit"
        );
        assert_eq!(to_pascal_case("___"), "Main");
        assert_eq!(to_pascal_case(""), "Main");
    }

    #[test]
    fn stem_strips_the_extension() {
        assert_eq!(stem("main.py"), "main");
        assert_eq!(stem("archive.tar.gz"), "archive.tar");
        assert_eq!(stem("noext"), "noext");
    }
}
