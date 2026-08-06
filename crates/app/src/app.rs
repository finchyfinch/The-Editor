//! The application shell: layout, command routing, and the document set.
//!
//! M1 builds the frame — panels, tabs, explorer, theme, palette, open/save.
//! The editor pane is still a read-only viewer; M2 replaces it with the real
//! editing widget. Everything the user can trigger goes through
//! [`EditorApp::run_command`], so the menus, the toolbar, the keyboard and the
//! palette cannot diverge in behaviour.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_config::paths::AppPaths;
use editor_config::settings::Settings;
use editor_config::theme::{ResolvedTheme, ThemePreference};
use editor_core::document::Document;
use editor_syntax::LanguageId;
use editor_widgets::{file_tree::FileTree, tab_bar, theme as ui_theme};
use eframe::egui;

use crate::commands::{self, CommandId};
use crate::palette::Palette;

/// Metadata shown in the About dialog.
pub(crate) struct BuildInfo {
    pub(crate) version: &'static str,
    pub(crate) commit: &'static str,
    pub(crate) date: &'static str,
    pub(crate) rustc: &'static str,
}

pub(crate) const BUILD: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("BUILD_COMMIT"),
    date: env!("BUILD_DATE"),
    rustc: env!("BUILD_RUSTC"),
};

/// How long a transient message stays on screen.
const TOAST_LIFETIME: Duration = Duration::from_secs(6);

/// A transient message. Errors surface here rather than as a panic or a
/// silently swallowed `Result`.
#[derive(Debug)]
struct Toast {
    text: String,
    error: bool,
    born: Instant,
}

/// One open document and its tab state.
#[derive(Debug)]
struct OpenDoc {
    doc: Document,
    language: LanguageId,
    /// Preview tabs are replaced by the next single-clicked file instead of
    /// accumulating. Promoted to permanent on double-click or first edit.
    preview: bool,
}

pub(crate) struct EditorApp {
    paths: AppPaths,
    settings: Settings,
    log_dir: String,

    tree: FileTree,
    docs: Vec<OpenDoc>,
    active: Option<usize>,

    palette: Palette,
    show_about: bool,
    show_shortcuts: bool,
    toasts: Vec<Toast>,

    /// What the theme preference last resolved to. Re-applied when it changes,
    /// which is how "follow system" reacts to the OS switching at runtime.
    applied_theme: Option<ResolvedTheme>,
    applied_scale: f32,
}

impl std::fmt::Debug for EditorApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorApp")
            .field("open_documents", &self.docs.len())
            .field("active", &self.active)
            .finish()
    }
}

