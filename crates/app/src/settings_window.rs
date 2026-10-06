//! The Settings window.
//!
//! Every control writes straight through to [`editor_config::settings::Settings`]
//! and the change takes effect on the next frame. There is no OK/Cancel and no
//! staging copy: an editor's settings are all small, reversible and immediately
//! visible, so showing the effect *is* the confirmation. A dialog that made you
//! press Apply to find out whether the font size was right would be worse.
//!
//! The file remains the source of truth — this window is a view onto it, not a
//! replacement for it. Anything hand-edited that this window does not know about
//! survives untouched, because the store underneath is a `toml_edit` document
//! rather than a struct (PLAN.md §3.9). The Advanced section is the escape hatch
//! to the file itself.
//!
//! Only settings that actually do something are shown. Offering a control that
//! silently does nothing is worse than not offering it, which is why
//! `editor.word_wrap` and `ui.syntax_theme` are absent until they are built.

use editor_config::settings::DocstringStyle;
use editor_config::settings::Renderer;
use editor_config::settings::Settings;
use editor_config::settings::TypeChecking;
use editor_config::settings::UnderlineDiagnostics;
use editor_config::theme::ThemePreference;
use eframe::egui;

/// Which page the window is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Appearance,
    Editor,
    Python,
    Advanced,
}

impl Page {
    const ALL: [Self; 4] = [Self::Appearance, Self::Editor, Self::Python, Self::Advanced];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Python => "Python",
            Self::Advanced => "Advanced",
        }
    }
}

/// What the window wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// A setting changed; write the file.
    Changed,
    /// Open `settings.toml` in a tab.
    OpenFile,
    /// Run the interpreter picker, which already exists as a command.
    PickInterpreter,
    /// Open the toolchain report.
    CheckToolchains,
}

/// Window state. Not the settings themselves — those live in `Settings`.
#[derive(Debug, Default)]
pub(crate) struct SettingsWindow {
    open: bool,
    page: Page,
    /// Edited in place so typing a path does not rewrite the file per keystroke;
    /// committed when the field loses focus or Enter is pressed.
    interpreter_draft: Option<String>,
}

