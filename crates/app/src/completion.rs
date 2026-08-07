//! The completion popup.
//!
//! Suggestions come from a language server, which answers several frames after
//! being asked. That gap is the whole design problem: by the time a reply
//! arrives the user has usually typed one or two more characters, and a popup
//! that replaced its contents wholesale on every reply would flicker and
//! reorder under the fingers.
//!
//! So the list is fetched once per *word*, at the position where the word
//! started, and narrowed locally as the user keeps typing. A reply is accepted
//! only if it answers the word still being typed; anything else is stale and
//! dropped. Typing a `.` or a fresh identifier character after a non-word
//! character starts a new word and a new request.
//!
//! Nothing here reaches the document. The popup reports what the user chose and
//! the application applies it, so the rules about undo grouping and the
//! highlighter's change outbox stay in one place.

use editor_lsp::session::Completion;
use eframe::egui;

/// The most suggestions shown at once. The rest are reachable by typing more,
/// which is faster than scrolling.
const VISIBLE: usize = 12;

/// What the popup wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    /// Replace the word being typed with this text.
    Accept {
        insert: String,
        replacing: usize,
    },
    /// The popup closed; nothing to do.
    Dismissed,
}

/// Popup state. Open only while there is something to show.
#[derive(Debug, Default)]
pub(crate) struct Popup {
    /// Everything the server sent for the current word.
    items: Vec<Completion>,
    /// Indices into `items` matching the prefix typed so far.
    matched: Vec<usize>,
    /// Which of `matched` is selected.
    selected: usize,
    /// Character offset where the word being completed starts.
    ///
    /// Everything from here to the caret is the prefix, and is what an accepted
    /// suggestion replaces.
    word_start: Option<usize>,
    /// The prefix the outstanding request was sent for, so a reply that arrives
    /// after the user has moved on can be recognised as stale.
    awaiting: Option<String>,
}

impl Popup {
    pub(crate) fn is_open(&self) -> bool {
        !self.matched.is_empty() && self.word_start.is_some()
    }

    /// True while a request is out and no answer has arrived.
    pub(crate) fn is_waiting(&self) -> bool {
        self.awaiting.is_some()
    }

    /// Note that a request has gone out for the word starting at `word_start`.
    pub(crate) fn requested(&mut self, word_start: usize, prefix: &str) {
        self.word_start = Some(word_start);
        self.awaiting = Some(prefix.to_owned());
        self.items.clear();
        self.matched.clear();
        self.selected = 0;
    }

    /// Take a server's reply.
    ///
    /// `prefix` is what the user has typed *now*, which may be longer than what
    /// was asked about. The reply is still usable in that case — it is a
    /// superset — so it is kept and filtered down.
    pub(crate) fn answered(&mut self, items: Vec<Completion>, prefix: &str) {
        let Some(asked) = self.awaiting.take() else {
            // Nothing outstanding: an answer to a request already abandoned.
            return;
        };
        if !prefix.starts_with(&asked) {
            // The user has backspaced past the point the request was made from,
            // or moved elsewhere entirely. The reply describes a word that no
            // longer exists.
            self.close();
            return;
        }
        self.items = items;
        self.refilter(prefix);
    }

    /// Narrow the list to what still matches, after another character.
    ///
    /// Case-insensitive and subsequence-free: a plain prefix match. Fuzzy
    /// matching belongs in the command palette, where the user is searching;
    /// here they are typing a name they already know, and a fuzzy match that
    /// promotes `filter_none` over `file` when they typed `fil` is a nuisance.
    pub(crate) fn refilter(&mut self, prefix: &str) {
        let lowered = prefix.to_lowercase();
        let previous = self.selected_label();

        self.matched = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.label.to_lowercase().starts_with(&lowered))
            .map(|(i, _)| i)
            .collect();

