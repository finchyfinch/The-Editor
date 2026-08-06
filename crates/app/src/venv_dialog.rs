//! The Create Virtual Environment dialog. See PLAN.md §3.8a.
//!
//! Deliberately thin. It gathers a base interpreter, a location and three
//! checkboxes, then hands plain commands to the console. Nothing happens
//! invisibly: the user watches `python -m venv` run and, when pip fails,
//! reads pip's own error.

use std::path::{Path, PathBuf};

use editor_proc::venv::{self, CreateOptions, Discovered};
use eframe::egui;

/// What the dialog produced.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) options: CreateOptions,
    /// Adopt the new environment as the project's interpreter afterwards.
    pub(crate) set_as_project_interpreter: bool,
    pub(crate) add_to_gitignore: bool,
    /// Where `.gitignore` lives.
    pub(crate) project_root: PathBuf,
}

pub(crate) struct Dialog {
    open: bool,
    /// Populated once, on open: discovery runs every candidate interpreter, so
    /// doing it per frame would spawn dozens of processes a second.
    interpreters: Vec<Discovered>,
    selected: usize,
    project_root: PathBuf,
    name: String,
    upgrade_pip: bool,
    install_requirements: bool,
    system_site_packages: bool,
    set_as_project_interpreter: bool,
    add_to_gitignore: bool,
    /// Set when discovery found nothing, so the dialog can say so rather than
    /// showing an empty dropdown.
    scanned: bool,
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenvDialog")
            .field("open", &self.open)
            .field("interpreters", &self.interpreters.len())
            .finish()
    }
}

impl Default for Dialog {
    fn default() -> Self {
        Self {
            open: false,
            interpreters: Vec::new(),
            selected: 0,
            project_root: PathBuf::new(),
            name: ".venv".to_owned(),
            upgrade_pip: true,
            install_requirements: true,
            system_site_packages: false,
            set_as_project_interpreter: true,
            add_to_gitignore: true,
            scanned: false,
        }
    }
}

impl Dialog {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Open for a project, scanning for interpreters as it does.
    pub(crate) fn open(&mut self, project_root: PathBuf) {
        self.interpreters = venv::discover();
        self.scanned = true;
        self.selected = 0;
        self.project_root = project_root;
        self.name = ".venv".to_owned();
        self.open = true;
    }

    fn close(&mut self) {
        self.open = false;
    }

    fn target(&self) -> PathBuf {
        self.project_root.join(self.name.trim())
    }

    fn requirements(&self) -> Option<PathBuf> {
        let path = self.project_root.join("requirements.txt");
        path.is_file().then_some(path)
    }