impl SettingsWindow {
    pub(crate) fn open(&mut self, settings: &Settings) {
        self.open = true;
        self.interpreter_draft = Some(settings.python_interpreter());
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Draw the window. Mutates `settings` directly as controls are used.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        detected_interpreter: Option<&str>,
        running_servers: &[&'static str],
    ) -> Action {
        if !self.open {
            return Action::None;
        }

        let mut action = Action::None;
        let mut open = true;

        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .default_height(440.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // Page list on the left. Four pages does not warrant a tree.
                    ui.vertical(|ui| {
                        ui.set_min_width(120.0);
                        for page in Page::ALL {
                            if ui
                                .selectable_label(self.page == page, page.label())
                                .clicked()
                            {
                                self.page = page;
                            }
                        }
                    });
                    ui.separator();

                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("settings_body")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                action = match self.page {
                                    Page::Appearance => appearance(ui, settings),
                                    Page::Editor => editor(ui, settings),
                                    Page::Python => self.python(
                                        ui,
                                        settings,
                                        detected_interpreter,
                                        running_servers,
                                    ),
                                    Page::Advanced => advanced(ui, settings),
                                };
                            });
                    });
                });
            });

        if !open {
            self.close(settings);
        }
        action
    }

    fn close(&mut self, settings: &mut Settings) {
        // Commit a path typed but never blurred, so closing the window does not
        // silently discard it.
        if let Some(draft) = self.interpreter_draft.take()
            && draft != settings.python_interpreter()
        {
            settings.set_python_interpreter(draft.trim());
        }
        self.open = false;
    }

    fn python(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        detected: Option<&str>,
        running_servers: &[&'static str],
    ) -> Action {
        let mut action = Action::None;
        heading(ui, "Interpreter");

        let draft = self
            .interpreter_draft
            .get_or_insert_with(|| settings.python_interpreter());

        ui.label("Path to the Python used by Run and by virtual environment creation.");
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(draft)
                    .desired_width(340.0)
                    .hint_text("Leave empty to detect automatically"),
            );
            // Written on blur rather than per keystroke: a half-typed path is
            // not a setting, and rewriting the file on every character would
            // also spam the disk.
            //
            // Blur alone covers Enter, because a single-line TextEdit gives up
            // focus when Enter is pressed in it. The `|| key_pressed(Enter)`
            // that used to be here read the key whether or not this field had
            // it — the same mistake the find bar made, where it silently moved
            // the caret and rewrote the document.
            let committed = field.lost_focus();
            if committed && draft.trim() != settings.python_interpreter() {
                settings.set_python_interpreter(draft.trim());
                action = Action::Changed;
            }
            if ui.button("Browse...").clicked() {
                action = Action::PickInterpreter;
            }
        });

        if let Some(found) = detected {
            ui.horizontal(|ui| {
                ui.weak("Currently using:");
                ui.monospace(found);
            });
        } else {
            ui.weak("No interpreter found. Run will not work until one is set.");
        }
        if !draft.trim().is_empty() && ui.button("Clear (detect automatically)").clicked() {
            draft.clear();
            settings.set_python_interpreter("");
            action = Action::Changed;
        }

        ui.add_space(12.0);
        heading(ui, "Analysis");

        ui.small(
            "A server whose findings you do not trust is worth less than none. \
             Switching one off here stops it being started; the others carry on.",
        );
        ui.add_space(4.0);
        let disabled = settings.disabled_servers();
        for spec in editor_lsp::registry::ALL {
            let mut enabled = !disabled.iter().any(|d| d == spec.id);
            let label = format!("{} \u{2014} {}", spec.name, spec.provides);
            if ui.checkbox(&mut enabled, label).changed() {
                settings.set_server_enabled(spec.id, enabled);
                action = Action::Changed;
            }
        }
        ui.add_space(8.0);
        if row(
            ui,
            "Python type checking",
            "How strictly Pyright checks types. Off, it still provides hover, \
             completion and Go to Definition, and Ruff still reports undefined \
             names and unused imports. The stricter modes suit code with type hints \
             throughout; elsewhere most of what they report is a gap in a \
             library's stubs rather than a bug. A project's own pyrightconfig.json \
             or [tool.pyright] takes precedence.",
            |ui| {
                let mut mode = settings.type_checking();
                let before = mode;
                egui::ComboBox::from_id_salt("type_checking")
                    .selected_text(mode.label())
                    .show_ui(ui, |ui| {
                        for option in TypeChecking::ALL {
                            ui.selectable_value(&mut mode, option, option.label());
                        }
                    });
                if mode != before {
                    settings.set_type_checking(mode);
                    return true;
                }
                false
            },
        ) {
            action = Action::Changed;
        }
        ui.add_space(8.0);

        if running_servers.is_empty() {
            ui.label("No language server is running. Syntax is still checked by The Editor.");
        } else {
            ui.label(format!("Running: {}", running_servers.join(", ")));
        }
        if ui.button("Check Toolchains...").clicked() {
            action = Action::CheckToolchains;
        }

        action
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.strong(text);
    ui.add_space(4.0);
}

/// How wide a hint may be before it wraps. Narrower than the window, so the
/// text does not reflow every time the window is resized a little.
const HINT_WIDTH: f32 = 380.0;

/// A row of label plus control, so every page lines up the same way.
fn row(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    add: impl FnOnce(&mut egui::Ui) -> bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [170.0, 20.0],
            egui::Label::new(label).halign(egui::Align::LEFT),
        );
        changed = add(ui);
    });
    if !hint.is_empty() {
        // Indented under the control and wrapped. `ui.horizontal` lays out on
        // one infinitely wide line, so a hint of any length ran off the panel
        // and out of the window instead of folding onto a second line.
        ui.horizontal_top(|ui| {
            ui.add_space(174.0);
            ui.allocate_ui_with_layout(
                egui::vec2(HINT_WIDTH, 0.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(hint).small()).wrap());
                },
            );
        });
    }
    ui.add_space(6.0);
    changed
}

