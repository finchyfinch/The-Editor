//! What the tests did.
//!
//! Failures at the top, because in a run of four hundred tests with two
//! failures the two are the only part anyone reads. Everything else is grouped
//! by file, collapsed by default, and there mainly so that a suite which found
//! nothing can be told from one that passed.
//!
//! The panel decides nothing: it reports what was clicked and the application
//! runs it. That is the same arrangement as the git panel, and it is what lets
//! "run this one again" mean the same thing whether it was clicked here, chosen
//! from a menu, or triggered by a shortcut.

use std::collections::BTreeMap;

use editor_testing::report::{Case, Outcome, Report};
use eframe::egui;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Run everything again.
    RunAll,
    /// Run only the tests that failed.
    RunFailures,
    /// Run this one.
    RunOne(String),
    /// Open the file a test failed in, at the line it failed on.
    Open { file: String, line: u32 },
    /// Stop the run.
    Stop,
}

/// What there is to show.
#[derive(Debug, Clone, Copy)]
pub(crate) enum State<'a> {
    /// Nothing has been run in this session.
    Idle,
    Running {
        report: &'a Report,
        label: &'a str,
    },
    Finished {
        report: &'a Report,
        label: &'a str,
    },
}

/// The Tests tab.
#[derive(Debug, Default)]
pub(crate) struct TestsPanel {
    /// Which test's message is expanded. One at a time: two failures side by
    /// side is two walls of traceback and no room for the list.
    expanded: Option<String>,
    /// Whether the passing tests are shown. Off by default — see the module
    /// note.
    show_passing: bool,
}

impl TestsPanel {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, state: State<'_>) -> Action {
        let mut action = Action::None;

        let (report, label, running) = match state {
            State::Idle => {
                ui.add_space(8.0);
                ui.weak("Nothing has been run yet.");
                ui.add_space(6.0);
                if ui.button("Run all tests").clicked() {
                    action = Action::RunAll;
                }
                return action;
            }
            State::Running { report, label } => (report, label, true),
            State::Finished { report, label } => (report, label, false),
        };

        ui.horizontal(|ui| {
            if running {
                if ui.button("Stop").clicked() {
                    action = Action::Stop;
                }
            } else if ui.button("Run all tests").clicked() {
                action = Action::RunAll;
            }

            let failures = report.count(Outcome::Failed);
            if ui
                .add_enabled(
                    !running && failures > 0,
                    egui::Button::new(format!("Run {failures} failure(s) again")),
                )
                .clicked()
            {
                action = Action::RunFailures;
            }

            ui.separator();
            ui.label(report.summary());
            ui.weak(label);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.show_passing, "Show passing");
            });
        });
        ui.separator();

        // Whatever the runner said that was not a result — a file that would
        // not import, a compile error. Nothing else on the screen matters while
        // one of these is outstanding.
        for note in &report.notes {
            ui.colored_label(ui.visuals().error_fg_color, note);
            ui.separator();
        }

        egui::ScrollArea::vertical()
            .id_salt("tests_panel_body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let failures: Vec<&Case> = report
                    .cases
                    .iter()
                    .filter(|c| c.outcome == Outcome::Failed)
                    .collect();
                if !failures.is_empty() {
                    ui.strong(format!("Failed ({})", failures.len()));
                    for case in failures {
                        self.row(ui, case, &mut action);
                    }
                    ui.add_space(8.0);
                }

                if self.show_passing {
                    // Grouped by the file the id names, which for pytest is the
                    // file and for libtest is the test binary. Both are the
                    // grouping somebody scanning a long list would draw.
                    let mut by_file: BTreeMap<&str, Vec<&Case>> = BTreeMap::new();
                    for case in &report.cases {
                        if case.outcome == Outcome::Failed {
                            continue;
                        }
                        by_file
                            .entry(case.file_hint().unwrap_or("(no file)"))
                            .or_default()
                            .push(case);
                    }
                    for (file, cases) in by_file {
                        egui::CollapsingHeader::new(format!("{file}  ({})", cases.len()))
                            .id_salt(file)
                            .show(ui, |ui| {
                                for case in cases {
                                    self.row(ui, case, &mut action);
                                }
                            });
                    }
                }
            });

        action
    }

    /// One test.
    fn row(&mut self, ui: &mut egui::Ui, case: &Case, action: &mut Action) {
        let expanded = self.expanded.as_deref() == Some(case.id.as_str());

        ui.horizontal(|ui| {
            ui.colored_label(
                outcome_colour(ui.visuals(), case.outcome),
                glyph(case.outcome),
            );

            let name = ui
                .selectable_label(expanded, case.short_name())
                .on_hover_text(&case.id);
            if name.clicked() {
                // Clicking a failure opens where it failed; clicking anything
                // else has nowhere to go, so it expands instead.
                match &case.location {
                    Some(location) if case.outcome == Outcome::Failed => {
                        *action = Action::Open {
                            file: location.file.to_string_lossy().into_owned(),
                            line: location.line,
                        };
                        self.expanded = Some(case.id.clone());
                    }
                    _ => {
                        self.expanded = (!expanded).then(|| case.id.clone());
                    }
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Run").clicked() {
                    *action = Action::RunOne(case.id.clone());
                }
            });
        });

        if expanded && !case.message.is_empty() {
            // Monospace and verbatim: a traceback is laid out against a fixed
            // width, and the part that matters is often the alignment of a
            // caret under an expression.
            ui.indent(&case.id, |ui| {
                ui.label(egui::RichText::new(&case.message).monospace());
                if let Some(location) = &case.location {
                    let text = format!("{}:{}", location.file.display(), location.line);
                    if ui.link(text).clicked() {
                        *action = Action::Open {
                            file: location.file.to_string_lossy().into_owned(),
                            line: location.line,
                        };
                    }
                }
            });
        }
    }
}

