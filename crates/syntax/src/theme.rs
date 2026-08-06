//! Syntax colours: a map from tree-sitter capture names to text styles.
//!
//! Capture names are hierarchical and dot-separated, e.g. `function.method`,
//! `punctuation.bracket`, `variable.parameter`. Lookup walks up that hierarchy,
//! so a theme that defines only `function` still colours `function.method`, and
//! a theme can override `function.builtin` specifically without restating
//! everything else. That is what makes a hand-written theme file short.
//!
//! No egui types here — this crate stays toolkit-free (PLAN.md §2.1), so
//! colours are plain RGB triples.

use std::collections::HashMap;

use editor_config::theme::ResolvedTheme;

/// How a run of text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub colour: Rgb,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    const fn plain(r: u8, g: u8, b: u8) -> Self {
        Self {
            colour: Rgb(r, g, b),
            bold: false,
            italic: false,
        }
    }

    const fn italic(r: u8, g: u8, b: u8) -> Self {
        Self {
            colour: Rgb(r, g, b),
            bold: false,
            italic: true,
        }
    }

    const fn bold(r: u8, g: u8, b: u8) -> Self {
        Self {
            colour: Rgb(r, g, b),
            bold: true,
            italic: false,
        }
    }
}

/// An 8-bit-per-channel colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A complete syntax colour scheme.
#[derive(Debug, Clone)]
pub struct SyntaxTheme {
    name: &'static str,
    /// Colour for text with no capture at all.
    default: Style,
    styles: HashMap<&'static str, Style>,
}

