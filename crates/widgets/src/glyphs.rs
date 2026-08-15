//! The symbols the interface draws, and a guarantee that it can draw them.
//!
//! egui bundles a small set of fonts. A character they do not cover is not an
//! error, a warning, or anything visible in a diff — it is a hollow rectangle
//! in the running application, and the only way to notice is to look at it.
//! Several shipped that way: the error glyph in the status bar, the theme
//! indicator beside it, the dirty-tab dot, and the chevron beside every folder
//! in the explorer.
//!
//! Symbols drawn through [`crate::icon::pick`] look after themselves — it
//! falls back until it finds one that works. These are the ones that cannot:
//! painted straight into a painter, or produced by a crate with no `Ui` to ask.
//! Each is named here and checked by the test below.
//!
//! Which family matters. The proportional fonts are Ubuntu-Light plus two
//! emoji fonts; monospace adds Hack in front of those, and Hack has the
//! geometric shapes the others lack. So `\u{25b8}` is fine in the editor's fold
//! column and an empty box in a label — which is exactly what happened.

/// Font family a symbol is drawn in, because coverage differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Labels, buttons, menus — everything laid out by egui's widgets.
    Proportional,
    /// The code editor and anything painted alongside it.
    Monospace,
}

/// Nothing is wrong. Not `\u{2713}`, which is not in the fonts.
pub const OK: &str = "\u{2714}";
/// The theme indicator in the status bar. Not `\u{25d0}`, likewise.
pub const THEME: &str = "\u{25d1}";
/// A tab with unsaved changes. Not `\u{25cf}` or `\u{2b24}`, likewise.
pub const DIRTY: &str = "\u{26ab}";
/// The close button on a tab.
pub const CLOSE: &str = "\u{00d7}";
/// An expanded and a collapsed row in the explorer, and the find bar's
/// replace toggle.
///
/// From the media-control block rather than the geometric-shapes one, which is
/// where the obvious `\u{25b8}` and `\u{25be}` live and where the proportional
/// fonts stop. These two are in both families.
pub const TREE_OPEN: &str = "\u{23f7}";
pub const TREE_CLOSED: &str = "\u{23f5}";
/// The editor's fold column, painted in the code font.
///
/// Deliberately lighter than [`TREE_OPEN`]: this one sits beside every foldable
/// line of code rather than beside a handful of folders, and the monospace font
/// has the smaller triangles that the proportional one lacks.
pub const FOLD_OPEN: &str = "\u{25be}";
pub const FOLD_CLOSED: &str = "\u{25b8}";

#[cfg(test)]
mod tests {
    use super::Family;
    use eframe::egui;

    /// Every symbol drawn outside [`crate::icon::pick`], and where.
    ///
    /// Listed by hand rather than scraped from the source: the point is to
    /// state what we mean to draw, so that adding a symbol without adding it
    /// here is the only way to get an unchecked one — and adding it here is
    /// the obvious thing to do.
    const IN_USE: &[(&str, &str, Family)] = &[
        ("OK", super::OK, Family::Proportional),
        ("THEME", super::THEME, Family::Proportional),
        ("DIRTY", super::DIRTY, Family::Proportional),
        ("CLOSE", super::CLOSE, Family::Proportional),
        ("TREE_OPEN", super::TREE_OPEN, Family::Proportional),
        ("TREE_CLOSED", super::TREE_CLOSED, Family::Proportional),
        ("FOLD_OPEN", super::FOLD_OPEN, Family::Monospace),
        ("FOLD_CLOSED", super::FOLD_CLOSED, Family::Monospace),
        // `editor_lsp::diagnostics::Severity::glyph`, drawn in the status bar,
        // the Problems panel and the editor's gutter.
        ("severity: error", "\u{2716}", Family::Proportional),
        ("severity: warning", "\u{26a0}", Family::Proportional),
        ("severity: information", "\u{2139}", Family::Proportional),
        ("severity: hint", "\u{25aa}", Family::Proportional),
        (
            "severity glyphs in the gutter",
            "\u{2716}\u{26a0}",
            Family::Monospace,
        ),
        // `editor_lsp::session::Completion::glyph`, drawn in the popup.
        ("completion: function", "\u{192}", Family::Proportional),
        ("completion: field", "\u{25ab}", Family::Proportional),
        ("completion: variable", "\u{25aa}", Family::Proportional),
        ("completion: class", "\u{25ce}", Family::Proportional),
        ("completion: interface", "\u{25cb}", Family::Proportional),
        ("completion: module", "\u{25a0}", Family::Proportional),
        ("completion: keyword", "\u{203a}", Family::Proportional),
        ("completion: constant", "\u{2022}", Family::Proportional),
        ("completion: anything else", "\u{b7}", Family::Proportional),
        // Punctuation used inline in labels, menu items and the console.
        ("ellipsis", "\u{2026}", Family::Proportional),
        ("em dash", "\u{2014}", Family::Proportional),
        ("bullet", "\u{2022}", Family::Proportional),
        ("prompt", "\u{203a}", Family::Proportional),
        ("folder", "\u{1f4c1}", Family::Proportional),
        ("file", "\u{1f4c4}", Family::Proportional),
        ("warning in the explorer", "\u{26a0}", Family::Proportional),
    ];