    /// The problem with the current input, if any.
    fn problem(&self) -> Option<String> {
        if self.project_root.as_os_str().is_empty() {
            return Some("Open a folder first".to_owned());
        }
        if self.interpreters.is_empty() {
            return Some("No Python installation found".to_owned());
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Some("Enter a folder name".to_owned());
        }
        if let Err(e) = editor_core::filename::validate(name) {
            return Some(e.to_string());
        }
        let target = self.target();
        if std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_some()) {
            return Some(format!("{} already exists", target.display()));
        }
        None
    }

    /// Draw the dialog. Returns a request once Create is pressed.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> Option<Request> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }

        let mut created = None;
        let requirements = self.requirements();

        egui::Modal::new(egui::Id::new("create_venv")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Create Virtual Environment");
            ui.add_space(8.0);

            egui::Grid::new("venv_fields")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Base interpreter");
                    if self.interpreters.is_empty() {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            if self.scanned {
                                "No Python installation found"
                            } else {
                                "Scanning\u{2026}"
                            },
                        );
                    } else {
                        let selected = self
                            .interpreters
                            .get(self.selected)
                            .map_or_else(String::new, Discovered::label);
                        egui::ComboBox::from_id_salt("venv_base")
                            .selected_text(selected)
                            .width(420.0)
                            .show_ui(ui, |ui| {
                                for (i, candidate) in self.interpreters.iter().enumerate() {
                                    if ui
                                        .selectable_label(self.selected == i, candidate.label())
                                        .clicked()
                                    {
                                        self.selected = i;
                                    }
                                }
                            });
                    }
                    ui.end_row();

                    ui.label("Folder name");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.name)
                            .desired_width(200.0)
                            .hint_text(".venv"),
                    );
                    ui.end_row();

                    ui.label("Location");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(self.target().display().to_string()).monospace(),
                        )
                        .truncate(),
                    );
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.checkbox(&mut self.upgrade_pip, "Upgrade pip after creation");

            ui.add_enabled_ui(requirements.is_some(), |ui| {
                let label = match &requirements {
                    Some(_) => "Install from requirements.txt".to_owned(),
                    None => "Install from requirements.txt (none found)".to_owned(),
                };
                ui.checkbox(&mut self.install_requirements, label);
            });

            ui.checkbox(
                &mut self.system_site_packages,
                "Inherit global site-packages",
            )
            .on_hover_text(
                "Lets the environment see packages installed system-wide. \
                 Usually left off: it is the isolation that makes a venv useful.",
            );
            ui.checkbox(
                &mut self.set_as_project_interpreter,
                "Use as this project's interpreter",
            );
            ui.checkbox(&mut self.add_to_gitignore, "Add to .gitignore");

            ui.add_space(10.0);
            let problem = self.problem();
            if let Some(message) = &problem {
                ui.colored_label(ui.visuals().error_fg_color, format!("\u{26a0} {message}"));
            } else {
                ui.weak("The commands will run in the output panel.");
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(problem.is_none(), egui::Button::new("Create"))
                        .clicked()
                        && let Some(base) = self.interpreters.get(self.selected)
                    {
                        created = Some(Request {
                            options: CreateOptions {
                                base: base.path.clone(),
                                target: self.target(),
                                upgrade_pip: self.upgrade_pip,
                                requirements: self
                                    .install_requirements
                                    .then(|| requirements.clone())
                                    .flatten(),
                                system_site_packages: self.system_site_packages,
                            },
                            set_as_project_interpreter: self.set_as_project_interpreter,
                            add_to_gitignore: self.add_to_gitignore,
                            project_root: self.project_root.clone(),
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        self.close();
                    }
                });
            });
        });

        if created.is_some() {
            self.close();
        }
        created
    }
}

/// Everything that happens once the commands have succeeded.
///
/// Separate from the dialog so it can be tested without a UI, and so the app
/// only has to remember one thing while the process runs.
#[derive(Debug, Clone)]
pub(crate) struct Completion {
    pub(crate) interpreter: PathBuf,
    pub(crate) set_as_project_interpreter: bool,
    pub(crate) add_to_gitignore: bool,
    pub(crate) project_root: PathBuf,
    pub(crate) folder_name: String,
}

impl Completion {
    #[must_use]
    pub(crate) fn from_request(request: &Request) -> Self {
        Self {
            interpreter: editor_proc::interpreter::venv_python(&request.options.target),
            set_as_project_interpreter: request.set_as_project_interpreter,
            add_to_gitignore: request.add_to_gitignore,
            project_root: request.project_root.clone(),
            folder_name: request
                .options
                .target
                .file_name()
                .map_or_else(|| ".venv".to_owned(), |n| n.to_string_lossy().into_owned()),
        }
    }

    /// The `.gitignore` entry for the new environment.
    #[must_use]
    pub(crate) fn gitignore_entry(&self) -> String {
        format!("{}/", self.folder_name.trim_end_matches('/'))
    }
}