impl EditorApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, paths: AppPaths, log_dir: String) -> Self {
        let (settings, settings_error) = Settings::load(&paths.settings_file());

        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
        });

        let mut app = Self {
            paths,
            log_dir,
            tree: FileTree::default(),
            docs: Vec::new(),
            active: None,
            palette: Palette::default(),
            show_about: false,
            show_shortcuts: false,
            toasts: Vec::new(),
            applied_theme: None,
            applied_scale: settings.ui_scale(),
            settings,
        };

        // A broken settings file must not stop The Editor starting; it becomes
        // a visible message and the defaults are used.
        if let Some(e) = settings_error {
            app.error(format!("Settings: {e:#}"));
        }
        // Write the documented default file on first run, so the settings the
        // user opens are self-explanatory rather than empty.
        if let Err(e) = app.settings.save() {
            app.error(format!("Could not write settings: {e:#}"));
        }

        app
    }

    // ---- messages --------------------------------------------------------

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("{text}");
        } else {
            tracing::info!("{text}");
        }
        self.toasts.push(Toast {
            text,
            error,
            born: Instant::now(),
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.toast(text, true);
    }

    fn info(&mut self, text: impl Into<String>) {
        self.toast(text, false);
    }

    // ---- documents -------------------------------------------------------

    fn active_doc(&self) -> Option<&OpenDoc> {
        self.active.and_then(|i| self.docs.get(i))
    }

    /// Open a file in a tab, or focus the tab it is already in.
    ///
    /// `preview` opens it in the reusable preview tab (single click in the
    /// explorer); otherwise it gets a permanent tab.
    fn open_path(&mut self, path: &Path, preview: bool) {
        if let Some(existing) = self.docs.iter().position(|d| d.doc.path() == Some(path)) {
            self.active = Some(existing);
            if !preview {
                self.docs[existing].preview = false;
            }
            return;
        }

        let doc = match Document::open(path) {
            Ok(doc) => doc,
            Err(e) => {
                self.error(format!("{e:#}"));
                return;
            }
        };

        if doc.is_large() {
            self.info(format!(
                "{} is large; opened read-only without highlighting",
                doc.display_name()
            ));
        }

        let language = path
            .extension()
            .and_then(|e| e.to_str())
            .map_or(LanguageId::PlainText, LanguageId::from_extension);

        let entry = OpenDoc {
            doc,
            language,
            preview,
        };

        // A preview tab replaces the existing one rather than adding to it.
        if preview && let Some(slot) = self.docs.iter().position(|d| d.preview) {
            self.docs[slot] = entry;
            self.active = Some(slot);
            return;
        }

        self.docs.push(entry);
        self.active = Some(self.docs.len() - 1);
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        // M2: prompt Save / Don't Save / Cancel when the document is dirty.
        // Nothing can be dirty yet, since editing does not exist.
        self.docs.remove(index);

        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.docs.len() - 1)),
            None => None,
        };
    }

    fn save_active(&mut self, ask_for_path: bool) {
        let Some(index) = self.active else {
            return;
        };
        let needs_path = ask_for_path || self.docs[index].doc.path().is_none();

        let result = if needs_path {
            let start = self
                .tree
                .root()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let chosen = rfd::FileDialog::new()
                .set_directory(start)
                .set_file_name(self.docs[index].doc.display_name())
                .save_file();
            let Some(path) = chosen else { return };
            self.docs[index].doc.save_as(&path)
        } else {
            self.docs[index].doc.save()
        };

        match result {
            Ok(()) => {
                let name = self.docs[index].doc.display_name();
                self.docs[index].preview = false;
                self.tree.refresh();
                self.info(format!("Saved {name}"));
            }
            Err(e) => self.error(format!("Save failed: {e:#}")),
        }
    }

    // ---- commands --------------------------------------------------------

    fn run_command(&mut self, id: CommandId, ctx: &egui::Context) {
        match id {
            CommandId::NewFile => {
                // The full New File wizard (name, language, boilerplate) is M8.
                self.docs.push(OpenDoc {
                    doc: Document::untitled(),
                    language: LanguageId::PlainText,
                    preview: false,
                });
                self.active = Some(self.docs.len() - 1);
            }
            CommandId::OpenFile => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.open_path(&path, false);
                }
            }
            CommandId::OpenFolder => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    tracing::info!(path = %dir.display(), "opening folder");
                    self.tree.set_root(dir);
                }
            }
            CommandId::Save => self.save_active(false),
            CommandId::SaveAs => self.save_active(true),
            CommandId::SaveAll => {
                let mut failures = Vec::new();
                for entry in &mut self.docs {
                    if entry.doc.is_dirty()
                        && entry.doc.path().is_some()
                        && let Err(e) = entry.doc.save()
                    {
                        failures.push(format!("{}: {e:#}", entry.doc.display_name()));
                    }
                }
                if failures.is_empty() {
                    self.info("All files saved");
                } else {
                    self.error(failures.join("; "));
                }
            }
            CommandId::CloseTab => {
                if let Some(active) = self.active {
                    self.close_tab(active);
                }
            }
            CommandId::CloseFolder => {
                self.tree = FileTree::default();
            }
            CommandId::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),

            CommandId::ToggleExplorer => {
                let show = !self.settings.show_file_tree();
                self.settings.set_show_file_tree(show);
                self.persist_settings();
            }
            CommandId::ThemeDark => self.set_theme(ThemePreference::Dark),
            CommandId::ThemeLight => self.set_theme(ThemePreference::Light),
            CommandId::ThemeSystem => self.set_theme(ThemePreference::System),
            CommandId::ToggleHiddenFiles => {
                let show = !self.tree.show_hidden();
                self.tree.set_show_hidden(show);
            }
            CommandId::ZoomIn => self.nudge_scale(0.1),
            CommandId::ZoomOut => self.nudge_scale(-0.1),
            CommandId::ZoomReset => {
                self.settings.set_ui_scale(1.0);
                self.persist_settings();
            }

            CommandId::CommandPalette => self.palette.open(),
            CommandId::OpenSettingsFile => {
                let path = self.paths.settings_file();
                self.open_path(&path, false);
            }

            CommandId::About => self.show_about = true,
            CommandId::KeyboardShortcuts => self.show_shortcuts = true,
            CommandId::OpenLogFolder => {
                let dir = self.paths.log_dir();
                if let Err(e) = open_in_file_manager(&dir) {
                    self.error(format!("Could not open {}: {e}", dir.display()));
                }
            }
        }
    }

    fn set_theme(&mut self, pref: ThemePreference) {
        self.settings.set_theme(pref);
        self.persist_settings();
        // Force a re-apply on the next frame even if the resolved theme is
        // unchanged, so switching Dark -> System with a dark OS still records
        // the new preference.
        self.applied_theme = None;
    }

    fn nudge_scale(&mut self, delta: f32) {
        let next = self.settings.ui_scale() + delta;
        self.settings.set_ui_scale(next);
        self.persist_settings();
    }

    fn persist_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.error(format!("Could not save settings: {e:#}"));
        }
    }

    /// Apply the theme and scale if either has changed since last frame.
    fn sync_appearance(&mut self, ctx: &egui::Context) {
        let resolved = ui_theme::resolve(ctx, self.settings.theme());
        let scale = self.settings.ui_scale();

        if self.applied_theme != Some(resolved) || (self.applied_scale - scale).abs() > f32::EPSILON
        {
            ui_theme::apply(ctx, resolved, scale);
            self.applied_theme = Some(resolved);
            self.applied_scale = scale;
        }
    }

    // ---- panels ----------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;

        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (menu, ids) in MENUS {
                    ui.menu_button(*menu, |ui| {
                        for id in *ids {
                            match id {
                                MenuEntry::Separator => {
                                    ui.separator();
                                }
                                MenuEntry::Item(id) => {
                                    if menu_item(ui, *id).clicked() {
                                        invoked = Some(*id);
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                }
            });
        });

        invoked
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;

        egui::Panel::top("toolbar").exact_size(36.0).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                for group in TOOLBAR {
                    for id in *group {
                        let cmd = commands::get(*id);
                        let tip = cmd.shortcut_text(ui.ctx()).map_or_else(
                            || cmd.title.to_owned(),
                            |sc| format!("{} ({sc})", cmd.title),
                        );
                        if ui.button(toolbar_glyph(*id)).on_hover_text(tip).clicked() {
                            invoked = Some(*id);
                        }
                    }
                    ui.separator();
                }
            });
        });

        invoked
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) -> Option<CommandId> {
        let mut invoked = None;

        // Read what the status bar needs before the closure borrows self.
        let summary = self.active_doc().map(|entry| {
            (
                entry.language.display_name(),
                entry.doc.encoding().label(),
                entry.doc.line_ending().label(),
                entry.doc.line_count(),
                entry.doc.read_only().is_some(),
            )
        });
        let theme_label = self.settings.theme().label();
        let tab_width = self.settings.tab_width();
        let insert_spaces = self.settings.insert_spaces();

        egui::Panel::bottom("status_bar")
            .exact_size(24.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    match &summary {
                        Some((lang, encoding, eol, lines, read_only)) => {
                            ui.weak(*lang);
                            ui.separator();
                            ui.weak(*encoding);
                            ui.separator();
                            ui.weak(*eol);
                            ui.separator();
                            ui.weak(format!(
                                "{}: {tab_width}",
                                if insert_spaces { "Spaces" } else { "Tabs" }
                            ));
                            ui.separator();
                            ui.weak(format!("{lines} lines"));
                            if *read_only {
                                ui.separator();
                                ui.weak("Read-only");
                            }
                        }
                        None => {
                            ui.weak("Ready");
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The theme indicator is a control, not a label — one
                        // of the three ways PLAN.md §3.11 requires it to be
                        // reachable.
                        if ui
                            .button(format!("\u{25d0} {theme_label}"))
                            .on_hover_text("Change theme")
                            .clicked()
                        {
                            invoked = Some(next_theme_command(theme_label));
                        }
                        ui.separator();
                        ui.weak(format!("v{}", BUILD.version));
                    });
                });
            });

        invoked
    }

    fn editor_pane(&mut self, ui: &mut egui::Ui) -> Option<tab_bar::Action> {
        let mut tab_action = None;

        egui::CentralPanel::default().show(ui, |ui| {
            if self.docs.is_empty() {
                self.welcome(ui);
                return;
            }

            let tabs: Vec<tab_bar::TabInfo> = self
                .docs
                .iter()
                .map(|entry| tab_bar::TabInfo {
                    title: entry.doc.display_name(),
                    tooltip: entry
                        .doc
                        .path()
                        .map_or_else(|| "Unsaved".to_owned(), |p| p.display().to_string()),
                    dirty: entry.doc.is_dirty(),
                    preview: entry.preview,
                })
                .collect();

            let action = tab_bar::ui(ui, &tabs, self.active);
            if action != tab_bar::Action::None {
                tab_action = Some(action);
            }
            ui.separator();

            if let Some(entry) = self.active.and_then(|i| self.docs.get(i)) {
                view_document(ui, &entry.doc, self.settings.font_size());
            }
        });

        tab_action
    }

    fn welcome(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.heading("The Editor");
            ui.label(format!("Version {}", BUILD.version));
            ui.add_space(24.0);

            egui::Grid::new("welcome_hints")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    for id in [
                        CommandId::OpenFolder,
                        CommandId::OpenFile,
                        CommandId::NewFile,
                        CommandId::CommandPalette,
                    ] {
                        let cmd = commands::get(id);
                        ui.weak(cmd.title);
                        ui.weak(cmd.shortcut_text(ui.ctx()).unwrap_or_default());
                        ui.end_row();
                    }
                });
        });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.born.elapsed() < TOAST_LIFETIME);
        if self.toasts.is_empty() {
            return;
        }
        // Keep repainting while any toast is on screen so it expires on time
        // rather than when the next input arrives.
        ctx.request_repaint_after(Duration::from_millis(250));

        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -40.0))
            .interactable(false)
            .show(ctx, |ui| {
                for toast in &self.toasts {
                    let colour = if toast.error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().text_color()
                    };
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.colored_label(colour, &toast.text);
                    });
                }
            });
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About The Editor")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.heading("The Editor");
                ui.label(format!("Version {}", BUILD.version));
                ui.add_space(6.0);
                ui.label("An IDE for Python and Rust.");
                ui.add_space(6.0);
                ui.label(format!(
                    "Copyright \u{a9} 2026 {}",
                    editor_config::APP_AUTHOR
                ));
                ui.label("Licensed under the MIT licence.");
                ui.add_space(10.0);
                ui.separator();
                egui::Grid::new("about_build")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Build");
                        ui.label(format!("{} ({})", BUILD.commit, BUILD.date));
                        ui.end_row();
                        ui.label("Compiler");
                        ui.label(BUILD.rustc);
                        ui.end_row();
                        ui.label("Config");
                        ui.label(self.paths.config_dir().display().to_string());
                        ui.end_row();
                        ui.label("Logs");
                        ui.label(&self.log_dir);
                        ui.end_row();
                        if self.paths.is_portable() {
                            ui.label("Mode");
                            ui.label("Portable");
                            ui.end_row();
                        }
                    });
            });
        self.show_about = open;
    }

    /// Generated from the registry, so it cannot describe a binding that does
    /// not exist. PLAN.md §3.10.
    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_shortcuts;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(460.0)
                    .show(ui, |ui| {
                        egui::Grid::new("shortcut_list")
                            .num_columns(2)
                            .striped(true)
                            .spacing([24.0, 4.0])
                            .show(ui, |ui| {
                                for cmd in commands::registry() {
                                    ui.label(cmd.palette_label());
                                    match cmd.shortcut_text(ui.ctx()) {
                                        Some(sc) => ui.label(sc),
                                        None => ui.weak("\u{2014}"),
                                    };
                                    ui.end_row();
                                }
                            });
                    });
            });
        self.show_shortcuts = open;
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync_appearance(&ctx);

        // One command per frame, from whichever source fired. Keyboard first,
        // so a shortcut is not swallowed by a menu that happens to be open —
        // except while the palette has focus, where keystrokes belong to its
        // query field and its own shortcut must not re-open it.
        let mut invoked = if self.palette.is_open() {
            None
        } else {
            commands::triggered(&ctx)
        };
        invoked = self.menu_bar(ui).or(invoked);
        invoked = self.toolbar(ui).or(invoked);
        invoked = self.status_bar(ui).or(invoked);

        if self.settings.show_file_tree() {
            egui::Panel::left("explorer")
                .default_size(250.0)
                .size_range(160.0..=600.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Explorer");
                    });
                    ui.separator();
                    match self.tree.ui(ui) {
                        editor_widgets::file_tree::Action::Open(path) => {
                            self.open_path(&path, false);
                        }
                        editor_widgets::file_tree::Action::Preview(path) => {
                            self.open_path(&path, true);
                        }
                        editor_widgets::file_tree::Action::None => {}
                    }
                });
        }

        egui::Panel::bottom("dock")
            .resizable(true)
            .default_size(150.0)
            .size_range(60.0..=600.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak("OUTPUT");
                    ui.weak("PROBLEMS");
                    ui.weak("SEARCH");
                    ui.weak("TERMINAL");
                });
                ui.separator();
                ui.weak("Output panel \u{2014} M7");
            });

        if let Some(action) = self.editor_pane(ui) {
            match action {
                tab_bar::Action::Select(i) => self.active = Some(i),
                tab_bar::Action::Close(i) => self.close_tab(i),
                tab_bar::Action::CloseOthers(keep) => {
                    if keep < self.docs.len() {
                        let kept = self.docs.remove(keep);
                        self.docs.clear();
                        self.docs.push(kept);
                        self.active = Some(0);
                    }
                }
                tab_bar::Action::CloseAll => {
                    self.docs.clear();
                    self.active = None;
                }
                tab_bar::Action::None => {}
            }
        }

        invoked = self.palette.ui(&ctx).or(invoked);

        self.about_window(&ctx);
        self.shortcuts_window(&ctx);
        self.toasts_ui(&ctx);

        if let Some(id) = invoked {
            tracing::debug!(?id, "command");
            self.run_command(id, &ctx);
        }
    }
}

