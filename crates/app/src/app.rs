//! The application shell: layout, command routing, and the document set.
//!
//! Everything the user can trigger goes through [`EditorApp::run_command`], so
//! the menus, the toolbar, the keyboard and the palette cannot diverge in
//! behaviour.
//!
//! Each open document carries its own editing view state and its own parse tree
//! for highlighting. Edits reach the parse tree by draining the document's
//! change outbox once per frame rather than by every edit path remembering to
//! notify it — the same route the language server will take in M6.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_config::paths::AppPaths;
use editor_config::settings::Settings;
use editor_config::theme::{ResolvedTheme, ThemePreference};
use editor_core::document::Document;
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::theme::SyntaxTheme;
use editor_widgets::editor_view::{EditorOptions, EditorView};
use editor_widgets::{file_tree::FileTree, tab_bar, theme as ui_theme};
use eframe::egui;

use crate::commands::{self, CommandId};
use crate::new_file;
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

/// The user's answer to the unsaved-changes prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Save,
    Discard,
    Cancel,
}

/// An action that would discard unsaved work, held until the user answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    CloseTab(usize),
    CloseOthers(usize),
    CloseAll,
    Quit,
}

/// One open document, its view state, and its tab state.
#[derive(Debug)]
struct OpenDoc {
    doc: Document,
    view: EditorView,
    language: LanguageId,
    /// Parse state for highlighting. `None` for plain text, for languages with
    /// no grammar, and for files too large to highlight.
    highlighter: Option<Highlighter>,
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
    new_file: new_file::Dialog,
    show_about: bool,
    show_shortcuts: bool,
    toasts: Vec<Toast>,
    /// A destructive action waiting on the user's answer about unsaved work.
    pending: Option<Pending>,
    /// Set once the user has answered the quit prompt, so the second close
    /// request is not intercepted again.
    quit_confirmed: bool,

    /// What the theme preference last resolved to. Re-applied when it changes,
    /// which is how "follow system" reacts to the OS switching at runtime.
    applied_theme: Option<ResolvedTheme>,
    applied_scale: f32,
    applied_ui_font: f32,
    /// Rebuilt whenever the UI theme changes, so the code pane follows it.
    syntax_theme: SyntaxTheme,
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
            new_file: new_file::Dialog::default(),
            show_about: false,
            show_shortcuts: false,
            toasts: Vec::new(),
            pending: None,
            quit_confirmed: false,
            applied_theme: None,
            applied_scale: settings.ui_scale(),
            applied_ui_font: settings.ui_font_size(),
            syntax_theme: SyntaxTheme::for_ui(ResolvedTheme::Dark),
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

    fn active_mut(&mut self) -> Option<&mut OpenDoc> {
        self.active.and_then(|i| self.docs.get_mut(i))
    }

    /// Editor options from settings, with the language left at its default —
    /// callers that have a document fill that in.
    fn editor_options(&self) -> EditorOptions {
        EditorOptions {
            font_size: self.settings.font_size(),
            tab_width: self.settings.tab_width(),
            insert_spaces: self.settings.insert_spaces(),
            show_line_numbers: true,
            language: LanguageId::PlainText,
            auto_close_brackets: self.settings.auto_close_brackets(),
        }
    }

    /// Give the keyboard to the active editor, so an opened or selected
    /// document can be typed into without clicking into it first.
    fn focus_active(&mut self) {
        if let Some(entry) = self.active_mut() {
            entry.view.focus();
        }
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
            self.focus_active();
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
            highlighter: new_highlighter(language, &doc),
            doc,
            view: EditorView::default(),
            language,
            preview,
        };

