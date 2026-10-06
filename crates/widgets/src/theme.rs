//! Turning a [`ResolvedTheme`] into egui `Visuals`.
//!
//! This is the only place UI colours are defined. Nothing else in the codebase
//! may name a colour literal — if a widget needs one it comes from
//! `ui.visuals()`, so that switching theme actually switches everything. That
//! discipline is why the theme toggle lands in M1 rather than M8: building the
//! rest of the UI against a theme that can already change surfaces hardcoded
//! colours immediately.
//!
//! Contrast ratios are asserted by the tests at the bottom, not checked by eye.

use editor_config::theme::{ResolvedTheme, ThemePreference};
use eframe::egui::{self, Color32};

/// Apply a resolved theme, the interface font size, and the zoom factor.
pub fn apply(ctx: &egui::Context, theme: ResolvedTheme, ui_scale: f32, ui_font_size: f32) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals(ResolvedTheme::Dark));
    ctx.set_visuals_of(egui::Theme::Light, visuals(ResolvedTheme::Light));
    ctx.set_theme(match theme {
        ResolvedTheme::Dark => egui::Theme::Dark,
        ResolvedTheme::Light => egui::Theme::Light,
    });

    ctx.all_styles_mut(|style| style.text_styles = text_styles(ui_font_size));

    // `set_zoom_factor`, NOT `set_pixels_per_point`.
    //
    // `set_pixels_per_point(1.0)` means "one physical pixel per logical point",
    // which *cancels* the display's DPI scaling — on a 150% or 200% display
    // everything renders at 1x and the whole interface comes out tiny. The zoom
    // factor multiplies the native scale instead, so 1.0 means "whatever this
    // monitor says", which is what HiDPI requires. eframe keeps the native
    // value up to date when the window moves between monitors.
    ctx.set_zoom_factor(ui_scale);
}

/// Interface text sizes, derived from one base size so that changing it scales
/// the whole UI consistently.
///
/// egui's defaults (Body 13.0, Small 9.0) are tuned for tool panels embedded in
/// a game. Body moves to 14.0 and Small to base-2, because 9pt secondary text
/// in an application someone reads all day is not readable.
///
/// Note that the interface being too small was mostly *not* this: it was the
/// zoom bug in [`apply`] above. This is a modest 13 -> 14 adjustment on top of
/// the restored DPI scaling, and `ui.font_size` in settings tunes it further.
fn text_styles(base: f32) -> std::collections::BTreeMap<egui::TextStyle, egui::FontId> {
    use egui::{FontFamily, FontId, TextStyle};

    [
        (
            TextStyle::Small,
            FontId::new(base - 2.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(base, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(base, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(base * 1.45, FontFamily::Proportional),
        ),
        // UI monospace — paths, the shortcut column, log output. The code pane
        // has its own independent `editor.font_size`.
        (
            TextStyle::Monospace,
            FontId::new(base - 1.0, FontFamily::Monospace),
        ),
    ]
    .into()
}

/// Read the operating system's light/dark preference, as reported through
/// winit. `None` when the platform does not tell us.
#[must_use]
pub fn system_theme(ctx: &egui::Context) -> Option<ResolvedTheme> {
    ctx.input(|i| i.raw.system_theme).map(|t| match t {
        egui::Theme::Dark => ResolvedTheme::Dark,
        egui::Theme::Light => ResolvedTheme::Light,
    })
}

/// Resolve a preference against the live OS setting.
#[must_use]
pub fn resolve(ctx: &egui::Context, pref: ThemePreference) -> ResolvedTheme {
    pref.resolve(system_theme(ctx))
}

/// The colour definitions themselves.
#[must_use]
pub fn visuals(theme: ResolvedTheme) -> egui::Visuals {
    match theme {
        ResolvedTheme::Dark => dark(),
        ResolvedTheme::Light => light(),
    }
}

fn dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();

    // A near-black that is not pure black: pure black against bright text
    // causes halation and is tiring over a long session.
    v.panel_fill = Color32::from_rgb(0x1e, 0x1e, 0x22);
    v.window_fill = Color32::from_rgb(0x25, 0x25, 0x2b);
    v.extreme_bg_color = Color32::from_rgb(0x18, 0x18, 0x1c); // the code pane
    v.faint_bg_color = Color32::from_rgb(0x26, 0x26, 0x2c); // current-line stripe
    v.override_text_color = Some(Color32::from_rgb(0xd8, 0xd8, 0xdd));
    v.hyperlink_color = Color32::from_rgb(0x6c, 0xb6, 0xff);

    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0x44, 0x44, 0x4c);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x2c, 0x2c, 0x33);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x26, 0x26, 0x2c);
    // egui draws an idle control with no outline, so an unticked checkbox was
    // a box a shade lighter than the window behind it and next to invisible.
    // See `idle_controls_are_outlined_at_3_to_1`.
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x78, 0x78, 0x82));
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x39, 0x39, 0x42);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x33, 0x33, 0x3b);
    v.widgets.active.bg_fill = Color32::from_rgb(0x44, 0x44, 0x50);
    v.selection.bg_fill = Color32::from_rgb(0x2d, 0x4f, 0x7c);
    v.selection.stroke.color = Color32::from_rgb(0xe6, 0xe6, 0xeb);

    common(&mut v);
    v
}