fn appearance(ui: &mut egui::Ui, settings: &mut Settings) -> Action {
    let mut changed = false;
    heading(ui, "Theme");

    changed |= row(ui, "Colour theme", "", |ui| {
        let mut theme = settings.theme();
        let before = theme;
        ui.horizontal(|ui| {
            ui.radio_value(&mut theme, ThemePreference::Dark, "Dark");
            ui.radio_value(&mut theme, ThemePreference::Light, "Light");
            ui.radio_value(&mut theme, ThemePreference::System, "Follow system");
        });
        if theme != before {
            settings.set_theme(theme);
            return true;
        }
        false
    });

    ui.add_space(8.0);
    heading(ui, "Size");

    changed |= row(
        ui,
        "Interface text",
        "Menus, tabs, the file tree and the status bar.",
        |ui| {
            let mut size = settings.ui_font_size();
            if ui
                .add(egui::Slider::new(&mut size, 9.0..=32.0).suffix(" pt"))
                .changed()
            {
                settings.set_ui_font_size(size);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Zoom",
        "Applied on top of the display's own DPI scaling. 1.0 uses whatever the monitor reports.",
        |ui| {
            let mut scale = settings.ui_scale();
            if ui
                .add(egui::Slider::new(&mut scale, 0.5..=3.0).fixed_decimals(2))
                .changed()
            {
                settings.set_ui_scale(scale);
                return true;
            }
            false
        },
    );

    ui.add_space(8.0);
    heading(ui, "Layout");

    changed |= row(ui, "Show the file tree", "", |ui| {
        let mut show = settings.show_file_tree();
        if ui.checkbox(&mut show, "").changed() {
            settings.set_show_file_tree(show);
            return true;
        }
        false
    });

    changed |= row(
        ui,
        "Preview tabs",
        "Single-clicking a file opens it in a reusable tab, in italics, that \
         the next single-clicked file replaces until you edit it or \
         double-click it. Off, every file you open gets a tab of its own.",
        |ui| {
            let mut preview = settings.preview_tabs();
            if ui.checkbox(&mut preview, "").changed() {
                settings.set_preview_tabs(preview);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Restore session on start",
        "Reopen the folder, tabs and window position from last time.",
        |ui| {
            let mut restore = settings.restore_session();
            if ui.checkbox(&mut restore, "").changed() {
                settings.set_restore_session(restore);
                return true;
            }
            false
        },
    );

    if changed {
        Action::Changed
    } else {
        Action::None
    }
}

fn editor(ui: &mut egui::Ui, settings: &mut Settings) -> Action {
    let mut changed = false;
    heading(ui, "Text");

    changed |= row(
        ui,
        "Code font size",
        "Independent of the interface text size.",
        |ui| {
            let mut size = settings.font_size();
            if ui
                .add(egui::Slider::new(&mut size, 6.0..=72.0).suffix(" pt"))
                .changed()
            {
                settings.set_font_size(size);
                return true;
            }
            false
        },
    );

    ui.add_space(8.0);
    heading(ui, "Indentation");

    changed |= row(
        ui,
        "Tab width",
        "How many columns one level of indent is.",
        |ui| {
            let mut width = settings.tab_width();
            if ui
                .add(egui::Slider::new(&mut width, 1..=16).suffix(" columns"))
                .changed()
            {
                settings.set_tab_width(width);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Insert spaces",
        "Off inserts real tab characters. Python code should leave this on.",
        |ui| {
            let mut spaces = settings.insert_spaces();
            if ui.checkbox(&mut spaces, "").changed() {
                settings.set_insert_spaces(spaces);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Follow .editorconfig",
        "A project's own file overrides the indent and save settings here for \
         the files it covers.",
        |ui| {
            let mut use_it = settings.use_editorconfig();
            if ui.checkbox(&mut use_it, "").changed() {
                settings.set_use_editorconfig(use_it);
                return true;
            }
            false
        },
    );

    ui.add_space(8.0);
    heading(ui, "On save");

    changed |= row(
        ui,
        "Trim trailing whitespace",
        "Invisible, meaningless, and noise in every later diff. Lands in the \
         undo history, so it can be taken back.",
        |ui| {
            let mut trim = settings.trim_trailing_whitespace();
            if ui.checkbox(&mut trim, "").changed() {
                settings.set_trim_trailing_whitespace(trim);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "End with a newline",
        "Files without one make diffs noisier and some tools drop the last line.",
        |ui| {
            let mut newline = settings.insert_final_newline();
            if ui.checkbox(&mut newline, "").changed() {
                settings.set_insert_final_newline(newline);
                return true;
            }
            false
        },
    );

    ui.add_space(8.0);
    heading(ui, "Diagnostics");

    changed |= row(
        ui,
        "Underline in the text",
        "The gutter marks and the Problems panel always show everything. A type \
         checker that cannot resolve a project's imports will report most of its \
         lines, which makes a file unreadable when all of them are underlined.",
        |ui| {
            let mut level = settings.underline_diagnostics();
            let before = level;
            egui::ComboBox::from_id_salt("underline_diagnostics")
                .selected_text(level.label())
                .show_ui(ui, |ui| {
                    for option in UnderlineDiagnostics::ALL {
                        ui.selectable_value(&mut level, option, option.label());
                    }
                });
            if level != before {
                settings.set_underline_diagnostics(level);
                return true;
            }
            false
        },
    );

    ui.add_space(8.0);
    heading(ui, "Typing");

    changed |= row(
        ui,
        "Auto-close brackets",
        "Typing an opening bracket or quote also inserts its closer.",
        |ui| {
            let mut close = settings.auto_close_brackets();
            if ui.checkbox(&mut close, "").changed() {
                settings.set_auto_close_brackets(close);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Python docstrings",
        "Typing \"\"\" on the first line of a def or class body writes a docstring \
         skeleton from the signature above it: one entry per parameter, the \
         return type, and anything the body raises. The layouts differ only in \
         how they are written down -- pick whichever the project already uses.",
        |ui| {
            let mut style = settings.docstrings();
            let before = style;
            let label = style.map_or("Off", DocstringStyle::label);
            egui::ComboBox::from_id_salt("docstrings")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut style, None, "Off");
                    for option in DocstringStyle::ALL {
                        ui.selectable_value(&mut style, Some(option), option.label());
                    }
                });
            if style != before {
                settings.set_docstrings(style);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Graphics backend",
        "glow (OpenGL) starts about sixteen times faster than wgpu and looks \
         identical, which is why it is the default. Switch to wgpu only if the \
         window fails to appear or draws incorrectly. Takes effect at the next \
         start.",
        |ui| {
            let mut renderer = settings.renderer();
            let before = renderer;
            egui::ComboBox::from_id_salt("renderer")
                .selected_text(renderer.as_str())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut renderer, Renderer::Glow, "glow");
                    ui.selectable_value(&mut renderer, Renderer::Wgpu, "wgpu");
                });
            if renderer != before {
                settings.set_renderer(renderer);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Sticky declarations",
        "Keep the def or class you are inside pinned to the top of the editor \
         once its own line has scrolled out of sight, up to four levels of \
         nesting. Click a pinned row to jump back to it.",
        |ui| {
            let mut sticky = settings.sticky_scopes();
            if ui.checkbox(&mut sticky, "").changed() {
                settings.set_sticky_scopes(sticky);
                return true;
            }
            false
        },
    );

    changed |= row(
        ui,
        "Reduce motion",
        "Stop the caret blinking. Repeating animation is distracting for some \
         people and disabling for a few, and the caret is the one animation \
         that is on screen the whole time you are reading.",
        |ui| {
            let mut reduce = settings.reduce_motion();
            if ui.checkbox(&mut reduce, "").changed() {
                settings.set_reduce_motion(reduce);
                return true;
            }
            false
        },
    );

    if changed {
        Action::Changed
    } else {
        Action::None
    }
}

fn advanced(ui: &mut egui::Ui, settings: &Settings) -> Action {
    let mut action = Action::None;
    heading(ui, "The settings file");

    ui.label(
        "Every setting lives in a TOML file you can edit by hand. Keys this version does \
         not recognise, and your comments, are preserved when the file is rewritten.",
    );
    ui.add_space(6.0);
    match settings.path() {
        Some(path) => {
            ui.horizontal(|ui| {
                ui.monospace(path.display().to_string());
            });
        }
        None => {
            ui.weak("No file: settings are in memory only for this session.");
        }
    }
    ui.add_space(6.0);
    if ui.button("Open settings.toml").clicked() {
        action = Action::OpenFile;
    }

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A form that writes a value the accessor then clamps away would show one
    /// thing and store another.
    #[test]
    fn every_control_writes_a_value_its_accessor_reads_back() {
        let mut settings = Settings::default();

        settings.set_theme(ThemePreference::Light);
        assert_eq!(settings.theme(), ThemePreference::Light);

        settings.set_ui_font_size(20.0);
        assert!((settings.ui_font_size() - 20.0).abs() < f32::EPSILON);

        settings.set_ui_scale(1.5);
        assert!((settings.ui_scale() - 1.5).abs() < f32::EPSILON);

        settings.set_show_file_tree(false);
        assert!(!settings.show_file_tree());

        settings.set_restore_session(false);
        assert!(!settings.restore_session());

        settings.set_font_size(18.0);
        assert!((settings.font_size() - 18.0).abs() < f32::EPSILON);

        settings.set_tab_width(2);
        assert_eq!(settings.tab_width(), 2);

        settings.set_insert_spaces(false);
        assert!(!settings.insert_spaces());

        settings.set_auto_close_brackets(false);
        assert!(!settings.auto_close_brackets());

        settings.set_docstrings(Some(DocstringStyle::Numpy));
        assert_eq!(settings.docstrings(), Some(DocstringStyle::Numpy));
        settings.set_docstrings(None);
        assert_eq!(settings.docstrings(), None);
        settings.set_reduce_motion(true);
        assert!(settings.reduce_motion());
        settings.set_renderer(Renderer::Wgpu);
        assert_eq!(settings.renderer(), Renderer::Wgpu);

        settings.set_underline_diagnostics(UnderlineDiagnostics::None);
        assert_eq!(settings.underline_diagnostics(), UnderlineDiagnostics::None);
        settings.set_underline_diagnostics(UnderlineDiagnostics::All);
        assert_eq!(settings.underline_diagnostics(), UnderlineDiagnostics::All);

        settings.set_python_interpreter("C:/Python/python.exe");
        assert_eq!(settings.python_interpreter(), "C:/Python/python.exe");
    }

    /// The slider bounds and the accessor clamps have to agree, or dragging to
    /// an end stop stores a value that reads back as something else.
    #[test]
    fn the_slider_ranges_match_what_the_accessors_will_keep() {
        let mut settings = Settings::default();

        settings.set_ui_font_size(9.0);
        assert!((settings.ui_font_size() - 9.0).abs() < f32::EPSILON);
        settings.set_ui_font_size(32.0);
        assert!((settings.ui_font_size() - 32.0).abs() < f32::EPSILON);

        settings.set_ui_scale(0.5);
        assert!((settings.ui_scale() - 0.5).abs() < f32::EPSILON);
        settings.set_ui_scale(3.0);
        assert!((settings.ui_scale() - 3.0).abs() < f32::EPSILON);

        settings.set_font_size(6.0);
        assert!((settings.font_size() - 6.0).abs() < f32::EPSILON);
        settings.set_font_size(72.0);
        assert!((settings.font_size() - 72.0).abs() < f32::EPSILON);

        settings.set_tab_width(1);
        assert_eq!(settings.tab_width(), 1);
        settings.set_tab_width(16);
        assert_eq!(settings.tab_width(), 16);
    }

    /// The whole reason the store is a `toml_edit` document: a form that
    /// rewrote the file from a struct would delete this.
    ///
    /// Goes through a real file rather than a test-only constructor, so what is
    /// exercised is the path the application actually takes.
    #[test]
    fn using_the_form_preserves_hand_written_comments_and_unknown_keys() {
        let dir = std::env::temp_dir().join("the-editor-settings-form-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("settings.toml");
        std::fs::write(
            &path,
            "\
# a comment the user wrote
[ui]
theme = \"dark\"
some_future_key = 42

[their_own_section]
whatever = true
",
        )
        .expect("write");

        let (mut settings, error) = Settings::load(&path);
        assert!(error.is_none(), "{error:?}");
        settings.set_ui_font_size(19.0);
        settings.set_tab_width(8);
        settings.save().expect("save");

        let out = std::fs::read_to_string(&path).expect("read back");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.contains("# a comment the user wrote"), "comment lost");
        assert!(out.contains("some_future_key = 42"), "unknown key lost");
        assert!(out.contains("[their_own_section]"), "unknown section lost");
        assert!(
            out.contains("tab_width = 8"),
            "the new value was not written"
        );
    }

    #[test]
    fn closing_the_window_commits_an_interpreter_path_still_being_typed() {
        // Typing a path and clicking the X must not throw the path away.
        let mut settings = Settings::default();
        let mut window = SettingsWindow {
            open: true,
            page: Page::Python,
            interpreter_draft: Some("  C:/Python/python.exe  ".to_owned()),
        };
        window.close(&mut settings);
        assert_eq!(settings.python_interpreter(), "C:/Python/python.exe");
        assert!(!window.is_open());
    }

    #[test]
    fn every_page_is_reachable_from_the_list() {
        // A page not in `ALL` would be unreachable, and the only clue would be
        // its absence from the sidebar.
        for page in Page::ALL {
            assert!(!page.label().is_empty());
        }
        assert_eq!(Page::ALL.len(), 4);
    }
}
