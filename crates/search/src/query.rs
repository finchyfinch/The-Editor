//! Search queries and in-file matching.
//!
//! Everything is built on the `regex` crate, including literal searches: a
//! literal is just an escaped pattern, so case-insensitivity, whole-word
//! matching and Unicode handling behave identically whichever mode the user is
//! in. Two code paths would drift.
//!
//! Offsets in and out are **character** indices, matching `editor-core`. The
//! byte offsets `regex` works in never escape this module.

use std::ops::Range;

use regex::{Regex, RegexBuilder};
use ropey::Rope;

/// What to search for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Treat the pattern as a regular expression rather than literal text.
    pub regex: bool,
}

impl Query {
    /// A plain literal search, for tests and simple callers.
    #[must_use]
    pub fn literal(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_owned(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_empty()
    }
}

/// A pattern that could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError(pub String);

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for QueryError {}

/// A compiled query, ready to run against text.
#[derive(Debug, Clone)]
pub struct Matcher {
    regex: Regex,
    /// True when the query was a regular expression, which is the only case
    /// where `$1` and friends mean anything in a replacement.
    captures_enabled: bool,
}

impl Matcher {
    /// Compile a query.
    ///
    /// # Errors
    /// If the pattern is an invalid regular expression. A literal query can
    /// only fail if it is pathologically long, since the pattern is escaped.
    pub fn new(query: &Query) -> Result<Self, QueryError> {
        let pattern = if query.regex {
            query.pattern.clone()
        } else {
            regex::escape(&query.pattern)
        };

        // `\b` around a pattern starting or ending with a non-word character
        // can never match, which would look like a broken search rather than a
        // meaningless option; skip the wrapping in that case.
        let pattern = if query.whole_word && word_boundaries_meaningful(&pattern) {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };

        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!query.case_sensitive)
            // Multi-line so `^` and `$` mean line starts and ends, which is
            // what someone typing `^import` expects.
            .multi_line(true)
            .build()
            .map_err(|e| QueryError(first_line(&e.to_string())))?;

        Ok(Self {
            regex,
            captures_enabled: query.regex,
        })
    }

    /// Every match in `text`, as character ranges, in document order.
    ///
    /// Empty matches are skipped: a pattern like `a*` matches at every position
    /// and would otherwise produce one "match" per character, which is useless
    /// to step through and dangerous to Replace All.
    #[must_use]
    pub fn find_all(&self, text: &Rope) -> Vec<Range<usize>> {
        let haystack = text.to_string();
        self.regex
            .find_iter(&haystack)
            .filter(|m| m.start() != m.end())
            .map(|m| byte_to_char(text, m.start())..byte_to_char(text, m.end()))
            .collect()
    }

    /// The number of matches, without building the list.
    #[must_use]
    pub fn count(&self, text: &Rope) -> usize {
        self.find_all(text).len()
    }

    /// The replacement text for the match at `range`.
    ///
    /// In regex mode, `$1` and `${name}` expand to captures; in literal mode
    /// the template is inserted verbatim, so searching for a price and
    /// replacing it with `$5` does not silently expand to a capture group.
    #[must_use]
    pub fn replacement(&self, text: &Rope, range: &Range<usize>, template: &str) -> String {
        if !self.captures_enabled {
            return template.to_owned();
        }
        let matched = text.slice(range.clone()).to_string();
        match self.regex.captures(&matched) {
            Some(caps) => {
                let mut out = String::new();
                caps.expand(template, &mut out);
                out
            }
            None => template.to_owned(),
        }
    }

    /// Index of the first match at or after `offset`, wrapping to the start.
    #[must_use]
    pub fn next_from(matches: &[Range<usize>], offset: usize) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }
        Some(matches.iter().position(|m| m.start >= offset).unwrap_or(0))
    }

    /// Index of the last match starting before `offset`, wrapping to the end.
    #[must_use]
    pub fn previous_from(matches: &[Range<usize>], offset: usize) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }
        Some(
            matches
                .iter()
                .rposition(|m| m.start < offset)
                .unwrap_or(matches.len() - 1),
        )
    }
}

/// `\b` only means something next to a word character.
fn word_boundaries_meaningful(pattern: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    pattern.chars().next().is_some_and(is_word) && pattern.chars().next_back().is_some_and(is_word)
}

fn byte_to_char(text: &Rope, byte: usize) -> usize {
    text.byte_to_char(byte.min(text.len_bytes()))
}