// ---- free functions ------------------------------------------------------

/// Read-only document view. Rows are virtualised through egui's `show_rows`,
/// which is enough for looking at a file; M2 replaces this wholesale with the
/// real editor widget validated by `crates/spike`.
fn view_document(ui: &mut egui::Ui, doc: &Document, font_size: f32) {
    let font = egui::FontId::monospace(font_size);
    let row_height = ui.fonts_mut(|f| f.row_height(&font));
    let digits = doc.line_count().to_string().len();
    let text = doc.text();

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, doc.line_count(), |ui, rows| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for line in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{:>digits$}", line + 1))
                                .font(font.clone())
                                .weak(),
                        )
                        .selectable(false),
                    );
                    let content = text
                        .get_line(line)
                        .map(|l| l.to_string())
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(content.trim_end_matches(['\n', '\r']))
                            .font(font.clone()),
                    );
                });
            }
        });
}

/// Which theme command the status-bar button should fire: cycle Dark → Light →
/// System → Dark.
fn next_theme_command(current_label: &str) -> CommandId {
    match current_label {
        "Dark" => CommandId::ThemeLight,
        "Light" => CommandId::ThemeSystem,
        _ => CommandId::ThemeDark,
    }
}

fn menu_item(ui: &mut egui::Ui, id: CommandId) -> egui::Response {
    let cmd = commands::get(id);
    let mut button = egui::Button::new(cmd.title);
    if let Some(sc) = cmd.shortcut_text(ui.ctx()) {
        button = button.shortcut_text(sc);
    }
    ui.add(button)
}