/// Check the environment really was created, rather than trusting the exit
/// code alone.
#[must_use]
pub(crate) fn interpreter_exists(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog_for(root: &Path) -> Dialog {
        Dialog {
            open: true,
            interpreters: vec![Discovered {
                path: PathBuf::from("/usr/bin/python3"),
                version: "3.14.6".to_owned(),
            }],
            project_root: root.to_path_buf(),
            scanned: true,
            ..Dialog::default()
        }
    }

    #[test]
    fn the_target_is_the_named_folder_inside_the_project() {
        let dialog = dialog_for(Path::new("/project"));
        assert_eq!(dialog.target(), PathBuf::from("/project/.venv"));
    }

    #[test]
    fn a_custom_folder_name_is_honoured_and_trimmed() {
        let mut dialog = dialog_for(Path::new("/project"));
        dialog.name = "  env311  ".to_owned();
        assert_eq!(dialog.target(), PathBuf::from("/project/env311"));
    }

    #[test]
    fn an_empty_name_is_refused() {
        let mut dialog = dialog_for(Path::new("/project"));
        dialog.name = "   ".to_owned();
        assert_eq!(dialog.problem(), Some("Enter a folder name".to_owned()));
    }

    #[test]
    fn a_name_that_is_a_path_is_refused() {
        // Otherwise the dialog would happily create an environment outside the
        // project, or somewhere unexpected entirely.
        let mut dialog = dialog_for(Path::new("/project"));
        dialog.name = "../escape".to_owned();
        assert!(
            dialog
                .problem()
                .is_some_and(|p| p.contains('/') || p.contains("path")),
            "got {:?}",
            dialog.problem()
        );
    }

    #[test]
    fn no_python_installation_is_reported_rather_than_shown_as_an_empty_list() {
        let mut dialog = dialog_for(Path::new("/project"));
        dialog.interpreters.clear();
        assert_eq!(
            dialog.problem(),
            Some("No Python installation found".to_owned())
        );
    }

    #[test]
    fn no_open_folder_is_reported() {
        let dialog = dialog_for(Path::new(""));
        assert_eq!(dialog.problem(), Some("Open a folder first".to_owned()));
    }

    #[test]
    fn an_existing_environment_is_refused() {
        let root = std::env::temp_dir().join("the-editor-venv-dialog");
        let existing = root.join(".venv");
        std::fs::create_dir_all(&existing).expect("create dirs");
        std::fs::write(existing.join("pyvenv.cfg"), b"x").expect("occupy");

        let dialog = dialog_for(&root);
        assert!(
            dialog
                .problem()
                .is_some_and(|p| p.contains("already exists")),
            "got {:?}",
            dialog.problem()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn requirements_are_only_offered_when_the_file_exists() {
        let root = std::env::temp_dir().join("the-editor-venv-reqs");
        std::fs::create_dir_all(&root).expect("create dir");
        std::fs::remove_file(root.join("requirements.txt")).ok();

        let dialog = dialog_for(&root);
        assert_eq!(dialog.requirements(), None);

        std::fs::write(root.join("requirements.txt"), b"ruff\n").expect("write");
        assert!(dialog.requirements().is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_completion_points_at_the_new_environments_interpreter() {
        let request = Request {
            options: CreateOptions {
                base: PathBuf::from("/usr/bin/python3"),
                target: PathBuf::from("/project/.venv"),
                upgrade_pip: true,
                requirements: None,
                system_site_packages: false,
            },
            set_as_project_interpreter: true,
            add_to_gitignore: true,
            project_root: PathBuf::from("/project"),
        };

        let completion = Completion::from_request(&request);
        assert_eq!(
            completion.interpreter,
            editor_proc::interpreter::venv_python(Path::new("/project/.venv")),
            "the project must adopt the new environment, not the base interpreter"
        );
        assert_eq!(completion.folder_name, ".venv");
        assert_eq!(completion.gitignore_entry(), ".venv/");
    }

    #[test]
    fn the_gitignore_entry_does_not_double_its_slash() {
        let completion = Completion {
            interpreter: PathBuf::new(),
            set_as_project_interpreter: false,
            add_to_gitignore: true,
            project_root: PathBuf::new(),
            folder_name: "env/".to_owned(),
        };
        assert_eq!(completion.gitignore_entry(), "env/");
    }
}
