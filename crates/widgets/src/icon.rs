//! Choosing a glyph the loaded fonts can actually draw.
//!
//! egui bundles a small set of fonts, and a character they do not cover is
//! drawn as an empty box. Which characters those are is not obvious from the
//! codepoint: `▶` and `■` render, `↑` and `↓` do not, even though all four look
//! like the sort of thing any font would have.
//!
//! Picking a symbol by eye and hoping therefore does not work, and the failure
//! is silent — a toolbar of identical boxes, which is exactly what happened
//! here. Every glyph in the interface is chosen through [`pick`], which asks
//! the font what it has and falls back until something can be drawn. The last
//! candidate should always be plain ASCII, which nothing can fail to render.

use eframe::egui;

/// The first candidate the fonts can draw.
///
/// The last is returned whether or not it is drawable, so it must be something
/// no font can miss. Checked against the button text style, since that is what
/// these are drawn in.
pub fn pick<'a>(ui: &egui::Ui, candidates: &[&'a str]) -> &'a str {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let last = candidates.len().saturating_sub(1);
    for (i, candidate) in candidates.iter().enumerate() {
        if i == last {
            return candidate;
        }
        if ui.fonts_mut(|f| f.has_glyphs(&font, candidate)) {
            return candidate;
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    /// The selection rule, separated from the font lookup so it can be tested
    /// without standing up a rendering context.
    fn choose<'a>(candidates: &[&'a str], drawable: impl Fn(&str) -> bool) -> &'a str {
        let last = candidates.len().saturating_sub(1);
        for (i, candidate) in candidates.iter().enumerate() {
            if i == last || drawable(candidate) {
                return candidate;
            }
        }
        ""
    }

    #[test]
    fn the_first_drawable_candidate_wins() {
        assert_eq!(choose(&["a", "b", "c"], |_| true), "a");
    }

    #[test]
    fn an_undrawable_candidate_is_skipped() {
        assert_eq!(choose(&["a", "b", "c"], |s| s != "a"), "b");
    }

    #[test]
    fn the_last_candidate_is_used_even_when_it_cannot_be_drawn() {
        // It is the fallback of last resort; returning nothing would leave an
        // invisible button, which is worse than a box.
        assert_eq!(choose(&["a", "b", "z"], |_| false), "z");
    }

    #[test]
    fn a_single_candidate_is_always_returned() {
        assert_eq!(choose(&["only"], |_| false), "only");
    }

    #[test]
    fn no_candidates_yields_nothing_rather_than_panicking() {
        assert_eq!(choose(&[], |_| true), "");
    }
}
