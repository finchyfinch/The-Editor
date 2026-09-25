//! Columns as the protocol counts them, and as the editor does.
//!
//! The editor counts a column in characters. The protocol, unless a server
//! agrees otherwise, counts it in UTF-16 code units — so on a line holding an
//! emoji, or any character outside the Basic Multilingual Plane, the two
//! disagree by one for every such character before the position. That was
//! enough to put a squiggle one character to the right of the mistake, and to
//! make a rename replace the wrong characters of a line.
//!
//! Every position crossing the boundary goes through [`to_protocol`] or
//! [`from_protocol`], with the line it is on, so there is one place to get this
//! right. The client offers UTF-32 first, which is the editor's own count and
//! needs no conversion at all; servers that do not accept it get UTF-16.

/// How a server counts columns, as agreed in the initialize handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    Utf8,
    /// The protocol's default, and so what a server that says nothing means.
    #[default]
    Utf16,
    /// One per character, which is how the editor counts.
    Utf32,
}

impl Encoding {
    /// Read the `positionEncoding` a server chose. Anything unrecognised is the
    /// protocol default.
    #[must_use]
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    /// The encodings the client can work in, most preferred first.
    #[must_use]
    pub fn offered() -> [&'static str; 3] {
        ["utf-32", "utf-16", "utf-8"]
    }

    fn width(self, c: char) -> u32 {
        match self {
            Self::Utf8 => c.len_utf8() as u32,
            Self::Utf16 => c.len_utf16() as u32,
            Self::Utf32 => 1,
        }
    }
}

/// The protocol column for character column `column` of `line`.
///
/// `line` is the line's text; a line terminator on it is ignored. A column past
/// the end counts the missing characters as one unit each, so the answer still
/// grows with the input rather than sticking.
#[must_use]
pub fn to_protocol(line: &str, column: u32, encoding: Encoding) -> u32 {
    if encoding == Encoding::Utf32 {
        return column;
    }
    let mut units = 0;
    let mut chars = 0;
    for c in line.chars() {
        if chars == column || c == '\n' || c == '\r' {
            break;
        }
        units += encoding.width(c);
        chars += 1;
    }
    units + column.saturating_sub(chars)
}

/// The character column for protocol column `column` of `line`.
///
/// A column that lands inside a character — half of a surrogate pair — is
/// rounded up past it, and one past the end of the line is clamped to it, the
/// same leniency the protocol asks clients to show.
#[must_use]
pub fn from_protocol(line: &str, column: u32, encoding: Encoding) -> u32 {
    if encoding == Encoding::Utf32 {
        return column;
    }
    let mut units = 0;
    let mut chars = 0;
    for c in line.chars() {
        if units >= column || c == '\n' || c == '\r' {
            break;
        }
        units += encoding.width(c);
        chars += 1;
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMOJI_LINE: &str = "print(\"\u{1F600}\", total)\n";

    #[test]
    fn plain_ascii_is_the_same_in_every_encoding() {
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            assert_eq!(to_protocol("abc def", 4, encoding), 4);
            assert_eq!(from_protocol("abc def", 4, encoding), 4);
        }
    }

    /// The case that went wrong: an emoji is one character and two UTF-16 units,
    /// so `total` is at character 11 and at UTF-16 column 12.
    #[test]
    fn an_emoji_before_the_position_counts_twice_in_utf16() {
        assert_eq!(to_protocol(EMOJI_LINE, 11, Encoding::Utf16), 12);
        assert_eq!(from_protocol(EMOJI_LINE, 12, Encoding::Utf16), 11);
    }

    #[test]
    fn utf8_counts_bytes() {
        // "é" is two bytes.
        assert_eq!(to_protocol("caf\u{e9} x", 5, Encoding::Utf8), 6);
        assert_eq!(from_protocol("caf\u{e9} x", 6, Encoding::Utf8), 5);
    }

    #[test]
    fn utf32_is_the_editors_own_count() {
        assert_eq!(to_protocol(EMOJI_LINE, 11, Encoding::Utf32), 11);
        assert_eq!(from_protocol(EMOJI_LINE, 11, Encoding::Utf32), 11);
    }

    #[test]
    fn every_column_round_trips() {
        let line = "a\u{1F600}b\u{e9}\u{20ac}c";
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for column in 0..=6 {
                let there = to_protocol(line, column, encoding);
                assert_eq!(
                    from_protocol(line, there, encoding),
                    column,
                    "{encoding:?} {column}"
                );
            }
        }
    }

    #[test]
    fn half_a_surrogate_pair_rounds_past_the_character() {
        // UTF-16 column 1 is inside the emoji.
        assert_eq!(from_protocol("\u{1F600}x", 1, Encoding::Utf16), 1);
    }

    #[test]
    fn past_the_end_is_clamped_coming_in_and_extended_going_out() {
        assert_eq!(from_protocol("ab\n", 9, Encoding::Utf16), 2);
        assert_eq!(to_protocol("ab\n", 4, Encoding::Utf16), 4);
    }

    #[test]
    fn a_server_that_names_nothing_is_using_utf16() {
        assert_eq!(Encoding::from_name(None), Encoding::Utf16);
        assert_eq!(Encoding::from_name(Some("utf-32")), Encoding::Utf32);
        assert_eq!(Encoding::from_name(Some("something-new")), Encoding::Utf16);
    }
}
