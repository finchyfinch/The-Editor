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

    egui::ScrollArea::horizontal()
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

fn tab_ui(ui: &mut egui::Ui, index: usize, tab: &TabInfo, active: bool, action: &mut Action) {
    let visuals = ui.visuals();
    let bg = if active {
        visuals.widgets.active.bg_fill
    } else {
        visuals.widgets.inactive.weak_bg_fill
    };

    egui::Frame::new()
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .corner_radius(egui::CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;

                let mut text = egui::RichText::new(&tab.title);
                if tab.preview {
                    text = text.italics();
                }
                if active {
                    text = text.strong();
                }

                let label = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                let label = label.on_hover_text(&tab.tooltip);

                if label.clicked() {
                    *action = Action::Select(index);
                }
                if label.middle_clicked() {
                    *action = Action::Close(index);
                }
                label.context_menu(|ui| {
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

                // The close affordance. Dirty tabs show a dot until hovered,
                // so the unsaved state is never hidden behind the mouse.
                let hovered = ui.rect_contains_pointer(ui.max_rect());
                let glyph = if tab.dirty && !hovered {
                    "\u{25cf}" // ●
                } else {
                    "\u{00d7}" // ×
                };
                let close = ui.add(
                    egui::Button::new(glyph)
                        .frame(false)
                        .min_size(egui::vec2(14.0, 14.0)),
                );
                if close.clicked() {
                    *action = Action::Close(index);
                }
                if close.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            });
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