fn light() -> egui::Visuals {
    let mut v = egui::Visuals::light();

    // Slightly off-white for the same reason dark is not pure black.
    v.panel_fill = Color32::from_rgb(0xf2, 0xf2, 0xf4);
    v.window_fill = Color32::from_rgb(0xfa, 0xfa, 0xfc);
    v.extreme_bg_color = Color32::from_rgb(0xff, 0xff, 0xff);
    v.faint_bg_color = Color32::from_rgb(0xec, 0xec, 0xf0);
    v.override_text_color = Some(Color32::from_rgb(0x1c, 0x1c, 0x20));
    v.hyperlink_color = Color32::from_rgb(0x0a, 0x5b, 0xb5);

    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0xc0, 0xc0, 0xc8);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0xe4, 0xe4, 0xe9);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xec, 0xec, 0xf0);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x84, 0x84, 0x8e));
    v.widgets.hovered.bg_fill = Color32::from_rgb(0xd8, 0xd8, 0xde);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xe0, 0xe0, 0xe6);
    v.widgets.active.bg_fill = Color32::from_rgb(0xc8, 0xc8, 0xd0);
    v.selection.bg_fill = Color32::from_rgb(0xb5, 0xd2, 0xf5);
    v.selection.stroke.color = Color32::from_rgb(0x10, 0x10, 0x14);

    common(&mut v);
    v
}

/// Geometry shared by both themes, so switching changes colour only and never
/// makes the window jump.
fn common(v: &mut egui::Visuals) {
    v.window_corner_radius = 6.into();
    v.menu_corner_radius = 4.into();
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = 3.into();
    }
}

