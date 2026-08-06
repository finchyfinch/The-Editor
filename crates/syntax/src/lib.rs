//! Language registry and tree-sitter driven syntax services.
//!
//! Provides highlighting, bracket matching, fold ranges and the indentation
//! hints consumed by `editor-core::indent`. Highlighting is computed for the
//! visible viewport only; parse trees are updated incrementally via
//! `Tree::edit` on every transaction. See PLAN.md §3.5.

pub mod templates;

// M3 populates these.
//
// pub mod language;    // LanguageId, detection by extension/shebang/override
// pub mod registry;    // grammar loading, injections (HTML -> JS/CSS)
// pub mod highlight;   // viewport highlighting, capture -> style mapping
// pub mod theme;       // TOML theme format
// pub mod folds;
// pub mod brackets;

/// Languages The Editor recognises in 1.0.
///
/// Placeholder until M3 replaces it with the real registry — it exists now so
/// the file-tree and tab-bar icons in M1 have something to key off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Python,
    Rust,
    Json,
    JavaScript,
    Html,
    Css,
    Ini,
    Toml,
    Markdown,
    PlainText,
}

impl LanguageId {
    /// Best-effort detection from a file extension. Case-insensitive.
    ///
    /// Returns [`LanguageId::PlainText`] for anything unrecognised, which is
    /// always a safe fallback: an unknown file opens as editable plain text
    /// rather than failing.
    #[must_use]
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "py" | "pyw" | "pyi" => Self::Python,
            "rs" => Self::Rust,
            "json" | "jsonc" => Self::Json,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "html" | "htm" => Self::Html,
            "css" => Self::Css,
            "ini" | "cfg" | "conf" => Self::Ini,
            "toml" => Self::Toml,
            "md" | "markdown" => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    /// Every language, in the order the New File dialog lists them: the two
    /// this IDE is built for first, then the rest.
    pub const ALL: [Self; 10] = [
        Self::Python,
        Self::Rust,
        Self::Json,
        Self::JavaScript,
        Self::Html,
        Self::Css,
        Self::Ini,
        Self::Toml,
        Self::Markdown,
        Self::PlainText,
    ];

    /// Extension a new file of this language gets, without the dot.
    #[must_use]
    pub fn default_extension(self) -> &'static str {
        match self {
            Self::Python => "py",
            Self::Rust => "rs",
            Self::Json => "json",
            Self::JavaScript => "js",
            Self::Html => "html",
            Self::Css => "css",
            Self::Ini => "ini",
            Self::Toml => "toml",
            Self::Markdown => "md",
            Self::PlainText => "txt",
        }
    }

    /// Human-readable name, for the status bar and the New File dialog.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::Rust => "Rust",
            Self::Json => "JSON",
            Self::JavaScript => "JavaScript",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Ini => "INI",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::PlainText => "Plain Text",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_languages_the_editor_ships_with() {
        assert_eq!(LanguageId::from_extension("py"), LanguageId::Python);
        assert_eq!(LanguageId::from_extension("PY"), LanguageId::Python);
        assert_eq!(LanguageId::from_extension("rs"), LanguageId::Rust);
        assert_eq!(LanguageId::from_extension("htm"), LanguageId::Html);
    }

    #[test]
    fn unknown_extensions_open_as_plain_text_rather_than_failing() {
        assert_eq!(LanguageId::from_extension("xyzzy"), LanguageId::PlainText);
        assert_eq!(LanguageId::from_extension(""), LanguageId::PlainText);
    }

    #[test]
    fn every_default_extension_maps_back_to_its_own_language() {
        for language in LanguageId::ALL {
            assert_eq!(
                LanguageId::from_extension(language.default_extension()),
                language,
                "{language:?} does not round-trip through its default extension"
            );
        }
    }

    #[test]
    fn python_and_rust_lead_the_language_list() {
        assert_eq!(LanguageId::ALL.first(), Some(&LanguageId::Python));
        assert_eq!(LanguageId::ALL.get(1), Some(&LanguageId::Rust));
    }
}