    /// Ask the fonts which of `wanted` they cannot draw.
    ///
    /// The obvious API for this is `Fonts::has_glyph`, and in epaint 0.36 it is
    /// wrong in both directions — see `crate::icon::drawable`, which is the
    /// same technique this uses and the one the running application relies on.
    /// Checking here with a *different* method to the one under test would only
    /// prove the two agree.
    fn undrawable(wanted: &[(&str, &str, Family)]) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());

        let atlas_rect = |font: &egui::FontId, c: char| {
            let galley = ctx
                .fonts_mut(|f| f.layout_no_wrap(c.to_string(), font.clone(), egui::Color32::WHITE));
            galley
                .rows
                .first()
                .and_then(|row| row.glyphs.first())
                .map(|glyph| glyph.uv_rect)
        };

        let mut bad = Vec::new();
        for (name, text, family) in wanted {
            let font = match family {
                Family::Proportional => egui::FontId::proportional(13.0),
                Family::Monospace => egui::FontId::monospace(13.0),
            };
            let tofu = atlas_rect(&font, char::REPLACEMENT_CHARACTER);
            for c in text.chars() {
                let rect = atlas_rect(&font, c);
                if rect.is_none() || rect == tofu {
                    bad.push(format!("{name}: {c:?} (U+{:04X}) in {family:?}", c as u32));
                }
            }
        }

        // The pass produced a font atlas upload, and epaint panics on a dropped
        // delta nobody applied. Nothing here is rendered, so discarding it is
        // right — it just has to be said out loud.
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        bad
    }

    /// Every symbol the interface draws must exist in the fonts that ship with
    /// it. One that does not is a hollow rectangle: it says nothing, and it
    /// looks like a bug because it is one.
    #[test]
    fn every_symbol_the_interface_draws_can_be_drawn() {
        let bad = undrawable(IN_USE);
        assert!(
            bad.is_empty(),
            "the bundled fonts cannot draw these, so they appear as empty boxes:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The characters that actually shipped as empty boxes, kept so the test
    /// above is demonstrably able to fail rather than merely passing.
    ///
    /// Every one of these was picked by eye as the obvious symbol for the job,
    /// which is the whole reason picking by eye does not work.
    #[test]
    fn the_symbols_that_shipped_as_empty_boxes_really_are_missing() {
        let culprits: &[(&str, &str, Family)] = &[
            ("tick", "\u{2713}", Family::Proportional),
            ("cross", "\u{2717}", Family::Proportional),
            ("half-filled circle", "\u{25d0}", Family::Proportional),
            ("filled circle", "\u{25cf}", Family::Proportional),
            ("small triangle", "\u{25b8}", Family::Proportional),
            ("up arrow", "\u{2191}", Family::Proportional),
            ("undo arrow", "\u{21b6}", Family::Proportional),
            ("card index", "\u{1f5c3}", Family::Proportional),
            // Guesses this very test rejected while it was being written,
            // which is the best evidence it is worth having.
            ("large filled circle", "\u{2b24}", Family::Proportional),
            ("white down triangle", "\u{25bf}", Family::Proportional),
        ];
        let bad = undrawable(culprits);
        assert_eq!(
            bad.len(),
            culprits.len(),
            "these are drawable after all, so replacing them was unnecessary: {bad:?}"
        );
    }
}