impl SyntaxTheme {
    /// The built-in theme matching a UI theme.
    #[must_use]
    pub fn for_ui(theme: ResolvedTheme) -> Self {
        match theme {
            ResolvedTheme::Dark => Self::dark(),
            ResolvedTheme::Light => Self::light(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Style for uncaptured text.
    #[must_use]
    pub fn default_style(&self) -> Style {
        self.default
    }

    /// Resolve a capture name, walking up the dotted hierarchy.
    ///
    /// `function.method.builtin` tries `function.method.builtin`, then
    /// `function.method`, then `function`, then falls back to the default.
    #[must_use]
    pub fn style_for(&self, capture: &str) -> Style {
        let mut key = capture;
        loop {
            if let Some(style) = self.styles.get(key) {
                return *style;
            }
            match key.rfind('.') {
                Some(dot) => key = &key[..dot],
                None => return self.default,
            }
        }
    }

    /// True if the theme has an entry for this capture or any of its parents.
    #[must_use]
    pub fn covers(&self, capture: &str) -> bool {
        let mut key = capture;
        loop {
            if self.styles.contains_key(key) {
                return true;
            }
            match key.rfind('.') {
                Some(dot) => key = &key[..dot],
                None => return false,
            }
        }
    }

    fn dark() -> Self {
        // Hues chosen to stay distinguishable under deuteranopia and
        // protanopia: the keyword/function/string triad differs in lightness
        // and blue-yellow as well as red-green.
        let styles = [
            ("keyword", Style::plain(0xc7, 0x9b, 0xf0)),
            ("keyword.operator", Style::plain(0xc7, 0x9b, 0xf0)),
            ("function", Style::plain(0x74, 0xb8, 0xff)),
            ("function.builtin", Style::plain(0x74, 0xb8, 0xff)),
            ("function.macro", Style::plain(0x6f, 0xd0, 0xd8)),
            ("constructor", Style::plain(0x6f, 0xd0, 0xd8)),
            ("type", Style::plain(0x6f, 0xd0, 0xd8)),
            ("type.builtin", Style::plain(0x6f, 0xd0, 0xd8)),
            ("string", Style::plain(0x9c, 0xd6, 0x7e)),
            ("string.escape", Style::plain(0xe8, 0xb3, 0x6a)),
            ("string.special", Style::plain(0xe8, 0xb3, 0x6a)),
            ("escape", Style::plain(0xe8, 0xb3, 0x6a)),
            ("number", Style::plain(0xe8, 0xb3, 0x6a)),
            ("constant", Style::plain(0xe8, 0xb3, 0x6a)),
            ("constant.builtin", Style::plain(0xe8, 0xb3, 0x6a)),
            ("comment", Style::italic(0x7d, 0x86, 0x94)),
            ("variable", Style::plain(0xd8, 0xd8, 0xdd)),
            ("variable.parameter", Style::plain(0xe0, 0xc0, 0x9a)),
            ("variable.builtin", Style::plain(0xe2, 0x84, 0x8c)),
            ("property", Style::plain(0x9a, 0xd0, 0xe8)),
            ("attribute", Style::plain(0xe0, 0xc0, 0x9a)),
            ("label", Style::plain(0xe0, 0xc0, 0x9a)),
            ("tag", Style::plain(0xe2, 0x84, 0x8c)),
            ("operator", Style::plain(0xb4, 0xbc, 0xc8)),
            ("punctuation", Style::plain(0x9a, 0xa2, 0xae)),
            ("punctuation.bracket", Style::plain(0x9a, 0xa2, 0xae)),
            ("punctuation.delimiter", Style::plain(0x9a, 0xa2, 0xae)),
            ("markup.heading", Style::bold(0x74, 0xb8, 0xff)),
            ("markup.link", Style::plain(0x9c, 0xd6, 0x7e)),
            ("markup.raw", Style::plain(0x9c, 0xd6, 0x7e)),
            ("error", Style::plain(0xf2, 0x6d, 0x6d)),
        ]
        .into_iter()
        .collect();

        Self {
            name: "Dark",
            default: Style::plain(0xd8, 0xd8, 0xdd),
            styles,
        }
    }

    fn light() -> Self {
        let styles = [
            ("keyword", Style::plain(0x7c, 0x3a, 0xa8)),
            ("keyword.operator", Style::plain(0x7c, 0x3a, 0xa8)),
            ("function", Style::plain(0x0a, 0x50, 0xa0)),
            ("function.builtin", Style::plain(0x0a, 0x50, 0xa0)),
            ("function.macro", Style::plain(0x0d, 0x66, 0x6e)),
            ("constructor", Style::plain(0x0d, 0x66, 0x6e)),
            ("type", Style::plain(0x0d, 0x66, 0x6e)),
            ("type.builtin", Style::plain(0x0d, 0x66, 0x6e)),
            ("string", Style::plain(0x1d, 0x6b, 0x24)),
            ("string.escape", Style::plain(0x9a, 0x4e, 0x06)),
            ("string.special", Style::plain(0x9a, 0x4e, 0x06)),
            ("escape", Style::plain(0x9a, 0x4e, 0x06)),
            ("number", Style::plain(0x9a, 0x4e, 0x06)),
            ("constant", Style::plain(0x9a, 0x4e, 0x06)),
            ("constant.builtin", Style::plain(0x9a, 0x4e, 0x06)),
            ("comment", Style::italic(0x5c, 0x66, 0x72)),
            ("variable", Style::plain(0x1c, 0x1c, 0x20)),
            ("variable.parameter", Style::plain(0x7a, 0x4c, 0x14)),
            ("variable.builtin", Style::plain(0xa8, 0x25, 0x25)),
            ("property", Style::plain(0x0c, 0x53, 0x66)),
            ("attribute", Style::plain(0x7a, 0x4c, 0x14)),
            ("label", Style::plain(0x7a, 0x4c, 0x14)),
            ("tag", Style::plain(0xa8, 0x25, 0x25)),
            ("operator", Style::plain(0x3d, 0x45, 0x50)),
            ("punctuation", Style::plain(0x4d, 0x55, 0x60)),
            ("punctuation.bracket", Style::plain(0x4d, 0x55, 0x60)),
            ("punctuation.delimiter", Style::plain(0x4d, 0x55, 0x60)),
            ("markup.heading", Style::bold(0x0a, 0x50, 0xa0)),
            ("markup.link", Style::plain(0x1d, 0x6b, 0x24)),
            ("markup.raw", Style::plain(0x1d, 0x6b, 0x24)),
            ("error", Style::plain(0xb3, 0x1d, 0x1d)),
        ]
        .into_iter()
        .collect();

        Self {
            name: "Light",
            default: Style::plain(0x1c, 0x1c, 0x20),
            styles,
        }
    }
}

/// WCAG relative luminance.
fn luminance(c: Rgb) -> f32 {
    fn channel(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

/// WCAG contrast ratio between two colours.
#[must_use]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Must match `editor-widgets::theme`'s `extreme_bg_color`, which is what
    /// the code pane is painted on.
    const DARK_BG: Rgb = Rgb(0x18, 0x18, 0x1c);
    const LIGHT_BG: Rgb = Rgb(0xff, 0xff, 0xff);

    #[test]
    fn capture_lookup_walks_up_the_dotted_hierarchy() {
        let theme = SyntaxTheme::dark();
        // `function.method` is not defined, so it inherits from `function`.
        assert_eq!(
            theme.style_for("function.method"),
            theme.style_for("function")
        );
        assert_eq!(
            theme.style_for("function.method.static"),
            theme.style_for("function")
        );
    }

    #[test]
    fn a_more_specific_entry_wins_over_its_parent() {
        let theme = SyntaxTheme::dark();
        assert_ne!(
            theme.style_for("variable.parameter"),
            theme.style_for("variable"),
            "parameters are deliberately distinct from ordinary variables"
        );
    }

    #[test]
    fn an_unknown_capture_falls_back_to_the_default_style() {
        let theme = SyntaxTheme::dark();
        assert_eq!(theme.style_for("nonsense.capture"), theme.default_style());
        assert!(!theme.covers("nonsense.capture"));
    }

    /// PLAN.md §3.11: syntax colours must reach WCAG AA against the code pane
    /// background in both themes. A hand-edited colour that breaks this fails
    /// here rather than shipping as unreadable text.
    #[test]
    fn every_syntax_colour_meets_wcag_aa_against_the_code_background() {
        for (theme, bg, label) in [
            (SyntaxTheme::dark(), DARK_BG, "dark"),
            (SyntaxTheme::light(), LIGHT_BG, "light"),
        ] {
            let mut all: Vec<(&str, Style)> = theme.styles.iter().map(|(k, v)| (*k, *v)).collect();
            all.push(("<default>", theme.default));

            for (capture, style) in all {
                let ratio = contrast_ratio(style.colour, bg);
                assert!(
                    ratio >= 4.5,
                    "{label}: {capture} is {ratio:.2}:1 against the editor background, \
                     below the 4.5:1 AA threshold"
                );
            }
        }
    }

    #[test]
    fn comments_are_dimmer_than_code_but_still_readable() {
        for (theme, bg) in [
            (SyntaxTheme::dark(), DARK_BG),
            (SyntaxTheme::light(), LIGHT_BG),
        ] {
            let comment = theme.style_for("comment");
            let default = theme.default_style();
            assert!(
                contrast_ratio(comment.colour, bg) < contrast_ratio(default.colour, bg),
                "comments should recede relative to code"
            );
            assert!(
                contrast_ratio(comment.colour, bg) >= 4.5,
                "...but not to the point of being unreadable"
            );
            assert!(comment.italic, "comments are italic in both themes");
        }
    }

    #[test]
    fn the_two_themes_define_exactly_the_same_captures() {
        let dark = SyntaxTheme::dark();
        let light = SyntaxTheme::light();
        let mut d: Vec<&str> = dark.styles.keys().copied().collect();
        let mut l: Vec<&str> = light.styles.keys().copied().collect();
        d.sort_unstable();
        l.sort_unstable();
        assert_eq!(
            d, l,
            "a capture styled in one theme but not the other renders inconsistently"
        );
    }

    #[test]
    fn the_main_token_kinds_are_visually_distinct_from_each_other() {
        // If keywords, strings and comments share a colour, highlighting is
        // decorative rather than useful.
        for theme in [SyntaxTheme::dark(), SyntaxTheme::light()] {
            let keyword = theme.style_for("keyword").colour;
            let string = theme.style_for("string").colour;
            let comment = theme.style_for("comment").colour;
            let function = theme.style_for("function").colour;

            for (a, b, names) in [
                (keyword, string, "keyword/string"),
                (keyword, comment, "keyword/comment"),
                (string, comment, "string/comment"),
                (keyword, function, "keyword/function"),
            ] {
                assert_ne!(a, b, "{names} share a colour");
            }
        }
    }

    #[test]
    fn themes_follow_the_ui_theme() {
        assert_eq!(SyntaxTheme::for_ui(ResolvedTheme::Dark).name(), "Dark");
        assert_eq!(SyntaxTheme::for_ui(ResolvedTheme::Light).name(), "Light");
    }
}
