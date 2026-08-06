//! The command palette (Ctrl+Shift+P).
//!
//! Fuzzy-matches the command registry with `nucleo`, the same matcher Helix
//! uses — subsequence scoring that rewards word-boundary and prefix hits, so
//! "ofo" finds "File: Open Folder" and ranks it above "File: Open File".

use eframe::egui;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::commands::{self, CommandId};

/// Palette state. Lives across frames so the query and selection persist while
/// it is open.
pub(crate) struct Palette {
    open: bool,
    query: String,
    selected: usize,
    matcher: Matcher,
    /// Rebuilt whenever the query changes.
    results: Vec<CommandId>,
    /// Set on the frame the palette opens, so focus can be grabbed once.
    just_opened: bool,
}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("open", &self.open)
            .field("query", &self.query)
            .field("results", &self.results.len())
            .finish()
    }
}

impl Default for Palette {
    fn default() -> Self {
        let mut p = Self {
            open: false,
            query: String::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            results: Vec::new(),
            just_opened: false,
        };
        p.refresh();
        p
    }
}

impl Palette {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.just_opened = true;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    /// Recompute the result list for the current query.
    fn refresh(&mut self) {
        self.results = rank(&self.query, &mut self.matcher);
        self.selected = 0;
    }

    /// Draw the palette. Returns the command the user chose, if any.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> Option<CommandId> {
        if !self.open {
            return None;
        }

        // Escape closes, and must be handled before the text field sees it.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }

        let mut chosen = None;
        let mut close_requested = false;

        egui::Modal::new(egui::Id::new("command_palette")).show(ctx, |ui| {
            ui.set_width(520.0);

            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Type a command\u{2026}")
                    .desired_width(f32::INFINITY),
            );
            if self.just_opened {
                edit.request_focus();
                self.just_opened = false;
            }
            if edit.changed() {
                self.refresh();
            }

            // Arrow keys move the selection without leaving the text field.
            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if down && !self.results.is_empty() {
                self.selected = (self.selected + 1) % self.results.len();
            }
            if up && !self.results.is_empty() {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or(self.results.len() - 1);
            }

            ui.separator();

            if self.results.is_empty() {
                ui.weak("No matching commands");
            }

            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (row, id) in self.results.clone().into_iter().enumerate() {
                        let cmd = commands::get(id);
                        let selected = row == self.selected;

                        let response = ui
                            .horizontal(|ui| {
                                let label = ui.selectable_label(selected, cmd.palette_label());
                                if let Some(sc) = cmd.shortcut_text(ui.ctx()) {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| ui.weak(sc),
                                    );
                                }
                                label
                            })
                            .inner;

                        if selected && (self.just_opened || down || up) {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            chosen = Some(id);
                        }
                    }
                });

            if enter {
                chosen = self.results.get(self.selected).copied();
            }
            if chosen.is_some() {
                close_requested = true;
            }
        });

        if close_requested {
            self.close();
        }
        chosen
    }
}

/// Score the registry against a query, best match first.
///
/// Pulled out of [`Palette`] so the ranking — the part with actual behaviour —
/// is testable without constructing UI state.
fn rank(query: &str, matcher: &mut Matcher) -> Vec<CommandId> {
    let registry = commands::registry();

    if query.trim().is_empty() {
        return registry.iter().map(|c| c.id).collect();
    }

    let pattern = Pattern::parse(query.trim(), CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, usize, CommandId)> = registry
        .iter()
        .enumerate()
        .filter_map(|(i, cmd)| {
            let label = cmd.palette_label();
            let mut buf = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(&label, &mut buf);
            pattern.score(haystack, matcher).map(|s| (s, i, cmd.id))
        })
        .collect();

    // Best score first; ties broken by registry order so the list does not
    // reshuffle unpredictably as the user types.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results_for(query: &str) -> Vec<String> {
        let mut matcher = Matcher::new(Config::DEFAULT);
        rank(query, &mut matcher)
            .iter()
            .map(|id| commands::get(*id).palette_label())
            .collect()
    }

    #[test]
    fn an_empty_query_lists_every_command_in_registry_order() {
        let all = results_for("");
        assert_eq!(all.len(), commands::registry().len());
        assert_eq!(all.first().map(String::as_str), Some("File: New File"));
    }

    #[test]
    fn matching_is_fuzzy_not_substring() {
        let results = results_for("ofo");
        assert!(
            results.iter().any(|r| r == "File: Open Folder"),
            "expected a subsequence match, got {results:?}"
        );
    }

    #[test]
    fn exact_words_rank_first() {
        let results = results_for("theme dark");
        assert_eq!(
            results.first().map(String::as_str),
            Some("View: Theme: Dark"),
            "got {results:?}"
        );
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(results_for("SAVE AS"), results_for("save as"));
        assert!(results_for("SAVE AS").iter().any(|r| r.contains("Save As")));
    }

    #[test]
    fn a_query_matching_nothing_yields_nothing_rather_than_everything() {
        assert!(results_for("zzzzqqqq").is_empty());
    }

    #[test]
    fn opening_resets_the_query_and_selection() {
        // Simulate a previous session's leftovers: the palette must not
        // reopen showing whatever was typed into it last time.
        let mut p = Palette {
            query: "stale".to_owned(),
            selected: 5,
            ..Palette::default()
        };
        p.open();
        assert!(p.is_open());
        assert!(p.query.is_empty());
        assert_eq!(p.selected, 0);
        assert_eq!(p.results.len(), commands::registry().len());
    }
}
