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
//!
//! Asking the font is itself the hard part; see [`drawable`] for why the
//! obvious way of doing it is wrong, and what this does instead.
//!
//! Symbols that are *not* chosen through [`pick`] — because they are drawn
//! straight into a painter, or produced by a crate with no access to a `Ui` —
//! are named in [`crate::glyphs`], where a test checks every one of them.

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
        if drawable(ui, &font, candidate) {
            return candidate;
        }
    }
    ""
}

/// Whether `text` will be drawn as itself rather than as empty boxes.
///
/// The obvious API for this is `Fonts::has_glyphs`, and in epaint 0.36 it is
/// wrong in both directions: it reports plain `'a'` as absent from the
/// monospace family, and `'\u{26a0}'` as present there but absent from the
/// proportional one — the exact opposite of what the application draws. Every
/// icon in the toolbar consequently fell through to its ASCII fallback, which
/// is how a carefully chosen set of symbols became `Un`, `Re` and `SA`.
///
/// So this asks the one source that cannot disagree with the screen: what was
/// rasterised. A character the fonts do not have is drawn with the replacement
/// glyph, so it lands on the *same rectangle of the font atlas* as `U+FFFD`.
/// Two characters sharing a `uv_rect` are the same picture, and a symbol whose
/// picture is the replacement box is precisely what must be rejected.
pub fn drawable(ui: &egui::Ui, font: &egui::FontId, text: &str) -> bool {
    let tofu = atlas_rect(ui, font, char::REPLACEMENT_CHARACTER);
    text.chars().all(|c| {
        c == char::REPLACEMENT_CHARACTER
            || atlas_rect(ui, font, c).is_some_and(|rect| Some(rect) != tofu)
    })
}

/// Where in the font atlas `c` is drawn from, if it is drawn at all.
fn atlas_rect(
    ui: &egui::Ui,
    font: &egui::FontId,
    c: char,
) -> Option<impl PartialEq + Copy + use<>> {
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(c.to_string(), font.clone(), egui::Color32::WHITE));
    galley
        .rows
        .first()
        .and_then(|row| row.glyphs.first())
        .map(|glyph| glyph.uv_rect)
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