        // Keep the highlight on the same entry where it survived the filter, so
        // typing another character does not move the selection out from under
        // a user who was about to press Enter.
        self.selected = previous
            .and_then(|label| {
                self.matched
                    .iter()
                    .position(|i| self.items[*i].label == label)
            })
            .unwrap_or(0);
    }

    fn selected_label(&self) -> Option<String> {
        self.matched
            .get(self.selected)
            .map(|i| self.items[*i].label.clone())
    }

    pub(crate) fn close(&mut self) {
        self.items.clear();
        self.matched.clear();
        self.selected = 0;
        self.word_start = None;
        self.awaiting = None;
    }

    /// Claim the keys the popup owns, before anything else sees them.
    ///
    /// Must run *before* the editor reads the frame's events. egui hands the
    /// same event list to every widget that asks, so whoever looks first wins:
    /// drawing the popup at the end of the frame and consuming keys there meant
    /// Enter had already inserted a newline into the document behind it.
    pub(crate) fn handle_keys(&mut self, ctx: &egui::Context, prefix_len: usize) -> Action {
        if !self.is_open() {
            return Action::None;
        }
        let mut action = Action::None;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                action = Action::Dismissed;
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                self.selected = (self.selected + 1) % self.matched.len();
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                self.selected = (self.selected + self.matched.len() - 1) % self.matched.len();
            } else if (i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                && let Some(index) = self.matched.get(self.selected)
            {
                action = Action::Accept {
                    insert: self.items[*index].insert.clone(),
                    replacing: prefix_len,
                };
            }
        });
        if action != Action::None {
            self.close();
        }
        action
    }

    /// Draw the list. Returns a choice made with the mouse.
    ///
    /// `caret` is where the caret is on screen; the list hangs below it.
    pub(crate) fn draw(
        &mut self,
        ctx: &egui::Context,
        caret: egui::Rect,
        prefix_len: usize,
    ) -> Action {
        if !self.is_open() {
            return Action::None;
        }

        let mut clicked = None;
        egui::Area::new(egui::Id::new("completion_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(caret.left(), caret.bottom() + 4.0))
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(460.0);
                    // Keep the selection visible by starting the window at it
                    // once it has passed the bottom of the list.
                    let start = self.selected.saturating_sub(VISIBLE - 1);
                    for (row, index) in self.matched.iter().enumerate().skip(start).take(VISIBLE) {
                        let item = &self.items[*index];
                        let response = ui
                            .selectable_label(row == self.selected, row_text(item))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            clicked = Some(item.insert.clone());
                        }
                    }
                    if self.matched.len() > VISIBLE {
                        ui.weak(format!("{} more\u{2026}", self.matched.len() - VISIBLE));
                    }
                });
            });

        if let Some(insert) = clicked {
            self.close();
            return Action::Accept {
                insert,
                replacing: prefix_len,
            };
        }
        Action::None
    }
}