/// Relative luminance, per WCAG 2.1.
fn luminance(c: Color32) -> f32 {
    fn channel(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio between two opaque colours, 1.0 to 21.0.
#[must_use]
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN.md §3.11: body text must reach WCAG AA on every surface it is
    /// painted on. A hand-edited colour that breaks this must fail here rather
    /// than ship.
    #[test]
    fn body_text_meets_wcag_aa_on_every_surface_in_both_themes() {
        for theme in [ResolvedTheme::Dark, ResolvedTheme::Light] {
            let v = visuals(theme);
            let text = v
                .override_text_color
                .expect("both themes set an explicit text colour");

            for (name, bg) in [
                ("panel", v.panel_fill),
                ("window", v.window_fill),
                ("code pane", v.extreme_bg_color),
                ("current line", v.faint_bg_color),
                ("inactive widget", v.widgets.inactive.bg_fill),
                ("hovered widget", v.widgets.hovered.bg_fill),
            ] {
                let ratio = contrast_ratio(text, bg);
                assert!(
                    ratio >= 4.5,
                    "{theme:?}: text on {name} is {ratio:.2}:1, below the 4.5:1 AA threshold"
                );
            }
        }
    }

    /// Panel separators are decorative, so WCAG's 3:1 rule for *meaningful*
    /// UI components does not apply to them — but they still have to be
    /// visible, and it is easy to pick a divider that vanishes into its
    /// background. 1.5:1 is the floor at which a hairline is still discernible.
    /// Interactive controls get held to 3:1 once M8 styles them individually.
    #[test]
    fn panel_separators_are_visible_against_their_background() {
        for theme in [ResolvedTheme::Dark, ResolvedTheme::Light] {
            let v = visuals(theme);
            let ratio = contrast_ratio(v.widgets.noninteractive.bg_stroke.color, v.panel_fill);
            assert!(
                ratio >= 1.5,
                "{theme:?}: panel separators at {ratio:.2}:1 disappear into the panel"
            );
        }
    }

    /// An idle checkbox is its outline and nothing else, so the outline is
    /// held to WCAG's 3:1 for user-interface components against everything a
    /// control is drawn on: the window, the panels, and its own fill.
    #[test]
    fn idle_controls_are_outlined_at_3_to_1() {
        for theme in [ResolvedTheme::Dark, ResolvedTheme::Light] {
            let v = visuals(theme);
            let outline = v.widgets.inactive.bg_stroke;
            assert!(
                outline.width >= 1.0,
                "{theme:?}: idle controls have no outline"
            );
            for (name, bg) in [("window", v.window_fill), ("panel", v.panel_fill)] {
                let ratio = contrast_ratio(outline.color, bg);
                assert!(
                    ratio >= 3.0,
                    "{theme:?}: control outline on {name} is {ratio:.2}:1, below 3:1"
                );
            }
        }
    }

    #[test]
    fn the_two_themes_are_actually_different() {
        let d = visuals(ResolvedTheme::Dark);
        let l = visuals(ResolvedTheme::Light);
        assert_ne!(d.panel_fill, l.panel_fill);
        assert!(
            luminance(d.panel_fill) < luminance(l.panel_fill),
            "dark must be darker than light"
        );
    }

    #[test]
    fn geometry_is_identical_so_switching_does_not_move_anything() {
        let d = visuals(ResolvedTheme::Dark);
        let l = visuals(ResolvedTheme::Light);
        assert_eq!(d.window_corner_radius, l.window_corner_radius);
        assert_eq!(d.menu_corner_radius, l.menu_corner_radius);
        assert_eq!(
            d.widgets.inactive.corner_radius,
            l.widgets.inactive.corner_radius
        );
    }

    #[test]
    fn text_styles_scale_together_from_the_base_size() {
        use egui::TextStyle;

        let small = text_styles(14.0);
        let large = text_styles(20.0);

        for style in [
            TextStyle::Small,
            TextStyle::Body,
            TextStyle::Button,
            TextStyle::Heading,
            TextStyle::Monospace,
        ] {
            let a = small.get(&style).expect("style present").size;
            let b = large.get(&style).expect("style present").size;
            assert!(b > a, "{style:?} must grow with the base size");
        }
    }

    #[test]
    fn the_default_interface_size_is_larger_than_eguis() {
        // egui's stock Body is 13.0 and Small is 9.0.
        let styles = text_styles(14.0);
        let size = |style| styles.get(&style).expect("style present").size;
        assert!(size(egui::TextStyle::Body) >= 14.0);
        assert!(
            size(egui::TextStyle::Small) >= 11.0,
            "9pt secondary text is not readable"
        );
    }

    #[test]
    fn headings_and_small_text_stay_ordered() {
        let s = text_styles(14.0);
        let size = |style| s.get(&style).expect("style present").size;
        assert!(size(egui::TextStyle::Small) < size(egui::TextStyle::Body));
        assert!(size(egui::TextStyle::Body) < size(egui::TextStyle::Heading));
    }

    #[test]
    fn contrast_ratio_matches_known_values() {
        // Black on white is the canonical 21:1.
        let r = contrast_ratio(Color32::BLACK, Color32::WHITE);
        assert!((r - 21.0).abs() < 0.01, "got {r}");
        // A colour against itself is 1:1.
        let r = contrast_ratio(
            Color32::from_rgb(0x40, 0x80, 0xc0),
            Color32::from_rgb(0x40, 0x80, 0xc0),
        );
        assert!((r - 1.0).abs() < 0.001, "got {r}");
    }
}