        // A preview tab replaces the existing one rather than adding to it.
        if preview && let Some(slot) = self.docs.iter().position(|d| d.preview) {
            self.docs[slot] = entry;
            self.active = Some(slot);
        } else {
            self.docs.push(entry);
            self.active = Some(self.docs.len() - 1);
        }
        self.focus_active();
    }

    /// Close a tab, asking first if it has unsaved changes.
    fn close_tab(&mut self, index: usize) {
        if self.docs.get(index).is_some_and(|d| d.doc.is_dirty()) {
            self.pending = Some(Pending::CloseTab(index));
            return;
        }
        self.force_close_tab(index);
    }

    /// Close a tab unconditionally. Only call once unsaved work is resolved.
    fn force_close_tab(&mut self, index: usize) {
        if index >= self.docs.len() {
            return;
        }
        self.docs.remove(index);

        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.docs.len() - 1)),
            None => None,
        };
    }

    /// Indices of every document with unsaved changes.
    fn dirty_indices(&self) -> Vec<usize> {
        self.docs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.doc.is_dirty())
            .map(|(i, _)| i)
            .collect()
    }

    /// Which documents a pending action would discard.
    fn at_risk(&self, pending: Pending) -> Vec<usize> {
        match pending {
            Pending::CloseTab(i) => self
                .docs
                .get(i)
                .filter(|d| d.doc.is_dirty())
                .map(|_| vec![i])
                .unwrap_or_default(),
            Pending::CloseOthers(keep) => self
                .dirty_indices()
                .into_iter()
                .filter(|i| *i != keep)
                .collect(),
            Pending::CloseAll | Pending::Quit => self.dirty_indices(),
        }
    }

    /// Save the given documents. Returns false if any could not be saved, in
    /// which case the destructive action must not proceed.
    fn save_indices(&mut self, indices: &[usize]) -> bool {
        let mut failures = Vec::new();
        for &i in indices {
            let Some(entry) = self.docs.get_mut(i) else {
                continue;
            };
            if entry.doc.path().is_none() {
                // An untitled buffer needs a destination. Rather than opening a
                // file chooser from inside a modal, refuse and let the user do
                // Save As deliberately.
                failures.push(format!("{} has never been saved", entry.doc.display_name()));
                continue;
            }
            if let Err(e) = entry.doc.save() {
                failures.push(format!("{}: {e:#}", entry.doc.display_name()));
            }
        }
        if failures.is_empty() {
            true
        } else {
            self.error(format!("Not closed \u{2014} {}", failures.join("; ")));
            false
        }
    }

    /// Carry out a pending action now that unsaved work has been dealt with.
    fn commit_pending(&mut self, pending: Pending, ctx: &egui::Context) {
        match pending {
            Pending::CloseTab(i) => self.force_close_tab(i),
            Pending::CloseOthers(keep) => {
                if keep < self.docs.len() {
                    let kept = self.docs.remove(keep);
                    self.docs.clear();
                    self.docs.push(kept);
                    self.active = Some(0);
                }
            }
            Pending::CloseAll => {
                self.docs.clear();
                self.active = None;
            }
            Pending::Quit => {
                self.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// The Save / Don't Save / Cancel prompt.
    ///
    /// Deliberately not a plain "are you sure": the third option has to be
    /// *save*, or the only way out of the dialog is to lose the work.
    fn unsaved_prompt(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending else {
            return;
        };
        let at_risk = self.at_risk(pending);

        // Nothing actually unsaved — proceed without bothering the user.
        if at_risk.is_empty() {
            self.pending = None;
            self.commit_pending(pending, ctx);
            return;
        }

        let names: Vec<String> = at_risk
            .iter()
            .filter_map(|i| self.docs.get(*i))
            .map(|d| d.doc.display_name())
            .collect();

        let mut decision = None;

        egui::Modal::new(egui::Id::new("unsaved_changes")).show(ctx, |ui| {
            ui.set_width(420.0);
            if names.len() == 1 {
                ui.heading(format!("Save changes to {}?", names[0]));
            } else {
                ui.heading(format!("Save changes to {} files?", names.len()));
            }
            ui.add_space(6.0);
            ui.label("Your changes will be lost if you don't save them.");

            if names.len() > 1 {
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for name in &names {
                            ui.weak(format!("\u{2022} {name}"));
                        }
                    });
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save = if names.len() == 1 { "Save" } else { "Save All" };
                    if ui.button(save).clicked() {
                        decision = Some(Decision::Save);
                    }
                    if ui.button("Don't Save").clicked() {
                        decision = Some(Decision::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(Decision::Cancel);
                    }
                });
            });
        });

        // Escape is Cancel — the safe option, never the destructive one.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(Decision::Cancel);
        }

        match decision {
            Some(Decision::Save) => {
                self.pending = None;
                if self.save_indices(&at_risk) {
                    self.commit_pending(pending, ctx);
                }
            }
            Some(Decision::Discard) => {
                self.pending = None;
                self.commit_pending(pending, ctx);
            }
            Some(Decision::Cancel) => self.pending = None,
            None => {}
        }
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

    /// Write a file created by the New File dialog and open it.
    fn create_file(&mut self, request: new_file::NewFile) {
        let new_file::NewFile {
            path,
            language,
            contents,
            cursor,
        } = request;

        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error(format!("Could not create {}: {e}", parent.display()));
            return;
        }

        // Normalise to the platform's line endings on the way out; Document
        // will detect and preserve them from here on.
        let eol = editor_core::document::LineEnding::platform_default();
        let text = if eol == editor_core::document::LineEnding::Crlf {
            contents.replace('\n', "\r\n")
        } else {
            contents
        };

        if let Err(e) = std::fs::write(&path, text.as_bytes()) {
            self.error(format!("Could not write {}: {e}", path.display()));
            return;
        }

        tracing::info!(path = %path.display(), "created file");
        self.tree.refresh();
        self.open_path(&path, false);

        if let Some(entry) = self.active_mut() {
            // The dialog's explicit choice wins over extension sniffing, so a
            // Python file named `build.cfg` is still treated as Python.
            entry.language = language;
            entry.highlighter = new_highlighter(language, &entry.doc);
            // Place the caret where the template asked.
            entry.view.set_caret(cursor);
        }
    }

    // ---- commands --------------------------------------------------------

    fn run_command(&mut self, id: CommandId, ctx: &egui::Context) {
        match id {
            CommandId::NewFile => {
                let directory = self
                    .active_doc()
                    .and_then(|d| d.doc.path())
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .or_else(|| self.tree.root().map(Path::to_path_buf))
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                self.new_file.open(directory);
            }
            CommandId::NewScratch => {
                self.docs.push(OpenDoc {
                    doc: Document::untitled(),
                    view: EditorView::default(),
                    language: LanguageId::PlainText,
                    highlighter: None,
                    preview: false,
                });
                self.active = Some(self.docs.len() - 1);
                self.focus_active();
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

            CommandId::Undo => {
                if let Some(entry) = self.active_mut() {
                    let changed = entry.view.undo(&mut entry.doc);
                    if !changed {
                        self.info("Nothing to undo");
                    }
                }
            }
            CommandId::Redo => {
                if let Some(entry) = self.active_mut() {
                    let changed = entry.view.redo(&mut entry.doc);
                    if !changed {
                        self.info("Nothing to redo");
                    }
                }
            }
            CommandId::Copy => {
                if let Some(entry) = self.active_mut()
                    && let Some(text) = entry.view.copy(&entry.doc)
                {
                    ctx.copy_text(text);
                }
            }
            CommandId::Cut => {
                if let Some(entry) = self.active_mut()
                    && let Some(text) = entry.view.cut(&mut entry.doc)
                {
                    ctx.copy_text(text);
                }
            }
            CommandId::Paste => match read_clipboard() {
                Ok(text) => {
                    if let Some(entry) = self.active_mut() {
                        entry.view.paste(&mut entry.doc, &text);
                    }
                }
                Err(e) => self.error(format!("Paste failed: {e}")),
            },
            CommandId::SelectAll => {
                if let Some(entry) = self.active_mut() {
                    entry.view.select_all(&entry.doc);
                }
            }
            CommandId::ToggleComment => {
                let opts = self.editor_options();
                if let Some(entry) = self.active_mut() {
                    let opts = EditorOptions {
                        language: entry.language,
                        ..opts
                    };
                    if !entry.view.toggle_comment(&mut entry.doc, opts) {
                        let language = entry.language.display_name();
                        self.info(format!("{language} has no line comment syntax"));
                    }
                }
            }
            CommandId::Indent | CommandId::Outdent => {
                let levels = if id == CommandId::Indent { 1 } else { -1 };
                let opts = self.editor_options();
                if let Some(entry) = self.active_mut() {
                    let opts = EditorOptions {
                        language: entry.language,
                        ..opts
                    };
                    entry.view.shift_lines(&mut entry.doc, opts, levels);
                }
            }

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

    /// Apply the theme, interface font size and zoom if any has changed since
    /// the last frame.
    fn sync_appearance(&mut self, ctx: &egui::Context) {
        let resolved = ui_theme::resolve(ctx, self.settings.theme());
        let scale = self.settings.ui_scale();
        let font_size = self.settings.ui_font_size();

        let changed = self.applied_theme != Some(resolved)
            || (self.applied_scale - scale).abs() > f32::EPSILON
            || (self.applied_ui_font - font_size).abs() > f32::EPSILON;

        if changed {
            ui_theme::apply(ctx, resolved, scale, font_size);
            // The code pane follows the UI theme. PLAN.md §3.11 allows pinning
            // them apart; the setting for that arrives with the settings UI.
            self.syntax_theme = SyntaxTheme::for_ui(resolved);
            self.applied_theme = Some(resolved);
            self.applied_scale = scale;
            self.applied_ui_font = font_size;
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

        // Panel heights follow the text size rather than being fixed, so
        // raising ui.font_size does not clip the toolbar or status bar.
        let row = ui.text_style_height(&egui::TextStyle::Body);

        egui::Panel::top("toolbar")
            .exact_size(row * 2.2)
            .show(ui, |ui| {
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
            let (line, column) = entry.view.cursor_position(&entry.doc);
            StatusSummary {
                language: entry.language.display_name(),
                encoding: entry.doc.encoding().label(),
                eol: entry.doc.line_ending().label(),
                lines: entry.doc.line_count(),
                read_only: entry.doc.read_only().is_some(),
                line,
                column,
                selected: entry.view.selection_len(),
            }
        });
        let theme_label = self.settings.theme().label();
        let tab_width = self.settings.tab_width();
        let insert_spaces = self.settings.insert_spaces();

        let row = ui.text_style_height(&egui::TextStyle::Body);

        egui::Panel::bottom("status_bar")
            .exact_size(row * 1.6)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    match &summary {
                        Some(s) => {
                            ui.weak(format!("Ln {}, Col {}", s.line, s.column));
                            if s.selected > 0 {
                                ui.weak(format!("({} selected)", s.selected));
                            }
                            ui.separator();
                            ui.weak(s.language);
                            ui.separator();
                            ui.weak(s.encoding);
                            ui.separator();
                            ui.weak(s.eol);
                            ui.separator();
                            ui.weak(format!(
                                "{}: {tab_width}",
                                if insert_spaces { "Spaces" } else { "Tabs" }
                            ));
                            ui.separator();
                            ui.weak(format!("{} lines", s.lines));
                            if s.read_only {
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

            let mut opts = self.editor_options();
            let syntax = &self.syntax_theme;

            if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) {
                opts.language = entry.language;
                // Bring the parse tree up to date before painting from it.
                // Draining the outbox here means every edit path — typing,
                // paste, undo, redo — feeds the highlighter without each one
                // having to remember to.
                let changes = entry.doc.take_changes();
                if let Some(h) = entry.highlighter.as_mut()
                    && !changes.is_empty()
                {
                    h.update(&changes, entry.doc.text());
                }

                // Editing a preview tab promotes it: the file is being worked
                // on, so it must not be replaced by the next explorer click.
                if entry
                    .view
                    .ui(ui, &mut entry.doc, entry.highlighter.as_mut(), syntax, opts)
                {
                    entry.preview = false;
                }
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

        // Never let the window close with unsaved work. This must run before
        // anything else in the frame, and `quit_confirmed` stops the second
        // close request being intercepted again.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            if self.docs.iter().any(|d| d.doc.is_dirty()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            } else {
                self.quit_confirmed = true;
            }
        }

        // One command per frame, from whichever source fired. Keyboard first,
        // so a shortcut is not swallowed by a menu that happens to be open —
        // except while a modal has focus, where keystrokes belong to its
        // fields and its own shortcut must not re-open it.
        let modal_open =
            self.palette.is_open() || self.new_file.is_open() || self.pending.is_some();
        let mut invoked = if modal_open {
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
                tab_bar::Action::Select(i) => {
                    self.active = Some(i);
                    self.focus_active();
                }
                tab_bar::Action::Close(i) => self.close_tab(i),
                tab_bar::Action::CloseOthers(keep) => {
                    self.pending = Some(Pending::CloseOthers(keep));
                }
                tab_bar::Action::CloseAll => {
                    self.pending = Some(Pending::CloseAll);
                }
                tab_bar::Action::None => {}
            }
        }

        invoked = self.palette.ui(&ctx).or(invoked);

        if let Some(request) = self.new_file.ui(&ctx, editor_config::APP_AUTHOR) {
            self.create_file(request);
        }

        self.about_window(&ctx);
        self.shortcuts_window(&ctx);
        self.unsaved_prompt(&ctx);
        self.toasts_ui(&ctx);

        if let Some(id) = invoked {
            tracing::debug!(?id, "command");
            self.run_command(id, &ctx);
        }
    }
}

// ---- free functions ------------------------------------------------------

/// What the status bar displays about the active document.
struct StatusSummary {
    language: &'static str,
    encoding: &'static str,
    eol: &'static str,
    lines: usize,
    read_only: bool,
    line: usize,
    column: usize,
    selected: usize,
}

/// Build a highlighter for a document, or `None` if it should not be
/// highlighted.
///
/// Files past the large-file threshold are deliberately left plain: parsing a
/// multi-megabyte file on every keystroke is exactly the cost the viewport
/// virtualisation exists to avoid, and the status bar already says the file
/// opened read-only.
fn new_highlighter(language: LanguageId, doc: &Document) -> Option<Highlighter> {
    if doc.is_large() {
        return None;
    }
    Highlighter::new(language, doc.text())
}

/// Read the system clipboard, for the Edit → Paste menu item.
///
/// Keyboard paste does not come through here: egui synthesises `Event::Paste`
/// with the text already attached, and the editor widget handles it. This
/// exists only because a menu click carries no such event.
fn read_clipboard() -> Result<String, String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|e| e.to_string())
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
        CommandId::Undo => "\u{21b6}",
        CommandId::Redo => "\u{21b7}",
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
            MenuEntry::Item(CommandId::NewScratch),
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
        "Edit",
        &[
            MenuEntry::Item(CommandId::Undo),
            MenuEntry::Item(CommandId::Redo),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Cut),
            MenuEntry::Item(CommandId::Copy),
            MenuEntry::Item(CommandId::Paste),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::SelectAll),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::ToggleComment),
            MenuEntry::Item(CommandId::Indent),
            MenuEntry::Item(CommandId::Outdent),
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
    &[CommandId::Undo, CommandId::Redo],
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