/// One row: kind glyph, label, and the detail greyed on the right.
fn row_text(item: &Completion) -> egui::RichText {
    let mut text = format!("{}  {}", item.glyph(), item.label);
    if let Some(detail) = &item.detail {
        // A type signature can run to a paragraph in Rust; the popup is a list.
        let short: String = detail.chars().take(48).collect();
        text.push_str("   ");
        text.push_str(short.trim());
    }
    egui::RichText::new(text).monospace()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Completion {
        Completion {
            label: label.to_owned(),
            insert: label.to_owned(),
            detail: None,
            kind: None,
            sort_text: None,
        }
    }

    fn opened(labels: &[&str], prefix: &str) -> Popup {
        let mut popup = Popup::default();
        popup.requested(0, prefix);
        popup.answered(labels.iter().map(|l| item(l)).collect(), prefix);
        popup
    }

    #[test]
    fn typing_more_narrows_the_list() {
        let mut popup = opened(&["parse", "partial", "print", "pop"], "p");
        assert_eq!(popup.matched.len(), 4);
        popup.refilter("pa");
        assert_eq!(popup.matched.len(), 2, "parse and partial");
        popup.refilter("par");
        assert_eq!(popup.matched.len(), 2);
        popup.refilter("pars");
        assert_eq!(popup.matched.len(), 1);
    }

    #[test]
    fn narrowing_to_nothing_closes_the_popup() {
        let mut popup = opened(&["parse", "print"], "p");
        popup.refilter("pz");
        assert!(!popup.is_open(), "an empty list must not stay on screen");
    }

    #[test]
    fn matching_ignores_case() {
        // Typing `js` should still offer `JSONDecoder`.
        let mut popup = opened(&["JSONDecoder", "join"], "j");
        popup.refilter("js");
        assert_eq!(popup.matched.len(), 1);
    }

    /// The selection must not move out from under someone about to press Enter.
    #[test]
    fn the_highlight_stays_on_the_same_entry_while_it_still_matches() {
        let mut popup = opened(&["parse", "partial", "particle"], "p");
        popup.selected = 2; // `particle`
        assert_eq!(popup.selected_label().as_deref(), Some("particle"));
        popup.refilter("part");
        assert_eq!(
            popup.selected_label().as_deref(),
            Some("particle"),
            "the highlight followed the wrong row"
        );
    }

    #[test]
    fn the_highlight_falls_back_to_the_top_when_its_entry_is_filtered_out() {
        let mut popup = opened(&["parse", "partial"], "p");
        popup.selected = 1; // `partial`
        popup.refilter("pars");
        assert_eq!(popup.selected, 0);
        assert_eq!(popup.selected_label().as_deref(), Some("parse"));
    }

    /// The reason replies carry the prefix they were asked for.
    #[test]
    fn a_reply_for_a_word_the_user_has_left_is_discarded() {
        let mut popup = Popup::default();
        popup.requested(10, "pa");
        // The user backspaced: the prefix is now shorter than what was asked.
        popup.answered(vec![item("parse")], "p");
        assert!(!popup.is_open(), "a stale reply must not open a popup");
    }

    #[test]
    fn a_reply_that_arrives_after_more_typing_is_still_used() {
        // Servers are slow; by the time `pa` comes back the user is on `par`.
        // The reply is a superset, so it is kept and narrowed rather than
        // thrown away, which would mean never showing a list to a fast typist.
        let mut popup = Popup::default();
        popup.requested(10, "pa");
        popup.answered(vec![item("parse"), item("pack")], "par");
        assert!(popup.is_open());
        assert_eq!(popup.matched.len(), 1, "narrowed to `parse`");
    }

    #[test]
    fn an_answer_with_nothing_outstanding_is_ignored() {
        let mut popup = Popup::default();
        popup.answered(vec![item("parse")], "p");
        assert!(!popup.is_open());
    }

    #[test]
    fn closing_forgets_everything() {
        let mut popup = opened(&["parse"], "p");
        assert!(popup.is_open());
        popup.close();
        assert!(!popup.is_open());
        assert!(!popup.is_waiting());
        assert_eq!(popup.word_start, None);
    }

    #[test]
    fn a_request_in_flight_is_reported_as_waiting() {
        // Used to avoid asking again on every keystroke.
        let mut popup = Popup::default();
        assert!(!popup.is_waiting());
        popup.requested(0, "p");
        assert!(popup.is_waiting());
        popup.answered(vec![item("parse")], "p");
        assert!(!popup.is_waiting());
    }

    /// The bug this split exists for: the popup drew at the end of the frame
    /// and consumed its keys there, by which time the editor had already read
    /// the same events and inserted a newline behind it.
    #[test]
    fn keys_are_claimed_separately_from_drawing() {
        // A compile-time guarantee more than a runtime one: `handle_keys` takes
        // no caret rect, so it cannot accidentally be moved to where the rect
        // is available -- which is only after the editor has painted.
        fn _assert_signature(
            popup: &mut Popup,
            ctx: &egui::Context,
            caret: egui::Rect,
        ) -> (Action, Action) {
            (popup.handle_keys(ctx, 0), popup.draw(ctx, caret, 0))
        }
    }

    #[test]
    fn a_row_shows_its_kind_and_detail_without_running_off_the_popup() {
        let long = Completion {
            detail: Some("a".repeat(300)),
            kind: Some(3),
            ..item("parse")
        };
        let text = row_text(&long).text().to_owned();
        assert!(text.contains("parse"));
        assert!(
            text.chars().count() < 120,
            "a row must not be a paragraph: {} chars",
            text.chars().count()
        );
    }
}
