//! The New File dialog.
//!
//! Name, language, location, and an optional boilerplate template with a live
//! preview. See PLAN.md §7.
//!
//! The name and the language stay in step in both directions: choosing Python
//! sets the extension to `.py`, and typing `.rs` switches the language to Rust.
//! Fighting the dialog over the extension is a small thing that makes a tool
//! feel obstructive.

use std::path::{Path, PathBuf};

use editor_core::filename::{self, NameError};
use editor_syntax::LanguageId;
use editor_syntax::templates::{self, Template, Vars};
use eframe::egui;

/// What the dialog produced.
#[derive(Debug, Clone)]
pub(crate) struct NewFile {
    pub(crate) path: PathBuf,
    pub(crate) language: LanguageId,
    pub(crate) contents: String,
    /// Character offset for the caret, from the template's `$CURSOR`.
    pub(crate) cursor: usize,
}

pub(crate) struct Dialog {
    open: bool,
    stem: String,
    language: LanguageId,
    directory: PathBuf,
    use_boilerplate: bool,
    template_index: usize,
    just_opened: bool,
    /// Set when the chosen path already exists, so Create can warn rather than
    /// silently overwrite someone's file.
    overwrite_confirmed: bool,
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewFileDialog")
            .field("open", &self.open)
            .field("stem", &self.stem)
            .field("language", &self.language)
            .finish()
    }
}

impl Default for Dialog {
    fn default() -> Self {
        Self {
            open: false,
            stem: String::new(),
            language: LanguageId::Python,
            directory: PathBuf::new(),
            use_boilerplate: true,
            template_index: 0,
            just_opened: false,
            overwrite_confirmed: false,
        }
    }
}

impl Dialog {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Open the dialog, defaulting the location to `directory`.
    pub(crate) fn open(&mut self, directory: PathBuf) {
        self.open = true;
        self.just_opened = true;
        self.stem = "untitled".to_owned();
        self.directory = directory;
        self.use_boilerplate = true;
        self.template_index = default_template_index(self.language);
        self.overwrite_confirmed = false;
    }

    fn close(&mut self) {
        self.open = false;
    }

    /// The filename as it will be written.
    fn filename(&self) -> String {
        let typed = self.stem.trim();

        // A dotfile such as `.gitignore` is already a complete name; appending
        // an extension to it would produce `.gitignore.txt`.
        if typed.starts_with('.') && !typed.trim_start_matches('.').contains('.') {
            return typed.to_owned();
        }
        // Respect an extension the user typed themselves.
        if Path::new(typed).extension().is_some() {
            return typed.to_owned();
        }
        format!("{typed}.{}", self.language.default_extension())
    }

    fn path(&self) -> PathBuf {
        self.directory.join(self.filename())
    }

