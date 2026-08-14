//! The Packages panel: what is installed, what is out of date, and the file
//! that is supposed to record both.
//!
//! Answers the question people otherwise leave the editor to answer — "what
//! have I actually got in here, and is any of it old?" — and then lets them do
//! something about it without leaving either.
//!
//! Every change runs in the console rather than here. The venv dialog settled
//! that argument: pip's own failure message, in full, is the useful one, and a
//! progress spinner that turns into "installation failed" throws away the part
//! that says what to do about it.

use std::path::{Path, PathBuf};

use editor_proc::packages::{Change, Listing, Package, Report};
use eframe::egui;

/// What the panel is asking the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Run this in the console.
    Run(Change),
    /// Write `pip freeze` to the project's requirements file.
    Freeze,
    /// Open the requirements file in a tab.
    OpenRequirements,
}

#[derive(Debug, Default)]
pub(crate) struct PackagesPanel {
    listing: Option<Listing>,
    report: Report,
    /// True between asking and the first reply, so the panel can say so.
    loading: bool,
    filter: String,
    /// The name typed into the install box.
    wanted: String,
    /// Set when the panel is opened, so typing a package name needs no click.
    focus_install: bool,
}

impl PackagesPanel {
    /// Ask for a fresh listing. Cheap to call again; the old one is dropped,
    /// which cancels it.
    pub(crate) fn refresh(&mut self, interpreter: &Path) {
        self.listing = Some(Listing::start(interpreter));
        self.report = Report::default();
        self.loading = true;
    }

    pub(crate) fn opened(&mut self) {
        self.focus_install = true;
    }

    /// Take whatever the worker has produced. Call once per frame.
    ///
    /// Returns true while a listing is still in flight, so the caller knows to
    /// keep the frame loop turning.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(listing) = self.listing.as_ref() else {
            return false;
        };
        if let Some(report) = listing.poll() {
            self.report = report;
            self.loading = false;
        }
        // Finished once the update check has come back or something failed.
        let done = self.report.checked_for_updates || self.report.error.is_some();
        if done {
            self.listing = None;
        }
        !done
    }

    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        interpreter: Option<&Path>,
        requirements: Option<&Path>,
    ) -> Action {
        let mut action = Action::None;

        let Some(interpreter) = interpreter else {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label("No Python interpreter selected.");
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak("Tools > Select Interpreter, or create a virtual environment.");
            });
            return action;
        };

        action = self.toolbar(ui, interpreter, requirements).or(action);
        ui.separator();

        if let Some(error) = self.report.error.clone() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().error_fg_color, error);
            });
            return action;
        }

        if self.loading && self.report.packages.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.spinner();
                ui.label("Asking pip what is installed\u{2026}");
            });
            return action;
        }

        self.table(ui).or(action)
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        interpreter: &Path,
        requirements: Option<&Path>,
    ) -> Action {
        let mut action = Action::None;

        ui.horizontal_wrapped(|ui| {
            if ui.button("Refresh").clicked() {
                self.refresh(interpreter);
            }

            ui.separator();
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.wanted)
                    .desired_width(180.0)
                    .hint_text("Package to install"),
            );
            if std::mem::take(&mut self.focus_install) {
                field.request_focus();
            }
            // `lost_focus` covers Enter: a single-line field surrenders focus
            // when Enter is pressed in it.
            let submitted = field.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !self.wanted.trim().is_empty();
            let clicked = ui
                .add_enabled(!self.wanted.trim().is_empty(), egui::Button::new("Install"))
                .clicked();
            if submitted || clicked {
                // Whatever was typed, verbatim: `ruff`, `ruff==0.5.0` and
                // `ruff[extra]` are all things people mean, and second-guessing
                // the spec is how a package manager installs the wrong thing.
                action = Action::Run(Change::Install(self.wanted.trim().to_owned()));
                self.wanted.clear();
            }

            ui.separator();
            match requirements {
                Some(path) => {
                    if ui
                        .button("Install from requirements")
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        action = Action::Run(Change::InstallRequirements(path.to_path_buf()));
                    }
                    if ui
                        .button("Freeze")
                        .on_hover_text(format!(
                            "Overwrite {} with everything installed",
                            path.display()
                        ))
                        .clicked()
                    {
                        action = Action::Freeze;
                    }
                    if ui.link("open").clicked() {
                        action = Action::OpenRequirements;
                    }
                }
                None => {
                    if ui
                        .button("Freeze to requirements.txt")
                        .on_hover_text("Write everything installed to a new requirements.txt")
                        .clicked()
                    {
                        action = Action::Freeze;
                    }
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .desired_width(140.0)
                        .hint_text("Filter"),
                );
            });
        });

        ui.horizontal_wrapped(|ui| {
            ui.weak(interpreter.display().to_string());
            ui.separator();
            ui.weak(format!("{} installed", self.report.packages.len()));
            let outdated = self.outdated_count();
            if !self.report.checked_for_updates {
                ui.separator();
                ui.weak("checking for updates\u{2026}");
            } else if outdated > 0 {
                ui.separator();
                ui.label(format!("{outdated} with a newer release"));
            }
        });

        action
    }

    fn outdated_count(&self) -> usize {
        self.report
            .packages
            .iter()
            .filter(|p| p.latest.is_some())
            .count()
    }

    fn table(&mut self, ui: &mut egui::Ui) -> Action {
        let mut action = Action::None;
        let filter = self.filter.trim().to_ascii_lowercase();
        let rows: Vec<Package> = self
            .report
            .packages
            .iter()
            .filter(|p| filter.is_empty() || p.name.to_ascii_lowercase().contains(&filter))
            .cloned()
            .collect();

        if rows.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak(if self.report.packages.is_empty() {
                    "Nothing is installed in this environment."
                } else {
                    "Nothing matches that filter."
                });
            });
            return action;
        }

        egui::ScrollArea::vertical()
            .id_salt("packages_table")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("packages_grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for package in &rows {
                            ui.label(&package.name);
                            ui.monospace(&package.version);
                            match &package.latest {
                                Some(latest) => {
                                    ui.monospace(
                                        egui::RichText::new(latest)
                                            .color(ui.visuals().warn_fg_color),
                                    );
                                }
                                None => {
                                    ui.label("");
                                }
                            }
                            ui.horizontal(|ui| {
                                // The Upgrade slot keeps its width whether or not
                                // there is an upgrade, so Remove stays in one
                                // column down the table rather than shuffling
                                // left on every up-to-date row.
                                ui.allocate_ui_with_layout(
                                    egui::vec2(72.0, ui.spacing().interact_size.y),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        if package.latest.is_some()
                                            && ui.small_button("Upgrade").clicked()
                                        {
                                            action =
                                                Action::Run(Change::Upgrade(package.name.clone()));
                                        }
                                    },
                                );
                                if ui
                                    .small_button("Remove")
                                    .on_hover_text(format!("pip uninstall {}", package.name))
                                    .clicked()
                                {
                                    action = Action::Run(Change::Remove(package.name.clone()));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });

        action
    }
}

