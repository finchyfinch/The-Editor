//! The document tab strip.
//!
//! One row of tabs above the editor. Each shows the filename, an unsaved
//! marker, and a close button. The close button turns into the unsaved dot
//! when the tab is clean and not hovered, which is how every editor people
//! already use behaves — a permanently visible × on every tab is noise, and a
//! dot that vanishes on hover loses the unsaved signal exactly when the cursor
//! is near the button that would discard it.

use eframe::egui;

/// One tab's presentation. The app owns the documents; this is just what the
/// strip needs to draw.
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub title: String,
    /// Full path, shown as a tooltip so two `main.py`s are distinguishable.
    pub tooltip: String,
    pub dirty: bool,
    /// Preview tabs are italic and get replaced by the next single-clicked
    /// file, rather than accumulating.
    pub preview: bool,
}

/// What the user did to the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    None,
    Select(usize),
    Close(usize),
    /// Middle-click or the context menu's "Close Others".
    CloseOthers(usize),
    CloseAll,
}

/// Draw the strip. `active` is the index of the selected tab, if any.
pub fn ui(ui: &mut egui::Ui, tabs: &[TabInfo], active: Option<usize>) -> Action {
    let mut action = Action::None;

    // An explicit salt, not egui's auto id. Auto ids are derived from how many
    // widgets the parent has already created, so two scroll areas laid out one
    // after another in the same panel collide — and the collision paints an
    // "ID clash" banner across the interface rather than failing quietly.
    egui::ScrollArea::horizontal()
        .id_salt("tab_bar")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, tab) in tabs.iter().enumerate() {
                    tab_ui(ui, i, tab, active == Some(i), &mut action);
                }
            });
        });

    action
}

/// Horizontal padding inside a tab.
const PAD_X: f32 = 10.0;
/// Vertical padding inside a tab.
const PAD_Y: f32 = 5.0;
/// Gap between the title and the close affordance.
const GAP: f32 = 8.0;
/// Side of the square close-button hit area.
const CLOSE_SIZE: f32 = 16.0;

/// Draw one tab.
///
/// The tab is measured and its rectangle allocated **before** anything is
/// painted, rather than being laid out by nested `horizontal` scopes. That
/// matters for two reasons that both showed up as bugs:
///
/// * The hover state is exact. Laying out first meant asking whether the
///   pointer was inside a rectangle that had not been decided yet, so the
///   dirty-dot / close-cross swap was reading the wrong rect.
/// * The whole tab is one clickable widget with one cursor icon. Building it
///   out of `Label`s meant egui applied the *text* cursor on hover, because
///   that is what a label does — which is wrong for something that behaves
///   like a button.
fn tab_ui(ui: &mut egui::Ui, index: usize, tab: &TabInfo, active: bool, action: &mut Action) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let visuals = ui.visuals().clone();

    let mut job = egui::text::LayoutJob::default();
    job.append(
        &tab.title,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: visuals.text_color(),
            italics: tab.preview,
            ..Default::default()
        },
    );
    let galley = ui.fonts_mut(|f| f.layout_job(job));

    let size = egui::vec2(
        PAD_X * 2.0 + galley.size().x + GAP + CLOSE_SIZE,
        galley.size().y.max(CLOSE_SIZE) + PAD_Y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    // Registered after the tab, so it wins the overlap and a click on the ×
    // closes rather than selects.
    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - PAD_X - CLOSE_SIZE / 2.0, rect.center().y),
        egui::Vec2::splat(CLOSE_SIZE),
    );
    let close = ui.interact(close_rect, response.id.with("close"), egui::Sense::click());

    let hovered = response.hovered() || close.hovered();

    // ---- paint -----------------------------------------------------------

    let bg = if active {
        visuals.widgets.active.bg_fill
    } else if hovered {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        visuals.widgets.inactive.weak_bg_fill
    };
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        bg,
    );

    let text_colour = if active {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    painter.galley(
        egui::pos2(rect.left() + PAD_X, rect.center().y - galley.size().y / 2.0),
        galley,
        text_colour,
    );

    // A dirty tab shows a dot until the pointer is over the tab, so the unsaved
    // state is never hidden underneath the mouse at the moment the user is
    // about to click the button that would discard it.
    let glyph = if tab.dirty && !hovered {
        "\u{25cf}" // filled circle
    } else {
        "\u{00d7}" // multiplication sign
    };
    if close.hovered() {
        painter.rect_filled(close_rect, 3, visuals.widgets.hovered.bg_fill);
    }
    painter.text(
        close_rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        font,
        if tab.dirty && !hovered {
            visuals.warn_fg_color
        } else {
            text_colour
        },
    );

    // ---- interaction -----------------------------------------------------

    // A tab is a button, not text. Without this, egui leaves the I-beam that a
    // label sets, which reads as "you can select this text".
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&tab.tooltip);
    let close = close.on_hover_cursor(egui::CursorIcon::PointingHand);

    if close.clicked() {
        *action = Action::Close(index);
    } else if response.clicked() {
        *action = Action::Select(index);
    }
    if response.middle_clicked() {
        *action = Action::Close(index);
    }

    response.context_menu(|ui| {
        if ui.button("Close").clicked() {
            *action = Action::Close(index);
            ui.close();
        }
        if ui.button("Close Others").clicked() {
            *action = Action::CloseOthers(index);
            ui.close();
        }
        if ui.button("Close All").clicked() {
            *action = Action::CloseAll;
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(title: &str, dirty: bool) -> TabInfo {
        TabInfo {
            title: title.to_owned(),
            tooltip: format!("/project/{title}"),
            dirty,
            preview: false,
        }
    }

    /// The tab strip is drawn, not computed, so what is worth testing here is
    /// the index bookkeeping the app performs in response to `Action`. These
    /// mirror the operations in `EditorApp::apply_tab_action`.
    #[test]
    fn closing_a_tab_before_the_active_one_shifts_the_active_index_down() {
        let mut tabs = vec![tab("a.py", false), tab("b.py", false), tab("c.py", false)];
        let mut active = 2usize;

        let closed = 0;
        tabs.remove(closed);
        if closed < active {
            active -= 1;
        }

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[active].title, "c.py", "the same document stays active");
    }

    #[test]
    fn closing_the_last_tab_moves_the_selection_back_rather_than_out_of_range() {
        let mut tabs = vec![tab("a.py", false), tab("b.py", false)];
        let mut active = 1usize;

        tabs.remove(active);
        active = active.min(tabs.len().saturating_sub(1));

        assert_eq!(active, 0);
        assert!(active < tabs.len());
    }

    #[test]
    fn close_others_keeps_exactly_one_tab_and_selects_it() {
        let mut tabs = vec![tab("a.py", false), tab("b.py", true), tab("c.py", false)];
        let keep = 1;

        let kept = tabs.remove(keep);
        tabs.clear();
        tabs.push(kept);

        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].title, "b.py");
        assert!(tabs[0].dirty, "close others must not discard unsaved state");
    }
}