fn toolbar_glyph(id: CommandId) -> &'static str {
    // Text glyphs until the icon set lands in M9. They are unambiguous with
    // the tooltip, which every button has.
    match id {
        CommandId::NewFile => "\u{2795}",
        CommandId::OpenFile => "\u{1f4c2}",
        CommandId::Save => "\u{1f4be}",
        CommandId::SaveAll => "\u{1f5c3}",
        CommandId::ToggleExplorer => "\u{2630}",
        CommandId::CommandPalette => "\u{2318}",
        CommandId::OpenSettingsFile => "\u{2699}",
        _ => "?",
    }
}

fn open_in_file_manager(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };

    cmd.spawn().map(|_| ())
}

/// One entry in a menu.
enum MenuEntry {
    Item(CommandId),
    Separator,
}

/// Menu structure. The titles and shortcuts come from the registry; this only
/// decides grouping and order.
const MENUS: &[(&str, &[MenuEntry])] = &[
    (
        "File",
        &[
            MenuEntry::Item(CommandId::NewFile),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::OpenFile),
            MenuEntry::Item(CommandId::OpenFolder),
            MenuEntry::Item(CommandId::CloseFolder),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Save),
            MenuEntry::Item(CommandId::SaveAs),
            MenuEntry::Item(CommandId::SaveAll),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::CloseTab),
            MenuEntry::Item(CommandId::OpenSettingsFile),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Exit),
        ],
    ),
    (
        "View",
        &[
            MenuEntry::Item(CommandId::ToggleExplorer),
            MenuEntry::Item(CommandId::ToggleHiddenFiles),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::ThemeDark),
            MenuEntry::Item(CommandId::ThemeLight),
            MenuEntry::Item(CommandId::ThemeSystem),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::ZoomIn),
            MenuEntry::Item(CommandId::ZoomOut),
            MenuEntry::Item(CommandId::ZoomReset),
        ],
    ),
    ("Tools", &[MenuEntry::Item(CommandId::CommandPalette)]),
    (
        "Help",
        &[
            MenuEntry::Item(CommandId::KeyboardShortcuts),
            MenuEntry::Item(CommandId::OpenLogFolder),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::About),
        ],
    ),
];