impl Action {
    /// Keep the first non-`None` of two actions.
    ///
    /// One frame can only carry out one thing, and a click is the thing the
    /// user just did — so an earlier one wins over a later widget's default.
    fn or(self, other: Self) -> Self {
        if self == Self::None { other } else { self }
    }
}

/// The project's requirements file, if it has one.
///
/// Only the conventional name. Looking for `requirements-dev.txt` and friends
/// would mean guessing which one a button should act on, and guessing wrong
/// there overwrites the wrong file.
#[must_use]
pub(crate) fn requirements_file(root: Option<&Path>) -> Option<PathBuf> {
    let path = root?.join("requirements.txt");
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_with_no_listing_is_not_busy() {
        let mut panel = PackagesPanel::default();
        assert!(!panel.poll());
    }

    #[test]
    fn the_outdated_count_only_counts_packages_with_a_newer_release() {
        let report = Report {
            packages: vec![
                Package {
                    name: "ruff".into(),
                    version: "0.5.0".into(),
                    latest: Some("0.6.0".into()),
                },
                Package {
                    name: "pytest".into(),
                    version: "8.2.1".into(),
                    latest: None,
                },
            ],
            checked_for_updates: true,
            error: None,
        };
        let panel = PackagesPanel {
            report,
            ..PackagesPanel::default()
        };
        assert_eq!(panel.outdated_count(), 1);
    }

    /// One frame carries one action, and it should be the one the user clicked
    /// rather than whatever a later widget produced.
    #[test]
    fn the_first_action_of_a_frame_wins() {
        let install = Action::Run(Change::Install("ruff".into()));
        assert_eq!(install.clone().or(Action::Freeze), install);
        assert_eq!(Action::None.or(Action::Freeze), Action::Freeze);
        assert_eq!(Action::None.or(Action::None), Action::None);
    }

    #[test]
    fn a_missing_requirements_file_is_reported_as_missing() {
        assert_eq!(requirements_file(None), None);
        let empty = std::env::temp_dir().join("the-editor-no-requirements");
        std::fs::create_dir_all(&empty).expect("create dir");
        assert_eq!(requirements_file(Some(&empty)), None);
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn a_requirements_file_that_exists_is_found() {
        let dir = std::env::temp_dir().join("the-editor-has-requirements");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("requirements.txt");
        std::fs::write(&path, b"ruff\n").expect("write");
        assert_eq!(requirements_file(Some(&dir)), Some(path));
        std::fs::remove_dir_all(&dir).ok();
    }
}