/// Regex errors are multi-line and shaped for a terminal; the bar has one line.
fn first_line(message: &str) -> String {
    message
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("invalid pattern")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_of(source: &str, query: &Query) -> Vec<String> {
        let rope = Rope::from_str(source);
        let matcher = Matcher::new(query).expect("query compiles");
        matcher
            .find_all(&rope)
            .into_iter()
            .map(|r| rope.slice(r).to_string())
            .collect()
    }

    #[test]
    fn a_literal_query_finds_every_occurrence() {
        assert_eq!(
            matches_of("one two one three one", &Query::literal("one")),
            ["one", "one", "one"]
        );
    }

    #[test]
    fn literal_queries_are_case_insensitive_by_default() {
        assert_eq!(
            matches_of("One ONE one", &Query::literal("one")).len(),
            3,
            "search should be forgiving until asked not to be"
        );

        let sensitive = Query {
            case_sensitive: true,
            ..Query::literal("one")
        };
        assert_eq!(matches_of("One ONE one", &sensitive), ["one"]);
    }

    #[test]
    fn regex_metacharacters_are_literal_unless_regex_mode_is_on() {
        // Searching for "a.c" must not match "abc".
        assert!(matches_of("abc a.c", &Query::literal("a.c")) == ["a.c"]);

        let regex = Query {
            regex: true,
            ..Query::literal("a.c")
        };
        assert_eq!(matches_of("abc a.c", &regex).len(), 2);
    }

    #[test]
    fn whole_word_matching_does_not_match_inside_a_word() {
        let query = Query {
            whole_word: true,
            ..Query::literal("cat")
        };
        assert_eq!(
            matches_of("cat concatenate cats cat.", &query),
            ["cat", "cat"]
        );
    }

    #[test]
    fn whole_word_is_ignored_when_it_could_never_match() {
        // `\b(?:...)\b` around a pattern of punctuation matches nothing at all,
        // which would look like a broken search rather than a no-op option.
        let query = Query {
            whole_word: true,
            ..Query::literal("=>")
        };
        assert_eq!(matches_of("a => b => c", &query).len(), 2);
    }

    #[test]
    fn an_invalid_regex_is_reported_rather_than_panicking() {
        let query = Query {
            regex: true,
            ..Query::literal("(unclosed")
        };
        let error = Matcher::new(&query).expect_err("should not compile");
        assert!(!error.0.is_empty());
        assert!(
            !error.0.contains('\n'),
            "the message goes in a one-line bar: {:?}",
            error.0
        );
    }

    #[test]
    fn empty_matches_are_skipped() {
        // `a*` matches at every position; stepping through those is useless and
        // replacing them would insert text between every character.
        let query = Query {
            regex: true,
            ..Query::literal("a*")
        };
        assert_eq!(matches_of("baaab", &query), ["aaa"]);
    }

    #[test]
    fn anchors_work_per_line() {
        let query = Query {
            regex: true,
            ..Query::literal("^import")
        };
        let source = "import os\nx = 1\nimport sys\n# import re\n";
        assert_eq!(matches_of(source, &query).len(), 2);
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        let source = "caf\u{e9} \u{1f600} target";
        let rope = Rope::from_str(source);
        let matcher = Matcher::new(&Query::literal("target")).expect("compiles");
        let found = matcher.find_all(&rope);

        assert_eq!(found.len(), 1);
        assert_eq!(
            rope.slice(found[0].clone()).to_string(),
            "target",
            "a byte offset here would slice mid-character"
        );
        assert_eq!(found[0].start, source.chars().count() - 6);
    }

    #[test]
    fn replacement_is_verbatim_for_a_literal_query() {
        let rope = Rope::from_str("cost 100");
        let matcher = Matcher::new(&Query::literal("100")).expect("compiles");
        let found = matcher.find_all(&rope);
        assert_eq!(
            matcher.replacement(&rope, &found[0], "$5"),
            "$5",
            "a literal replacement must not expand capture references"
        );
    }

    #[test]
    fn replacement_expands_captures_in_regex_mode() {
        let query = Query {
            regex: true,
            ..Query::literal(r"(\w+)@(\w+)")
        };
        let rope = Rope::from_str("write to user@example today");
        let matcher = Matcher::new(&query).expect("compiles");
        let found = matcher.find_all(&rope);
        assert_eq!(
            matcher.replacement(&rope, &found[0], "$2 dot $1"),
            "example dot user"
        );
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let matches = vec![10..13, 40..43, 70..73];

        assert_eq!(Matcher::next_from(&matches, 0), Some(0));
        assert_eq!(Matcher::next_from(&matches, 11), Some(1));
        assert_eq!(
            Matcher::next_from(&matches, 99),
            Some(0),
            "past the last match, wrap to the first"
        );

        assert_eq!(Matcher::previous_from(&matches, 50), Some(1));
        assert_eq!(
            Matcher::previous_from(&matches, 0),
            Some(2),
            "before the first match, wrap to the last"
        );
    }

    #[test]
    fn stepping_through_an_empty_result_set_yields_nothing() {
        assert_eq!(Matcher::next_from(&[], 0), None);
        assert_eq!(Matcher::previous_from(&[], 0), None);
    }

    #[test]
    fn searching_an_empty_document_finds_nothing() {
        let rope = Rope::new();
        let matcher = Matcher::new(&Query::literal("anything")).expect("compiles");
        assert!(matcher.find_all(&rope).is_empty());
        assert_eq!(matcher.count(&rope), 0);
    }

    #[test]
    fn overlapping_candidates_do_not_produce_overlapping_matches() {
        // "aaaa" contains "aa" twice without overlap, not three times.
        let found = matches_of("aaaa", &Query::literal("aa"));
        assert_eq!(found.len(), 2);
    }
}