    fn templates(&self) -> Vec<&'static Template> {
        templates::for_language(self.language)
    }

    fn selected_template(&self) -> &'static Template {
        let available = self.templates();
        if !self.use_boilerplate {
            return templates::empty_for(self.language);
        }
        available
            .get(self.template_index)
            .copied()
            .unwrap_or_else(|| templates::empty_for(self.language))
    }

    fn vars(&self, author: &str) -> Vars {
        let filename = self.filename();
        let stem = filename::stem(&filename).to_owned();
        Vars {
            class_name: filename::to_pascal_case(&stem),
            filename,
            stem,
            author: author.to_owned(),
            date: templates::today_iso8601(),
        }
    }

    /// The problem with the current input, if any.
    fn problem(&self) -> Option<String> {
        if self.directory.as_os_str().is_empty() {
            return Some("Choose a location".to_owned());
        }
        // Check the typed name before the assembled one. An empty name plus an
        // automatic extension produces ".py", which passes filename validation
        // as a perfectly legal dotfile — and is certainly not what was meant.
        if self.stem.trim().is_empty() {
            return Some(NameError::Empty.to_string());
        }
        match filename::validate(&self.filename()) {
            Err(e) => Some(e.to_string()),
            Ok(()) => None,
        }
    }

    /// Draw the dialog. Returns the file to create once Create is pressed.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, author: &str) -> Option<NewFile> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }

        let mut created = None;

        egui::Modal::new(egui::Id::new("new_file_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("New File");
            ui.add_space(8.0);

            egui::Grid::new("new_file_fields")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.stem)
                                .desired_width(300.0)
                                .hint_text("file name"),
                        );
                        if self.just_opened {
                            edit.request_focus();
                            // Select the placeholder so typing replaces it.
                            self.just_opened = false;
                        }
                        if edit.changed() {
                            self.overwrite_confirmed = false;
                            self.sync_language_from_extension();
                        }
                        ui.weak(format!(".{}", self.language.default_extension()));
                    });
                    ui.end_row();

                    ui.label("Language");
                    egui::ComboBox::from_id_salt("new_file_language")
                        .selected_text(self.language.display_name())
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for language in LanguageId::ALL {
                                if ui
                                    .selectable_label(
                                        self.language == language,
                                        language.display_name(),
                                    )
                                    .clicked()
                                {
                                    self.language = language;
                                    self.template_index = default_template_index(language);
                                    self.overwrite_confirmed = false;
                                }
                            }
                        });
                    ui.end_row();

                    ui.label("Location");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(self.directory.display().to_string())
                                    .monospace(),
                            )
                            .truncate(),
                        );
                        if ui.button("Browse\u{2026}").clicked()
                            && let Some(dir) = rfd::FileDialog::new()
                                .set_directory(&self.directory)
                                .pick_folder()
                        {
                            self.directory = dir;
                            self.overwrite_confirmed = false;
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(6.0);
            ui.checkbox(&mut self.use_boilerplate, "Include boilerplate code");

            let available = self.templates();
            if self.use_boilerplate && available.len() > 1 {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label("Template");
                    let selected = available
                        .get(self.template_index)
                        .map_or("Empty", |t| t.name);
                    egui::ComboBox::from_id_salt("new_file_template")
                        .selected_text(selected)
                        .width(240.0)
                        .show_ui(ui, |ui| {
                            for (i, template) in available.iter().enumerate() {
                                if ui
                                    .selectable_label(self.template_index == i, template.name)
                                    .clicked()
                                {
                                    self.template_index = i;
                                }
                            }
                        });
                });
            }

            // Live preview, so the checkbox is not a guess.
            let rendered = templates::render(self.selected_template(), &self.vars(author));
            ui.add_space(8.0);
            ui.label("Preview");
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(8)
                .corner_radius(4)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if rendered.text.trim().is_empty() {
                                ui.weak("(empty file)");
                            } else {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&rendered.text).monospace(),
                                    )
                                    .selectable(false),
                                );
                            }
                        });
                });

            ui.add_space(8.0);

            let problem = self.problem();
            let exists = problem.is_none() && self.path().exists();

            if let Some(message) = &problem {
                ui.colored_label(ui.visuals().error_fg_color, format!("\u{26a0} {message}"));
            } else if exists && !self.overwrite_confirmed {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("\u{26a0} {} already exists", self.filename()),
                );
            } else {
                ui.weak(self.path().display().to_string());
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if exists && !self.overwrite_confirmed {
                        "Overwrite\u{2026}"
                    } else {
                        "Create"
                    };
                    let can_create = problem.is_none();
                    if ui
                        .add_enabled(can_create, egui::Button::new(label))
                        .clicked()
                    {
                        if exists && !self.overwrite_confirmed {
                            // One extra click before clobbering an existing
                            // file. Never silently.
                            self.overwrite_confirmed = true;
                        } else {
                            created = Some(NewFile {
                                path: self.path(),
                                language: self.language,
                                contents: rendered.text.clone(),
                                cursor: rendered.cursor,
                            });
                        }
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

    /// If the user typed an extension, switch the language to match it.
    fn sync_language_from_extension(&mut self) {
        if let Some(ext) = Path::new(&self.stem).extension().and_then(|e| e.to_str()) {
            let detected = LanguageId::from_extension(ext);
            if detected != LanguageId::PlainText && detected != self.language {
                self.language = detected;
                self.template_index = default_template_index(detected);
            }
        }
    }
}

/// Which template a language starts on.
///
/// Index 1 where there is one — the first real template after Empty — because
/// ticking "include boilerplate" and getting an empty file would be absurd.
fn default_template_index(language: LanguageId) -> usize {
    usize::from(templates::for_language(language).len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> Dialog {
        Dialog {
            open: true,
            stem: "thing".to_owned(),
            language: LanguageId::Python,
            directory: PathBuf::from("/project/src"),
            use_boilerplate: true,
            template_index: 1,
            just_opened: false,
            overwrite_confirmed: false,
        }
    }

    #[test]
    fn the_extension_follows_the_language() {
        let mut d = dialog();
        assert_eq!(d.filename(), "thing.py");
        d.language = LanguageId::Rust;
        assert_eq!(d.filename(), "thing.rs");
    }

    #[test]
    fn an_extension_typed_by_the_user_is_respected_and_sets_the_language() {
        let mut d = dialog();
        d.stem = "server.rs".to_owned();
        d.sync_language_from_extension();

        assert_eq!(d.language, LanguageId::Rust);
        assert_eq!(
            d.filename(),
            "server.rs",
            "the extension must not be doubled up"
        );
    }

    #[test]
    fn an_unknown_extension_leaves_the_language_alone() {
        let mut d = dialog();
        d.stem = "notes.xyz".to_owned();
        d.sync_language_from_extension();
        assert_eq!(
            d.language,
            LanguageId::Python,
            "an unrecognised extension must not reset the choice to plain text"
        );
        assert_eq!(d.filename(), "notes.xyz");
    }

    #[test]
    fn the_full_path_is_the_location_plus_the_filename() {
        let d = dialog();
        assert_eq!(d.path(), PathBuf::from("/project/src").join("thing.py"));
    }

    #[test]
    fn invalid_names_are_reported_and_block_creation() {
        let mut d = dialog();

        d.stem = "sub/dir".to_owned();
        assert!(
            d.problem().is_some_and(|p| p.contains('/')),
            "a path separator must be rejected, not silently create a directory"
        );

        d.stem = "CON".to_owned();
        assert!(d.problem().is_some_and(|p| p.contains("reserved")));

        d.stem = "perfectly_fine".to_owned();
        assert_eq!(d.problem(), None);
    }

    /// Regression: an empty name plus the automatic extension produced ".py",
    /// which is a legal dotfile and so passed validation. The dialog would have
    /// happily created a file called `.py`.
    #[test]
    fn an_empty_name_is_rejected_rather_than_creating_a_dotfile() {
        let mut d = dialog();
        d.stem = String::new();
        assert_eq!(d.problem(), Some("Enter a file name".to_owned()));

        d.stem = "   ".to_owned();
        assert_eq!(d.problem(), Some("Enter a file name".to_owned()));
    }

    #[test]
    fn a_dotfile_name_is_taken_as_complete() {
        let mut d = dialog();
        d.stem = ".gitignore".to_owned();
        assert_eq!(
            d.filename(),
            ".gitignore",
            "appending an extension here would produce .gitignore.py"
        );
        assert_eq!(d.problem(), None);
    }

    #[test]
    fn an_empty_location_is_reported() {
        let mut d = dialog();
        d.directory = PathBuf::new();
        assert_eq!(d.problem(), Some("Choose a location".to_owned()));
    }

    #[test]
    fn unticking_boilerplate_yields_an_empty_file_whatever_the_template() {
        let mut d = dialog();
        assert_ne!(d.selected_template().id, "python.empty");

        d.use_boilerplate = false;
        assert_eq!(d.selected_template().id, "python.empty");

        let rendered = templates::render(d.selected_template(), &d.vars("Author"));
        assert!(rendered.text.trim().is_empty());
    }

    #[test]
    fn ticking_boilerplate_selects_a_real_template_not_the_empty_one() {
        for language in [LanguageId::Python, LanguageId::Rust, LanguageId::Html] {
            let index = default_template_index(language);
            let chosen = templates::for_language(language)[index];
            assert_ne!(
                chosen.name, "Empty",
                "{language:?} defaults to Empty even with boilerplate ticked"
            );
        }
    }

    #[test]
    fn a_language_with_only_an_empty_template_still_works() {
        let index = default_template_index(LanguageId::Toml);
        assert_eq!(index, 0);
        assert_eq!(
            templates::for_language(LanguageId::Toml)[index].name,
            "Empty"
        );
    }

    #[test]
    fn template_variables_come_from_the_filename() {
        let mut d = dialog();
        d.stem = "my_data_store".to_owned();
        let v = d.vars("Gareth Finch");

        assert_eq!(v.filename, "my_data_store.py");
        assert_eq!(v.stem, "my_data_store");
        assert_eq!(v.class_name, "MyDataStore");
        assert_eq!(v.author, "Gareth Finch");
    }

    #[test]
    fn switching_language_switches_the_rendered_boilerplate() {
        let mut d = dialog();
        let python = templates::render(d.selected_template(), &d.vars("A")).text;
        assert!(python.contains("__main__"));

        d.language = LanguageId::Rust;
        d.template_index = default_template_index(LanguageId::Rust);
        let rust = templates::render(d.selected_template(), &d.vars("A")).text;
        assert!(rust.contains("fn main()"));
    }
}
