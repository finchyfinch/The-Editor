//! The find and replace bar.
//!
//! Sits above the editor and stays out of the way: it never takes focus back
//! once the user clicks into the text, and Escape always closes it. The match
//! list is recomputed only when the query or the document changes, not per
//! frame — searching a multi-megabyte file on every repaint would make typing
//! stutter for no benefit.

use std::ops::Range;

use editor_core::document::Document;
use editor_search::query::{Matcher, Query};
use eframe::egui;

/// What the bar wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    None,
    /// Move the caret to this match and scroll it into view.
    Reveal(Range<usize>),
    /// Replace one match with this text.
    Replace { range: Range<usize>, with: String },
    /// Replace every match, as one undo step.
    ReplaceAll(Vec<(Range<usize>, String)>),
    /// Return focus to the editor.
    FocusEditor,
}

/// Find/replace state for one document view.
#[derive(Debug, Default)]
pub struct FindBar {
    open: bool,
    show_replace: bool,
    query: Query,
    replacement: String,
    /// Cached results, keyed by what produced them.
    matches: Vec<Range<usize>>,
    current: Option<usize>,
    error: Option<String>,
    /// The query and document version the cache was built from.
    cached_for: Option<(Query, u64)>,
    focus_find: bool,
}

impl FindBar {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open in find mode. `seed` pre-fills the query, normally the selection.
    pub fn open_find(&mut self, seed: Option<String>) {
        self.open = true;
        self.show_replace = false;
        self.focus_find = true;
        if let Some(text) = seed.filter(|t| !t.is_empty() && !t.contains('\n')) {
            self.query.pattern = text;
            self.cached_for = None;
        }
    }

    /// Open in replace mode.
    pub fn open_replace(&mut self, seed: Option<String>) {
        self.open_find(seed);
        self.show_replace = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.current = None;
        self.cached_for = None;
    }

    /// Match ranges for the editor to highlight.
    #[must_use]
    pub fn matches(&self) -> &[Range<usize>] {
        &self.matches
    }

    /// Which match is the current one, for a stronger highlight.
    #[must_use]
    pub fn current_match(&self) -> Option<Range<usize>> {
        self.current.and_then(|i| self.matches.get(i)).cloned()
    }

    /// Recompute the match list if the query or the document has changed.
    fn refresh(&mut self, doc: &Document) {
        let key = (self.query.clone(), doc.version());
        if self.cached_for.as_ref() == Some(&key) {
            return;
        }
        self.cached_for = Some(key);

        if self.query.is_empty() {
            self.matches.clear();
            self.current = None;
            self.error = None;
            return;
        }

        match Matcher::new(&self.query) {
            Ok(matcher) => {
                self.matches = matcher.find_all(doc.text());
                self.error = None;
                // Keep the current match if it still exists, so replacing one
                // occurrence does not jump the view back to the top.
                self.current = self
                    .current
                    .map(|i| i.min(self.matches.len().saturating_sub(1)));
                if self.matches.is_empty() {
                    self.current = None;
                }
            }
            Err(e) => {
                self.matches.clear();
                self.current = None;
                self.error = Some(e.0);
            }
        }
    }