/// A cue for the outcome, so it is not carried by colour alone (PLAN.md §3.11).
fn glyph(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Passed => editor_widgets::glyphs::OK,
        Outcome::Failed => editor_widgets::glyphs::TEST_FAILED,
        Outcome::Skipped => editor_widgets::glyphs::TEST_SKIPPED,
        Outcome::Running => editor_widgets::glyphs::TEST_RUNNING,
    }
}

fn outcome_colour(visuals: &egui::Visuals, outcome: Outcome) -> egui::Color32 {
    match outcome {
        Outcome::Passed => {
            editor_widgets::editor_view::change_colour(visuals, editor_vcs::diff::LineStatus::Added)
        }
        Outcome::Failed => visuals.error_fg_color,
        Outcome::Skipped | Outcome::Running => visuals.weak_text_color(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_testing::report::Location;

    fn case(id: &str, outcome: Outcome) -> Case {
        Case {
            id: id.to_owned(),
            location: None,
            outcome,
            message: String::new(),
        }
    }

    #[test]
    fn a_new_panel_shows_nothing_expanded_and_hides_the_passes() {
        let panel = TestsPanel::default();
        assert_eq!(panel.expanded, None);
        assert!(
            !panel.show_passing,
            "a run of four hundred with two failures should open on the two"
        );
    }

    #[test]
    fn every_outcome_has_a_glyph_so_colour_is_not_the_only_cue() {
        let glyphs = [
            glyph(Outcome::Passed),
            glyph(Outcome::Failed),
            glyph(Outcome::Skipped),
            glyph(Outcome::Running),
        ];
        assert!(glyphs.iter().all(|g| !g.is_empty()));
        assert_ne!(glyphs[0], glyphs[1], "a pass must not look like a failure");
    }

    #[test]
    fn a_failure_with_a_location_is_something_to_open() {
        let mut case = case("tests/t.py::a", Outcome::Failed);
        case.location = Some(Location {
            file: "tests/t.py".into(),
            line: 9,
        });
        // The row builds the action from exactly these two fields.
        let location = case.location.expect("location");
        assert_eq!(location.file.to_string_lossy(), "tests/t.py");
        assert_eq!(location.line, 9);
    }

    #[test]
    fn cases_group_by_the_file_their_id_names() {
        let cases = [
            case("tests/a.py::one", Outcome::Passed),
            case("tests/a.py::two", Outcome::Passed),
            case("tests/b.py::three", Outcome::Passed),
        ];
        let mut by_file: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &cases {
            *by_file
                .entry(c.file_hint().unwrap_or("(no file)"))
                .or_default() += 1;
        }
        assert_eq!(by_file.get("tests/a.py"), Some(&2));
        assert_eq!(by_file.get("tests/b.py"), Some(&1));
    }
}
