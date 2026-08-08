//! The project-wide search panel.
//!
//! Results stream in while the search runs, grouped by file. Grouping matters
//! more than it looks: a flat list of two hundred lines from forty files is
//! unreadable, and the question being asked is nearly always "which files is
//! this in" before "which lines".
//!
//! The search itself lives on a worker thread in `editor-search`; this owns
//! the query box, the grouping, and turning a click into a place to go.

use std::collections::BTreeMap;
use std::path::PathBuf;

use editor_search::project::{Hit, Progress, Search};
use editor_search::query::Query;
use eframe::egui;

/// What the panel wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Open this file with the caret on the match.
    Open {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    /// Start a search; the application supplies the root and the file list.
    Start(Query),
}

#[derive(Debug, Default)]
pub(crate) struct ProjectSearch {
    pub(crate) query: String,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    /// Results so far, grouped by file and in the order found within each.
    hits: BTreeMap<PathBuf, Vec<Hit>>,
    total: usize,
    running: Option<Search>,
    /// Set once the worker reports completion, for the summary line.
    finished: Option<Summary>,
    focus_query: bool,
}

#[derive(Debug, Clone, Copy)]
struct Summary {
    files: usize,
    truncated: bool,
}

impl ProjectSearch {
    /// Called when the panel is opened, so typing can start immediately.
    pub(crate) fn focus(&mut self) {
        self.focus_query = true;
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Take over a freshly started search.
    pub(crate) fn started(&mut self, search: Search) {
        // Replaces any predecessor, whose `Drop` cancels it -- otherwise two
        // searches race to fill the same list and the results interleave.
        self.running = Some(search);
        self.hits.clear();
        self.total = 0;
        self.finished = None;
    }

    /// Collect whatever the worker has produced. Call once per frame.
    pub(crate) fn poll(&mut self) {
        let Some(search) = self.running.as_ref() else {
            return;
        };
        let mut done = None;
        for progress in search.drain() {
            match progress {
                Progress::Hit(hit) => {
                    self.total += 1;
                    self.hits.entry(hit.path.clone()).or_default().push(hit);
                }
                Progress::Done { files, truncated } => {
                    done = Some(Summary { files, truncated });
                }
            }
        }
        if let Some(summary) = done {
            self.finished = Some(summary);
            self.running = None;
        }
    }

    fn current_query(&self) -> Query {
        Query {
            pattern: self.query.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let mut action = Action::None;

        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search the project\u{2026}")
                    .desired_width(280.0),
            );
            if std::mem::take(&mut self.focus_query) {
                field.request_focus();
            }
            // Enter, not every keystroke: a project search reads the whole tree
            // and firing one per character would spend its life cancelling.
            let go = ui.button("Search").clicked()
                || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));

            ui.checkbox(&mut self.case_sensitive, "Aa")
                .on_hover_text("Match case");
            ui.checkbox(&mut self.whole_word, "W")
                .on_hover_text("Whole word");
            ui.checkbox(&mut self.regex, ".*")
                .on_hover_text("Regular expression");

            if self.is_running() {
                ui.spinner();
                if ui.button("Stop").clicked()
                    && let Some(search) = self.running.take()
                {
                    search.cancel();
                }
            }

            if go && !self.query.trim().is_empty() {
                action = Action::Start(self.current_query());
            }
        });

        ui.separator();

        if self.hits.is_empty() {
            ui.weak(if self.is_running() {
                "Searching\u{2026}"
            } else if self.finished.is_some() {
                "No matches"
            } else {
                "Enter a search"
            });
            return action;
        }

        let summary = match self.finished {
            Some(s) if s.truncated => format!(
                "{} matches in {} files (stopped early \u{2014} narrow the search)",
                self.total,
                self.hits.len()
            ),
            Some(s) => format!(
                "{} matches in {} of {} files",
                self.total,
                self.hits.len(),
                s.files
            ),
            None => format!("{} matches so far\u{2026}", self.total),
        };
        ui.weak(summary);

        egui::ScrollArea::vertical()
            .id_salt("project_search_results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (path, hits) in &self.hits {
                    ui.horizontal(|ui| {
                        ui.strong(
                            path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        );
                        ui.weak(path.display().to_string().replace('\\', "/"));
                        ui.weak(format!("({})", hits.len()));
                    });
                    for hit in hits {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.weak(format!("{}", hit.line));
                            let row = ui.add(
                                egui::Label::new(egui::RichText::new(&hit.text).monospace())
                                    .sense(egui::Sense::click())
                                    .truncate(),
                            );
                            if row
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                action = Action::Open {
                                    path: path.clone(),
                                    line: hit.line,
                                    column: hit.column,
                                };
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            });

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, line: usize) -> Hit {
        Hit {
            path: PathBuf::from(path),
            line,
            column: 0,
            text: format!("line {line}"),
        }
    }

    fn with_hits(hits: &[Hit]) -> ProjectSearch {
        let mut panel = ProjectSearch::default();
        for h in hits {
            panel.total += 1;
            panel
                .hits
                .entry(h.path.clone())
                .or_default()
                .push(h.clone());
        }
        panel
    }

    #[test]
    fn results_are_grouped_by_file() {
        // A flat list of two hundred lines from forty files is unreadable, and
        // the first question is nearly always which files rather than which
        // lines.
        let panel = with_hits(&[hit("a.py", 1), hit("b.py", 2), hit("a.py", 9)]);
        assert_eq!(panel.hits.len(), 2);
        assert_eq!(panel.hits[&PathBuf::from("a.py")].len(), 2);
        assert_eq!(panel.total, 3);
    }

    #[test]
    fn files_are_listed_in_a_stable_order() {
        // A BTreeMap, so the list does not reshuffle as results stream in.
        let panel = with_hits(&[hit("z.py", 1), hit("a.py", 1), hit("m.py", 1)]);
        let order: Vec<String> = panel.hits.keys().map(|p| p.display().to_string()).collect();
        assert_eq!(order, ["a.py", "m.py", "z.py"]);
    }

    #[test]
    fn hits_within_a_file_keep_the_order_they_were_found_in() {
        let panel = with_hits(&[hit("a.py", 3), hit("a.py", 1), hit("a.py", 7)]);
        let lines: Vec<usize> = panel.hits[&PathBuf::from("a.py")]
            .iter()
            .map(|h| h.line)
            .collect();
        assert_eq!(lines, [3, 1, 7], "the worker reports in file order already");
    }

    #[test]
    fn the_query_carries_every_option() {
        let panel = ProjectSearch {
            query: "needle".to_owned(),
            case_sensitive: true,
            whole_word: true,
            regex: true,
            ..ProjectSearch::default()
        };
        let q = panel.current_query();
        assert_eq!(q.pattern, "needle");
        assert!(q.case_sensitive && q.whole_word && q.regex);
    }

    #[test]
    fn starting_a_search_clears_the_previous_results() {
        // Otherwise a second search appends to the first and the counts lie.
        let mut panel = with_hits(&[hit("a.py", 1)]);
        assert_eq!(panel.total, 1);
        let empty = Search::start(
            std::path::Path::new("/nowhere"),
            Vec::new(),
            &Query::literal("x"),
        );
        panel.started(empty);
        assert!(panel.hits.is_empty());
        assert_eq!(panel.total, 0);
    }
}