    /// Draw the bar. `caret` is where the editor's cursor is, so the first
    /// Enter jumps to the next match after it rather than back to the top.
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &Document, caret: usize) -> Action {
        if !self.open {
            return Action::None;
        }
        self.refresh(doc);

        let mut action = Action::None;
        // Whether one of this bar's own text fields owns the keyboard. Enter
        // means "next match" only then; see the guard at the end of this
        // function for why that matters.
        let mut typing_here = false;

        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let toggle = ui.small_button(if self.show_replace {
                        crate::glyphs::TREE_OPEN
                    } else {
                        crate::glyphs::TREE_CLOSED
                    });
                    if toggle.on_hover_text("Toggle replace").clicked() {
                        self.show_replace = !self.show_replace;
                    }

                    let find = ui.add(
                        egui::TextEdit::singleline(&mut self.query.pattern)
                            .hint_text("Find")
                            .desired_width(240.0),
                    );
                    // `lost_focus` as well as `has_focus`: a single-line
                    // TextEdit surrenders focus on Enter, during its own draw,
                    // so by the time we ask it no longer has it. Testing only
                    // `has_focus` would stop Enter finding the next match at
                    // all -- the very key this field exists to answer.
                    typing_here |= find.has_focus() || find.lost_focus();
                    if std::mem::take(&mut self.focus_find) {
                        find.request_focus();
                    }
                    if find.changed() {
                        self.cached_for = None;
                        self.refresh(doc);
                        // Re-anchor to the caret as the query changes, so
                        // typing walks forward through the file.
                        self.current = Matcher::next_from(&self.matches, caret);
                        if let Some(range) = self.current_match() {
                            action = Action::Reveal(range);
                        }
                    }

                    // Option toggles. Changing any of them invalidates the cache.
                    let mut options_changed = false;
                    options_changed |= ui
                        .selectable_label(self.query.case_sensitive, "Aa")
                        .on_hover_text("Match case")
                        .clicked()
                        .then(|| self.query.case_sensitive = !self.query.case_sensitive)
                        .is_some();
                    options_changed |= ui
                        .selectable_label(self.query.whole_word, "ab")
                        .on_hover_text("Whole word")
                        .clicked()
                        .then(|| self.query.whole_word = !self.query.whole_word)
                        .is_some();
                    options_changed |= ui
                        .selectable_label(self.query.regex, ".*")
                        .on_hover_text("Regular expression")
                        .clicked()
                        .then(|| self.query.regex = !self.query.regex)
                        .is_some();
                    if options_changed {
                        self.cached_for = None;
                        self.refresh(doc);
                    }

                    ui.separator();

                    if ui
                        .small_button(crate::icon::pick(ui, &["\u{2b06}", "\u{2191}", "^"]))
                        .on_hover_text("Previous (Shift+Enter)")
                        .clicked()
                        && let Some(range) = self.step(caret, -1)
                    {
                        action = Action::Reveal(range);
                    }
                    if ui
                        .small_button(crate::icon::pick(ui, &["\u{2b07}", "\u{2193}", "v"]))
                        .on_hover_text("Next (Enter)")
                        .clicked()
                        && let Some(range) = self.step(caret, 1)
                    {
                        action = Action::Reveal(range);
                    }

                    match (&self.error, self.matches.len()) {
                        (Some(message), _) => {
                            ui.colored_label(ui.visuals().error_fg_color, message);
                        }
                        (None, 0) if !self.query.is_empty() => {
                            ui.weak("No results");
                        }
                        (None, 0) => {}
                        (None, total) => {
                            let position = self.current.map_or(0, |i| i + 1);
                            ui.weak(format!("{position} of {total}"));
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(crate::icon::pick(ui, &["\u{00d7}", "x"]))
                            .on_hover_text("Close (Esc)")
                            .clicked()
                        {
                            self.close();
                            action = Action::FocusEditor;
                        }
                    });
                });

                if self.show_replace {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add_space(22.0);
                        let replace = ui.add(
                            egui::TextEdit::singleline(&mut self.replacement)
                                .hint_text(if self.query.regex {
                                    "Replace (use $1 for captures)"
                                } else {
                                    "Replace"
                                })
                                .desired_width(240.0),
                        );
                        typing_here |= replace.has_focus() || replace.lost_focus();

                        let has_matches = !self.matches.is_empty() && self.error.is_none();
                        if ui
                            .add_enabled(has_matches, egui::Button::new("Replace"))
                            .clicked()
                            && let Some(replace) = self.replace_one(doc, caret)
                        {
                            action = replace;
                        }
                        if ui
                            .add_enabled(has_matches, egui::Button::new("Replace All"))
                            .on_hover_text(format!("{} occurrences", self.matches.len()))
                            .clicked()
                            && let Some(replace) = self.replace_all(doc)
                        {
                            action = replace;
                        }
                    });
                }
            });

        // Keyboard, handled after the widgets so a focused field sees its own
        // characters first.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return Action::FocusEditor;
        }
        // Enter steps to the next match, but only when the keyboard is in one
        // of this bar's own fields.
        //
        // It used to be read unconditionally, and the bar is drawn before the
        // editor. So: leave a query in the box, click back into the code, press
        // Enter to split a line -- and the bar would jump the caret to the next
        // match, selecting it, and the editor would then insert the newline
        // there, on top of the selection. A keystroke meant for one place
        // silently rewrote another and deleted the matched word, with no clue
        // beyond the Find box not being empty. Clearing the box on blur is not
        // the answer either; a query is worth keeping between searches.
        let shift = ui.input(|i| i.modifiers.shift);
        let wanted = if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        // Consumed rather than peeked at, so the editor cannot act on the same
        // keystroke even if both somehow believe they have focus. The modifier
        // is chosen above rather than tried in turn because `consume_key`
        // matches logically -- asking for NONE would also swallow Shift+Enter.
        let enter = typing_here && ui.input_mut(|i| i.consume_key(wanted, egui::Key::Enter));
        if enter && let Some(range) = self.step(caret, if shift { -1 } else { 1 }) {
            action = Action::Reveal(range);
        }

        action
    }

    /// Move to the next or previous match, wrapping. Public so F3 and
    /// Shift+F3 reach the same logic as the bar's own arrows.
    pub fn step_from(&mut self, caret: usize, direction: isize) -> Option<Range<usize>> {
        self.step(caret, direction)
    }

    /// Move to the next or previous match, wrapping.
    fn step(&mut self, caret: usize, direction: isize) -> Option<Range<usize>> {
        if self.matches.is_empty() {
            return None;
        }
        let next = match (self.current, direction >= 0) {
            // No current match: start from wherever the caret is.
            (None, true) => Matcher::next_from(&self.matches, caret)?,
            (None, false) => Matcher::previous_from(&self.matches, caret)?,
            (Some(i), true) => (i + 1) % self.matches.len(),
            (Some(i), false) => i.checked_sub(1).unwrap_or(self.matches.len() - 1),
        };
        self.current = Some(next);
        self.matches.get(next).cloned()
    }

    fn replace_one(&mut self, doc: &Document, caret: usize) -> Option<Action> {
        let matcher = Matcher::new(&self.query).ok()?;
        // Without a current match, replace the first one after the caret rather
        // than silently doing nothing.
        let index = self
            .current
            .or_else(|| Matcher::next_from(&self.matches, caret))?;
        let range = self.matches.get(index)?.clone();
        let with = matcher.replacement(doc.text(), &range, &self.replacement);

        // The document is about to change, so the cache is stale.
        self.cached_for = None;
        Some(Action::Replace { range, with })
    }

    fn replace_all(&mut self, doc: &Document) -> Option<Action> {
        let matcher = Matcher::new(&self.query).ok()?;
        let edits: Vec<(Range<usize>, String)> = self
            .matches
            .iter()
            .map(|range| {
                let with = matcher.replacement(doc.text(), range, &self.replacement);
                (range.clone(), with)
            })
            .collect();
        if edits.is_empty() {
            return None;
        }
        self.cached_for = None;
        self.current = None;
        Some(Action::ReplaceAll(edits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::edit::Transaction;
    use editor_core::selection::Selection;

    fn doc_with(text: &str) -> Document {
        let mut doc = Document::untitled();
        doc.apply(
            &Transaction::insert(0, text),
            Selection::at(0),
            Selection::at(text.chars().count()),
        );
        doc
    }

    fn bar_for(pattern: &str) -> FindBar {
        FindBar {
            open: true,
            query: Query::literal(pattern),
            ..FindBar::default()
        }
    }

    #[test]
    fn refresh_finds_every_match() {
        let doc = doc_with("one two one three one");
        let mut bar = bar_for("one");
        bar.refresh(&doc);
        assert_eq!(bar.matches().len(), 3);
        assert!(bar.error.is_none());
    }

    #[test]
    fn results_are_cached_until_the_query_or_document_changes() {
        let doc = doc_with("aaa");
        let mut bar = bar_for("a");
        bar.refresh(&doc);
        let key = bar.cached_for.clone();

        bar.refresh(&doc);
        assert_eq!(
            bar.cached_for, key,
            "an unchanged query and document must not re-search"
        );

        bar.query.pattern = "aa".to_owned();
        bar.cached_for = None;
        bar.refresh(&doc);
        assert_ne!(bar.cached_for, key);
    }

    #[test]
    fn editing_the_document_invalidates_the_cache() {
        let mut doc = doc_with("target");
        let mut bar = bar_for("target");
        bar.refresh(&doc);
        assert_eq!(bar.matches().len(), 1);

        doc.apply(
            &Transaction::insert(0, "target "),
            Selection::at(0),
            Selection::at(7),
        );
        bar.refresh(&doc);
        assert_eq!(
            bar.matches().len(),
            2,
            "a document edit must be noticed without the query changing"
        );
    }

    #[test]
    fn stepping_forward_wraps_at_the_end() {
        let doc = doc_with("x x x");
        let mut bar = bar_for("x");
        bar.refresh(&doc);

        assert_eq!(bar.step(0, 1), Some(0..1));
        assert_eq!(bar.step(0, 1), Some(2..3));
        assert_eq!(bar.step(0, 1), Some(4..5));
        assert_eq!(bar.step(0, 1), Some(0..1), "wraps to the first match");
    }

    #[test]
    fn stepping_backward_wraps_at_the_start() {
        let doc = doc_with("x x x");
        let mut bar = bar_for("x");
        bar.refresh(&doc);

        bar.current = Some(0);
        assert_eq!(bar.step(0, -1), Some(4..5), "wraps to the last match");
    }

    #[test]
    fn the_first_step_starts_from_the_caret_not_the_top_of_the_file() {
        let doc = doc_with("x .... x .... x");
        let mut bar = bar_for("x");
        bar.refresh(&doc);

        assert_eq!(
            bar.step(8, 1),
            Some(14..15),
            "searching from mid-file should not jump backwards first"
        );
    }

    #[test]
    fn an_invalid_regex_reports_an_error_and_clears_the_matches() {
        let doc = doc_with("anything");
        let mut bar = FindBar {
            open: true,
            query: Query {
                regex: true,
                ..Query::literal("(unclosed")
            },
            ..FindBar::default()
        };
        bar.refresh(&doc);

        assert!(bar.error.is_some());
        assert!(bar.matches().is_empty());
        assert_eq!(bar.step(0, 1), None, "stepping must not panic");
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let doc = doc_with("some text");
        let mut bar = bar_for("");
        bar.refresh(&doc);
        assert!(bar.matches().is_empty());
        assert!(bar.error.is_none());
    }

    #[test]
    fn replace_all_produces_one_edit_per_match() {
        let doc = doc_with("one one one");
        let mut bar = bar_for("one");
        bar.refresh(&doc);
        bar.replacement = "two".to_owned();

        match bar.replace_all(&doc) {
            Some(Action::ReplaceAll(edits)) => {
                assert_eq!(edits.len(), 3);
                assert!(edits.iter().all(|(_, with)| with == "two"));
            }
            other => panic!("expected ReplaceAll, got {other:?}"),
        }
    }

    #[test]
    fn replace_all_expands_captures_in_regex_mode() {
        let doc = doc_with("a1 b2");
        let mut bar = FindBar {
            open: true,
            query: Query {
                regex: true,
                ..Query::literal(r"([a-z])(\d)")
            },
            replacement: "$2$1".to_owned(),
            ..FindBar::default()
        };
        bar.refresh(&doc);

        match bar.replace_all(&doc) {
            Some(Action::ReplaceAll(edits)) => {
                let replacements: Vec<&str> = edits.iter().map(|(_, w)| w.as_str()).collect();
                assert_eq!(replacements, ["1a", "2b"]);
            }
            other => panic!("expected ReplaceAll, got {other:?}"),
        }
    }

    #[test]
    fn replace_one_targets_the_current_match() {
        let doc = doc_with("one one one");
        let mut bar = bar_for("one");
        bar.refresh(&doc);
        bar.replacement = "two".to_owned();
        bar.current = Some(1);

        match bar.replace_one(&doc, 0) {
            Some(Action::Replace { range, with }) => {
                assert_eq!(range, 4..7);
                assert_eq!(with, "two");
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn replace_all_on_no_matches_does_nothing() {
        let doc = doc_with("nothing here");
        let mut bar = bar_for("absent");
        bar.refresh(&doc);
        assert!(bar.replace_all(&doc).is_none());
    }

    #[test]
    fn closing_clears_the_highlights() {
        let doc = doc_with("x x");
        let mut bar = bar_for("x");
        bar.refresh(&doc);
        assert!(!bar.matches().is_empty());

        bar.close();
        assert!(bar.matches().is_empty());
        assert!(bar.current_match().is_none());
        assert!(!bar.is_open());
    }

    #[test]
    fn opening_seeds_the_query_from_a_single_line_selection_only() {
        let mut bar = FindBar::default();
        bar.open_find(Some("word".to_owned()));
        assert_eq!(bar.query.pattern, "word");

        let mut bar = FindBar::default();
        bar.query.pattern = "kept".to_owned();
        bar.open_find(Some("two\nlines".to_owned()));
        assert_eq!(
            bar.query.pattern, "kept",
            "a multi-line selection is not a sensible seed"
        );
    }
}