/// Toolbar groups, separated by dividers.
const TOOLBAR: &[&[CommandId]] = &[
    &[CommandId::NewFile, CommandId::OpenFile],
    &[CommandId::Save, CommandId::SaveAll],
    &[CommandId::ToggleExplorer],
    &[CommandId::CommandPalette, CommandId::OpenSettingsFile],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_and_toolbar_entry_is_a_registered_command() {
        for (menu, entries) in MENUS {
            for entry in *entries {
                if let MenuEntry::Item(id) = entry {
                    // `get` panics if the command is not registered.
                    let cmd = commands::get(*id);
                    assert!(!cmd.title.is_empty(), "{menu} has an untitled entry");
                }
            }
        }
        for group in TOOLBAR {
            for id in *group {
                assert_ne!(
                    toolbar_glyph(*id),
                    "?",
                    "{id:?} is on the toolbar but has no glyph"
                );
            }
        }
    }

    #[test]
    fn the_theme_button_cycles_through_all_three_preferences() {
        assert_eq!(next_theme_command("Dark"), CommandId::ThemeLight);
        assert_eq!(next_theme_command("Light"), CommandId::ThemeSystem);
        assert_eq!(next_theme_command("Follow System"), CommandId::ThemeDark);
    }

    #[test]
    fn every_theme_preference_has_a_command_and_a_menu_entry() {
        let in_menu: Vec<CommandId> = MENUS
            .iter()
            .flat_map(|(_, entries)| entries.iter())
            .filter_map(|e| match e {
                MenuEntry::Item(id) => Some(*id),
                MenuEntry::Separator => None,
            })
            .collect();

        for id in [
            CommandId::ThemeDark,
            CommandId::ThemeLight,
            CommandId::ThemeSystem,
        ] {
            assert!(in_menu.contains(&id), "{id:?} is not reachable from a menu");
        }
        assert_eq!(
            ThemePreference::ALL.len(),
            3,
            "a new theme preference needs a command, a menu entry and a status-bar cycle step"
        );
    }
}
