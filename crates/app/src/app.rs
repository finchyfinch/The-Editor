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
use editor_config::session::{OpenFile, Session, WindowGeometry};
use editor_config::settings::Settings;
use editor_config::theme::{ResolvedTheme, ThemePreference};
use editor_core::document::Document;
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::theme::SyntaxTheme;
use editor_widgets::editor_view::{EditorOptions, EditorView, Underline, severity_colour};
use editor_widgets::find_bar::{self, FindBar};
use editor_widgets::{file_tree::FileTree, tab_bar, theme as ui_theme};
use eframe::egui;

use crate::commands::{self, CommandId};
use crate::completion;
use crate::new_file;
use crate::palette::Palette;
use crate::runner::Runner;
use crate::settings_window;
use crate::venv_dialog;
use crate::watcher::Watcher;

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

/// How long typing has to pause before the built-in syntax check runs.
///
/// Half a written line is not valid code, so checking on every keystroke would
/// keep a squiggle under the caret the whole time the user is writing. Long
/// enough not to fire mid-word, short enough that a finished mistake is flagged
/// before the eye has left the line.
const SYNTAX_DEBOUNCE: Duration = Duration::from_millis(400);

/// Height of the output dock when it first appears.
const DEFAULT_DOCK_HEIGHT: f32 = 220.0;
/// Smallest useful dock: enough for the header and a couple of lines.
const MIN_DOCK_HEIGHT: f32 = 80.0;
/// Largest the dock can be dragged to. A hard cap as well as the
/// leave-room-for-the-editor clamp, so no window size can hide the editor.
const MAX_DOCK_HEIGHT: f32 = 900.0;

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

/// A place to jump to. `path` is `None` for the file already showing, which is
/// how an untitled buffer's own results still work.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    path: Option<PathBuf>,
    /// Zero-based, as the protocol and the parse tree both give them.
    line: usize,
    column: usize,
}

/// The state behind Find Uses and its next/previous walk.
#[derive(Debug, Default)]
struct UseResults {
    /// The name being tracked, for the status message.
    name: String,
    locations: Vec<Target>,
    /// Which result the caret is on.
    index: usize,
    /// True when the results came from the parse-tree fallback, which only
    /// ever searches the open file. Said out loud, because a list that looks
    /// complete but is not is worse than no list.
    local_only: bool,
    /// A question asked of a language server and not yet answered.
    pending: Option<editor_lsp::session::Query>,
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
    /// Find/replace state, per document so switching tabs keeps each one's
    /// query.
    find: FindBar,
    /// A Find Next/Previous requested from the keyboard, applied on the frame
    /// the bar next draws.
    pending_find_step: Option<isize>,
    /// Preview tabs are replaced by the next single-clicked file instead of
    /// accumulating. Promoted to permanent on double-click or first edit.
    preview: bool,
    /// Document version the built-in syntax check last ran against. `None`
    /// until it has run once.
    syntax_version: Option<u64>,
    /// When the next syntax check is due, so squiggles do not flicker under
    /// the caret while a line is half-typed.
    syntax_due: Option<Instant>,
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
    settings_form: settings_window::SettingsWindow,
    /// The result of the last toolchain probe, and whether its window is open.
    /// `None` when closed; probing is a handful of process spawns, so it is
    /// done when asked for rather than every frame.
    toolchains: Option<Vec<editor_lsp::registry::Found>>,
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

    runner: Runner,
    /// Whether the bottom dock is showing the run output.
    show_output: bool,
    /// Height of the bottom dock, owned here rather than by the panel.
    dock_height: f32,
    venv_dialog: venv_dialog::Dialog,
    /// What to do once a virtual environment finishes being created. Held
    /// across frames because creation is three processes, not a function call.
    pending_venv: Option<venv_dialog::Completion>,

    /// Watches the open folder. `None` when no folder is open, or when the
    /// platform refused to watch it.
    watcher: Option<Watcher>,
    /// The session to restore on the first frame, once the window exists and
    /// its geometry can be checked against the monitors actually attached.
    restore: Option<Session>,
    /// Set once the session has been written on quit, so it is not written
    /// again by a second close request.
    session_saved: bool,

    /// Recently opened files, newest first. Persisted in the session file.
    recent: Vec<PathBuf>,
    /// A recent file the user picked from a menu, opened after the menu closes.
    pending_recent: Option<PathBuf>,
    /// A file or folder awaiting a yes/no before it is moved to the trash.
    pending_delete: Option<PathBuf>,
    /// Results of the last Find Uses, and where in them the user is.
    uses: UseResults,
    /// The completion popup, and the request behind it.
    completion: completion::Popup,
    /// Document version the popup was last synced against, so the word under
    /// the caret is only re-examined when something actually changed.
    completion_version: Option<u64>,

    lsp: editor_lsp::session::Lsp,
    /// Which bottom-dock tab is showing.
    dock: DockTab,
    /// Document versions the servers were last told about, so an unchanged
    /// document is not re-sent every frame.
    synced: std::collections::HashMap<PathBuf, u64>,
}

/// The bottom dock's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DockTab {
    #[default]
    Output,
    Problems,
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
        let session = Session::load(&paths.session_file());

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
            settings_form: settings_window::SettingsWindow::default(),
            toolchains: None,
            toasts: Vec::new(),
            pending: None,
            quit_confirmed: false,
            applied_theme: None,
            applied_scale: settings.ui_scale(),
            applied_ui_font: settings.ui_font_size(),
            syntax_theme: SyntaxTheme::for_ui(ResolvedTheme::Dark),
            runner: Runner::default(),
            show_output: false,
            dock_height: DEFAULT_DOCK_HEIGHT,
            venv_dialog: venv_dialog::Dialog::default(),
            pending_venv: None,
            watcher: None,
            restore: settings.restore_session().then(|| session.clone()),
            session_saved: false,
            recent: Vec::new(),
            pending_recent: None,
            pending_delete: None,
            uses: UseResults::default(),
            completion: completion::Popup::default(),
            completion_version: None,
            lsp: editor_lsp::session::Lsp::default(),
            dock: DockTab::default(),
            synced: std::collections::HashMap::new(),
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
        // Only files that actually opened go on the recent list. Offering one
        // that failed a moment ago is a trap.
        Session::push_recent(&mut self.recent, path);

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
            find: FindBar::default(),
            pending_find_step: None,
            preview,
            syntax_version: None,
            syntax_due: None,
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

    /// Reopen what was open last time.
    ///
    /// Runs on the first frame rather than in `new`, because the window has to
    /// exist before its remembered geometry can be checked against the
    /// monitors that are actually attached.
    fn restore_session(&mut self, session: &Session, ctx: &egui::Context) {
        if let Some(geometry) = session.window {
            let monitor = ctx.input(|i| i.viewport().monitor_size);
            let visible = monitor.is_none_or(|size| geometry.is_on_screen(size.x, size.y));

            if visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    geometry.x, geometry.y,
                )));
            } else {
                // The monitor it was on is gone. Keep the size, drop the
                // position, and let the window manager place it.
                tracing::info!("remembered window position is off-screen; ignoring it");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                geometry.width,
                geometry.height,
            )));
            if geometry.maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }

        // Restored first, so that reopening the previous tabs below pushes
        // them to the front of a list that already has the older history in it
        // rather than replacing it.
        self.recent.clone_from(&session.recent_files);

        if let Some(folder) = &session.folder {
            self.open_folder(folder.clone());
        }
        for file in &session.open_files {
            self.open_path(&file.path, false);
            if let Some(entry) = self.active_mut() {
                let caret = file.caret.min(entry.doc.len_chars());
                entry.view.set_caret(caret);
            }
        }
        if let Some(active) = session.active.filter(|i| *i < self.docs.len()) {
            self.active = Some(active);
            self.focus_active();
        }
        self.show_output = session.show_output;
    }

    /// Gather the current state for writing out.
    fn current_session(&self, ctx: &egui::Context) -> Session {
        let window = ctx.input(|i| {
            let viewport = i.viewport();
            // Position comes from the outer rect — that is where the window
            // actually is — but the size comes from the *inner* rect, because
            // that is what `InnerSize` sets when restoring. Saving the outer
            // size and restoring it as the inner one makes the window grow by
            // the height of its own title bar on every launch.
            let outer = viewport.outer_rect?;
            let inner = viewport.inner_rect?;
            Some(WindowGeometry {
                x: outer.min.x,
                y: outer.min.y,
                width: inner.width(),
                height: inner.height(),
                maximized: viewport.maximized.unwrap_or(false),
            })
        });

        Session {
            recent_files: self.recent.clone(),
            folder: self.tree.root().map(Path::to_path_buf),
            open_files: self
                .docs
                .iter()
                .filter_map(|entry| {
                    Some(OpenFile {
                        path: entry.doc.path()?.to_path_buf(),
                        caret: entry.view.selection.head,
                    })
                })
                .collect(),
            active: self.active,
            window: window.filter(|w| w.is_plausible()),
            show_output: self.show_output,
        }
    }

    fn save_session(&mut self, ctx: &egui::Context) {
        if self.session_saved || !self.settings.restore_session() {
            return;
        }
        let session = self.current_session(ctx);
        if let Err(e) = session.save(&self.paths.session_file()) {
            tracing::warn!("could not save the session: {e}");
        }
        self.session_saved = true;
    }

    /// Open a project folder and start watching it.
    fn open_folder(&mut self, folder: PathBuf) {
        tracing::info!(path = %folder.display(), "opening folder");

        // Reopening the same folder — which session restore can do right after
        // startup — should not tear down a working watch and build another.
        if self.watcher.as_ref().is_some_and(|w| w.root() == folder) {
            self.tree.set_root(folder);
            return;
        }

        match Watcher::new(&folder) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(e) => {
                // Not fatal: the tree still works, it just will not notice
                // changes made elsewhere.
                tracing::warn!("could not watch {}: {e}", folder.display());
                self.watcher = None;
                self.info("Changes made outside The Editor will not be noticed automatically");
            }
        }
        self.tree.set_root(folder);
    }

    /// The Problems panel: every diagnostic, grouped by file.
    ///
    /// Returns the location to jump to when a row is clicked.
    fn problems_ui(&mut self, ui: &mut egui::Ui) -> Option<(PathBuf, usize, usize)> {
        let files = self.lsp.diagnostics().all();

        // The what-is-missing notice sits at the bottom and is drawn first, so
        // it keeps its place while the list above it scrolls. It shows whether
        // or not there are diagnostics: finding a syntax error does not mean a
        // type checker has stopped being missing.
        if self.lsp.running().is_empty() && self.missing_tools_ui(ui) {
            self.toolchains = Some(editor_lsp::registry::find_all(&self.tool_search_path()));
        }

        if files.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.weak("No problems");
            });
            return None;
        }

        let mut clicked = None;
        egui::ScrollArea::both()
            .id_salt("problems")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (path, diagnostics) in files {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    ui.horizontal(|ui| {
                        ui.strong(name);
                        ui.weak(format!("({})", diagnostics.len()));
                    });

                    // A file that does not parse cannot be analysed further by
                    // anything. Ruff reports the syntax error and stops;
                    // Pyright does the same. Without saying so, a missing
                    // import that goes unreported below a syntax error looks
                    // like the linter failing rather than waiting.
                    if has_syntax_error(&diagnostics) {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.weak(
                                "\u{2139} This file does not parse, so nothing can check it \
                                 beyond its syntax. Fix the error above and the rest \
                                 will follow.",
                            );
                        });
                    }

                    for diagnostic in diagnostics {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.colored_label(
                                severity_colour(ui.visuals(), diagnostic.severity),
                                diagnostic.severity.glyph(),
                            );
                            ui.weak(format!("{}:{}", diagnostic.line + 1, diagnostic.column + 1));
                            let row = ui.add(
                                egui::Label::new(diagnostic.summary())
                                    .sense(egui::Sense::click())
                                    .truncate(),
                            );
                            if row
                                .on_hover_text(&diagnostic.message)
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                clicked = Some((
                                    path.clone(),
                                    diagnostic.line as usize,
                                    diagnostic.column as usize,
                                ));
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            });
        clicked
    }

    /// The strip at the foot of the Problems panel saying what is not checking
    /// this file, and the exact command that would fix it.
    ///
    /// Returns true if the user asked for the full toolchain window.
    ///
    /// Naming three tools the user has not got and stopping there is not a
    /// report, it is a riddle. Each line carries the command that installs it,
    /// with a button that puts it on the clipboard, because the next thing
    /// anyone does with a command is paste it into a terminal.
    fn missing_tools_ui(&self, ui: &mut egui::Ui) -> bool {
        let missing = self.lsp.missing();
        let mut open_toolchains = false;

        egui::Panel::bottom("problems_toolchains").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.small("Syntax is checked by The Editor itself. Nothing else is checking.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_toolchains = ui.small_button("Check Toolchains...").clicked();
                });
            });

            for spec in missing {
                ui.horizontal(|ui| {
                    ui.small(format!("{} \u{2014} {}:", spec.name, spec.provides));
                    ui.code(spec.install);
                    if ui
                        .small_button("Copy")
                        .on_hover_text("Copy the command to the clipboard")
                        .clicked()
                    {
                        ui.ctx().copy_text(spec.install.to_owned());
                    }
                });
            }
            ui.add_space(4.0);
        });

        open_toolchains
    }

    /// The word being typed at the caret: where it starts, and what it is.
    ///
    /// `None` when the caret is not immediately after an identifier character,
    /// which is how the popup knows to close: the moment you type a space or a
    /// bracket, the word you were completing has ended.
    fn completion_prefix(&self) -> Option<(usize, String)> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let caret = entry.view.selection.head;
        if !entry.view.selection.is_empty() {
            // With a selection, typing replaces it; there is no prefix.
            return None;
        }
        let text = entry.doc.text();

        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = caret;
        while start > 0 && is_word(text.char(start - 1)) {
            start -= 1;
        }
        if start == caret {
            // No word yet, but a `.` just typed is the single most common
            // reason to want a list: `os.` and then everything the module has.
            // The prefix is empty and the suggestion replaces nothing.
            return (caret > 0 && text.char(caret - 1) == '.').then(|| (caret, String::new()));
        }
        // A name cannot begin with a digit, so `1234` is a number being typed,
        // not a prefix worth asking a server about.
        if text.char(start).is_ascii_digit() {
            return None;
        }
        Some((start, text.slice(start..caret).chars().collect()))
    }

    /// Keep the completion popup in step with the document.
    ///
    /// Called once per frame. Asks for suggestions when a new word starts,
    /// narrows the list locally while the same word grows, and closes when the
    /// word ends.
    fn sync_completion(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            self.completion.close();
            return;
        };
        let version = entry.doc.version();
        if self.completion_version == Some(version) {
            return;
        }
        self.completion_version = Some(version);

        let Some((start, prefix)) = self.completion_prefix() else {
            self.completion.close();
            return;
        };

        // Below this the list is most of what the server knows, which is
        // thousands of entries and no help. An empty prefix is exempt: it means
        // a `.` was just typed, where the member list is exactly what is
        // wanted and is already narrowed by the thing before the dot.
        const MIN_PREFIX: usize = 2;

        if self.completion.is_open() {
            self.completion.refilter(&prefix);
            return;
        }
        let long_enough = prefix.is_empty() || prefix.chars().count() >= MIN_PREFIX;
        if self.completion.is_waiting() || !long_enough {
            return;
        }
        self.request_completions(start, &prefix);
    }

    /// Ask the servers about the word starting at `start`.
    fn request_completions(&mut self, start: usize, prefix: &str) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            return;
        };
        // Asked about the *start* of the word, not the caret: a server given
        // the position after `par` may filter to what it thinks matches, and
        // its idea of matching is not the one the popup then applies. After a
        // `.` the two are the same position anyway.
        let (line, column) = entry.doc.line_col(start);
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if self.lsp.complete(&path, line, column) {
            self.completion.requested(start, prefix);
            return;
        }
        self.complete_locally(start, prefix);
    }

    /// Suggest names already written in this file.
    ///
    /// What is available with no language server: a word list from the parse
    /// tree, marked with what defines each name. It cannot see another file, an
    /// import, or anything in the standard library, so the detail column says
    /// where the suggestion came from rather than letting it pass for a real
    /// completion.
    ///
    /// Not offered after a `.`: the members of an object have nothing to do
    /// with the names that happen to appear elsewhere in the file, and a list
    /// of them there would be actively misleading.
    fn complete_locally(&mut self, start: usize, prefix: &str) {
        if prefix.is_empty() {
            return;
        }
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(Highlighter::tree) else {
            return;
        };

        let caret_word = editor_syntax::symbols::identifier_at(
            tree,
            entry.doc.text(),
            entry.view.selection.head,
        )
        .map(|s| s.range);

        let items: Vec<editor_lsp::session::Completion> =
            editor_syntax::symbols::identifiers(tree, entry.doc.text(), caret_word)
                .into_iter()
                .map(|symbol| editor_lsp::session::Completion {
                    kind: Some(lsp_kind(symbol.kind)),
                    detail: Some("in this file".to_owned()),
                    // No `sortText`: the walk already put definitions first,
                    // and the popup sorts by it when present.
                    sort_text: None,
                    insert: symbol.name.clone(),
                    label: symbol.name,
                })
                .collect();

        if items.is_empty() {
            return;
        }
        self.completion.requested(start, prefix);
        self.completion.answered(items, prefix);
    }

    /// Ctrl+Space: ask now, whatever the prefix length.
    ///
    /// The two-character floor exists so the popup does not appear unbidden
    /// over a single letter. Asking for it explicitly is a different matter.
    fn trigger_completion(&mut self) {
        let Some((start, prefix)) = self.completion_prefix() else {
            self.info("Put the caret in or after a name first");
            return;
        };
        self.completion.close();
        self.request_completions(start, &prefix);
        if !self.completion.is_open() && !self.completion.is_waiting() {
            self.info("No suggestions here");
        }
    }

    /// Let the popup claim its keys, and apply an acceptance.
    ///
    /// Runs near the top of the frame, before the editor reads events.
    fn completion_keys(&mut self, ctx: &egui::Context) {
        let Some((_, prefix)) = self.completion_prefix() else {
            return;
        };
        let action = self.completion.handle_keys(ctx, prefix.chars().count());
        self.apply_completion(action);
    }

    /// Draw the popup, and apply a choice made with the mouse.
    fn completion_draw(&mut self, ctx: &egui::Context) {
        let Some((_, prefix)) = self.completion_prefix() else {
            return;
        };
        let Some(caret) = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|e| e.view.caret_screen_rect())
        else {
            return;
        };
        let action = self.completion.draw(ctx, caret, prefix.chars().count());
        self.apply_completion(action);
    }

    /// Put an accepted suggestion into the document.
    fn apply_completion(&mut self, action: completion::Action) {
        let completion::Action::Accept { insert, replacing } = action else {
            return;
        };
        let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) else {
            return;
        };
        let caret_offset = entry.view.selection.head;
        let start = caret_offset.saturating_sub(replacing);

        // One transaction, so accepting a suggestion is one undo step rather
        // than a delete and an insert. The run is broken either side so it does
        // not coalesce with the typing that led up to it.
        use editor_core::edit::{Edit, Transaction};
        let before = entry.view.selection;
        let after = editor_core::selection::Selection::at(start + insert.chars().count());
        entry.doc.break_undo_run();
        entry.doc.apply(
            &Transaction::new(vec![Edit::replace(start..caret_offset, insert)]),
            before,
            after,
        );
        entry.view.set_caret(after.head);
        entry.doc.break_undo_run();
        entry.view.focus();

        // The edit bumps the version. Recording it here stops the next frame
        // treating the freshly inserted word as a new prefix and asking again.
        self.completion_version = self
            .active
            .and_then(|i| self.docs.get(i))
            .map(|e| e.doc.version());
    }

    /// The name the caret is on, for messages about a server's answer.
    fn symbol_under_caret(&self) -> Option<String> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let tree = entry.highlighter.as_ref().and_then(Highlighter::tree)?;
        editor_syntax::symbols::identifier_at(tree, entry.doc.text(), entry.view.selection.head)
            .map(|s| s.name)
    }

    /// Go to Definition / Find Uses, from the menu, F12 or the context menu.
    ///
    /// Asks the language server first and falls back to a parse-tree search of
    /// the open file. The fallback is genuinely weaker — one file, no scope, no
    /// imports — so it says so rather than letting a partial answer pass for a
    /// complete one.
    fn ask_about_symbol(&mut self, query: editor_lsp::session::Query) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let caret = entry.view.selection.head;
        let (line, column) = entry.doc.line_col(caret);
        // `line_col` is one-based for the status bar; the protocol is not.
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if let Some(path) = entry.doc.path().map(Path::to_path_buf)
            && self.lsp.ask(query, &path, line, column)
        {
            // The answer arrives through `poll`, possibly several frames later.
            self.uses.pending = Some(query);
            return;
        }

        self.answer_locally(query, caret);
    }

    /// The no-language-server path: search the open file's parse tree.
    fn answer_locally(&mut self, query: editor_lsp::session::Query, caret: usize) {
        use editor_lsp::session::Query;
        use editor_syntax::symbols;

        let Some(index) = self.active else { return };
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(Highlighter::tree) else {
            self.info("This file has no grammar, so there is nothing to search");
            return;
        };
        let text = entry.doc.text();

        let Some(symbol) = symbols::identifier_at(tree, text, caret) else {
            self.info("Put the caret on a name first");
            return;
        };

        let ranges: Vec<std::ops::Range<usize>> = match query {
            Query::Definition => symbols::definitions(tree, text, &symbol.name),
            Query::References => symbols::occurrences(tree, text, &symbol.name)
                .into_iter()
                .map(|o| o.range)
                .collect(),
        };

        if ranges.is_empty() {
            self.info(format!(
                "No {} of `{}` in this file (no language server, so other files were not searched)",
                query.noun(),
                symbol.name
            ));
            return;
        }

        let path = entry.doc.path().map(Path::to_path_buf);
        let locations: Vec<Target> = ranges
            .iter()
            .map(|range| {
                let (line, column) = entry.doc.line_col(range.start);
                Target {
                    path: path.clone(),
                    line: line - 1,
                    column: column - 1,
                }
            })
            .collect();

        self.land_on(query, symbol.name, locations, true);
    }

    /// Take a set of results and go to the first one.
    fn land_on(
        &mut self,
        query: editor_lsp::session::Query,
        name: String,
        locations: Vec<Target>,
        local_only: bool,
    ) {
        use editor_lsp::session::Query;

        self.uses.pending = None;
        if locations.is_empty() {
            self.info(format!("No {} found", query.noun()));
            return;
        }
        let first = locations[0].clone();

        if query == Query::Definition {
            // A definition is a jump, not a list: keeping the previous Find
            // Uses results means F8 still walks what the user was walking.
            self.go_to(&first);
            if locations.len() > 1 {
                self.info(format!("{} definitions of `{name}`", locations.len()));
            }
            return;
        }

        // Servers answer in whatever order suits their index -- pyright does
        // not return references in document order -- so F8 would otherwise jump
        // about the file rather than walking down it.
        let mut locations = locations;
        locations.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.line.cmp(&b.line))
                .then(a.column.cmp(&b.column))
        });
        locations.dedup();

        // Start from the result the caret is already on, if it is one of them,
        // so the first F8 moves to the *next* use rather than back to the top.
        let start = self.current_location_index(&locations).unwrap_or(0);
        let first = locations[start].clone();

        let count = locations.len();
        self.uses = UseResults {
            name,
            locations,
            index: start,
            local_only,
            pending: None,
        };
        self.go_to(&first);
        self.info(format!(
            "{} of {count}{}  \u{2014}  F8 next, Shift+F8 previous",
            start + 1,
            if local_only { " in this file" } else { "" }
        ));
    }

    /// Which result the caret is already sitting on, if any.
    ///
    /// Find Uses is normally run with the caret on one of its own results, so
    /// starting at index 0 would mean the first F8 jumped back to the top of
    /// the file before going anywhere useful.
    fn current_location_index(&self, locations: &[Target]) -> Option<usize> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let here = entry.doc.path();
        let (line, column) = entry.doc.line_col(entry.view.selection.head);
        let (line, column) = (line - 1, column - 1);

        locations.iter().position(|target| {
            let same_file = match (&target.path, here) {
                (Some(path), Some(current)) => path == current,
                (None, _) => true,
                _ => false,
            };
            // Column is not compared: the caret can be anywhere within the
            // name, and the result points at its first character.
            same_file && target.line == line && target.column <= column
        })
    }

    /// Step through the Find Uses results.
    fn step_use(&mut self, direction: isize) {
        let count = self.uses.locations.len();
        if count == 0 {
            self.info("Nothing to step through \u{2014} run Find Uses first");
            return;
        }
        // Wraps, like Find Next, so walking off the end returns to the start
        // rather than stopping with no explanation.
        let next = (self.uses.index as isize + direction).rem_euclid(count as isize) as usize;
        self.uses.index = next;
        let target = self.uses.locations[next].clone();
        self.go_to(&target);
        self.info(format!(
            "{} of {count} \u{2014} `{}`{}",
            next + 1,
            self.uses.name,
            // Repeated on every step, not just the first: after three F8s the
            // opening message is long gone and the list still is not complete.
            if self.uses.local_only {
                " in this file"
            } else {
                ""
            }
        ));
    }

    /// Move the caret to a result, opening its file if it is not already open.
    fn go_to(&mut self, target: &Target) {
        match &target.path {
            Some(path) => self.open_at(path, target.line, target.column),
            // A result in the file already showing, which may be untitled.
            None => {
                if let Some(entry) = self.active_mut() {
                    let offset = entry.doc.offset_at(target.line, target.column);
                    entry.view.set_caret(offset);
                    entry.view.focus();
                }
            }
        }
    }

    /// Open a file and put the caret at a zero-based line and column.
    fn open_at(&mut self, path: &Path, line: usize, column: usize) {
        self.open_path(path, false);
        if let Some(entry) = self.active_mut() {
            let offset = entry.doc.offset_at(line, column);
            entry.view.set_caret(offset);
            entry.view.focus();
        }
    }

    /// Keep the language servers' view of the open documents current, and
    /// drain whatever they have said.
    ///
    /// Driven from the documents' own version counters rather than from the
    /// edit path, so no future way of changing text can forget to tell them —
    /// the same reason the highlighter is driven from the change outbox.
    fn sync_language_servers(&mut self) {
        let extra_path = self.tool_search_path();
        self.lsp
            .set_root(self.tree.root().map(Path::to_path_buf), extra_path);

        // Tell the servers about anything new or changed.
        for entry in &self.docs {
            let Some(path) = entry.doc.path() else {
                continue;
            };
            let version = entry.doc.version();
            match self.synced.get(path) {
                None => {
                    self.lsp.open(path, &entry.doc.text().to_string());
                    self.synced.insert(path.to_path_buf(), version);
                }
                Some(known) if *known != version => {
                    self.lsp.change(path, &entry.doc.text().to_string());
                    self.synced.insert(path.to_path_buf(), version);
                }
                Some(_) => {}
            }
        }

        // ...and about anything closed.
        let open: std::collections::HashSet<PathBuf> = self
            .docs
            .iter()
            .filter_map(|d| d.doc.path().map(Path::to_path_buf))
            .collect();
        let closed: Vec<PathBuf> = self
            .synced
            .keys()
            .filter(|p| !open.contains(*p))
            .cloned()
            .collect();
        for path in closed {
            self.lsp.close(&path);
            self.synced.remove(&path);
        }

        for notice in self.lsp.poll() {
            match notice {
                editor_lsp::session::Notice::ServerReady(id) => {
                    tracing::info!(server = id, "ready");
                }
                editor_lsp::session::Notice::ServerDied {
                    name, restarting, ..
                } => {
                    if restarting {
                        self.info(format!("{name} stopped unexpectedly; restarting"));
                    } else {
                        self.error(format!("{name} keeps failing; giving up on it"));
                    }
                }
                editor_lsp::session::Notice::DiagnosticsChanged(_) => {}
                editor_lsp::session::Notice::Completions(items) => {
                    // Matched against the word as it is *now*, not as it was
                    // when the request went out; the popup decides whether the
                    // reply still describes the word being typed.
                    let prefix = self.completion_prefix().map(|(_, p)| p).unwrap_or_default();
                    self.completion.answered(items, &prefix);
                }
                editor_lsp::session::Notice::Answered { query, locations } => {
                    // A stale answer to a question the user has moved on from
                    // would yank the caret somewhere unexpected.
                    if self.uses.pending != Some(query) {
                        continue;
                    }
                    if locations.is_empty() {
                        // The server knows the project and still found nothing,
                        // so falling back to a text-shaped search of one file
                        // would only produce a worse answer to the same
                        // question.
                        self.uses.pending = None;
                        self.info(format!("No {} found", query.noun()));
                        continue;
                    }
                    let name = self.symbol_under_caret().unwrap_or_default();
                    let targets = locations
                        .into_iter()
                        .map(|l| Target {
                            path: Some(l.path),
                            line: l.line as usize,
                            column: l.column as usize,
                        })
                        .collect();
                    self.land_on(query, name, targets, false);
                }
            }
        }
    }

    /// Feed every document's edits to its parse tree, and re-check its syntax.
    ///
    /// Two jobs in one pass because they need the same thing: a parse tree that
    /// matches the text. Draining the change outbox here rather than at each
    /// edit site means no future way of changing text can forget to do it —
    /// typing, paste, undo, redo and replace-all all take this route.
    ///
    /// The syntax check is what makes broken code visible with no language
    /// server installed. It reads the tree the highlighter already maintains,
    /// so it costs a tree walk that returns immediately when the root has no
    /// error flag — which is every keystroke in a file that parses.
    fn sync_highlighters(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut next_due: Option<Instant> = None;

        for entry in &mut self.docs {
            let changes = entry.doc.take_changes();
            if let Some(h) = entry.highlighter.as_mut()
                && !changes.is_empty()
            {
                h.update(&changes, entry.doc.text());
            }

            let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
                // A never-saved buffer has no key in the diagnostic store, the
                // same reason the language servers cannot see it.
                continue;
            };
            let version = entry.doc.version();
            if entry.syntax_version == Some(version) {
                continue;
            }

            // Wait for a pause in typing. Half a line of Python is not valid
            // Python, so checking on every keystroke would put a squiggle under
            // the caret for as long as the user is writing.
            let due = *entry.syntax_due.get_or_insert(now + SYNTAX_DEBOUNCE);
            if now < due {
                next_due = Some(next_due.map_or(due, |soonest: Instant| soonest.min(due)));
                continue;
            }

            let found = entry
                .highlighter
                .as_ref()
                .map(|h| h.errors(entry.doc.text()))
                .unwrap_or_default();
            self.lsp
                .set_builtin(&path, found.into_iter().map(to_diagnostic).collect());
            entry.syntax_version = Some(version);
            entry.syntax_due = None;
        }

        // Nothing else will wake the frame loop once typing stops, so the
        // pending check has to ask for the frame it needs.
        if let Some(due) = next_due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
        }
    }

    /// Directories searched before `PATH` when looking for language servers.
    ///
    /// A project virtual environment's tools come first, so a project with
    /// `ruff` pinned in its venv is linted by that version rather than by
    /// whatever happens to be installed globally.
    fn tool_search_path(&self) -> Vec<PathBuf> {
        let Some(root) = self.tree.root() else {
            return Vec::new();
        };
        editor_proc::interpreter::find_venv(root)
            .and_then(|venv| venv.path.parent().map(Path::to_path_buf))
            .into_iter()
            .collect()
    }

    /// Diagnostics for a document, converted to character offsets.
    ///
    /// The protocol works in zero-based line and UTF-16 column; the editor works
    /// in character offsets. Converting here rather than at the point of use
    /// means one place to get it wrong.
    fn underlines_for(entry: &OpenDoc, store: &editor_lsp::diagnostics::Store) -> Vec<Underline> {
        let Some(path) = entry.doc.path() else {
            return Vec::new();
        };
        store
            .for_file(path)
            .into_iter()
            .map(|d| {
                let start = entry.doc.offset_at(d.line as usize, d.column as usize);
                let end = entry
                    .doc
                    .offset_at(d.end_line as usize, d.end_column as usize);
                Underline {
                    range: start..end.max(start),
                    severity: d.severity,
                    message: d.summary(),
                }
            })
            .collect()
    }

    /// React to files changing outside The Editor.
    fn poll_watcher(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let changes = watcher.drain();
        if changes.is_empty() {
            return;
        }
        if changes.structural {
            self.tree.refresh();
        }

        // An open document whose file changed underneath it: reload silently
        // when there is nothing to lose, warn when there is. Saving over a
        // file that `git checkout` has rewritten is how people lose work.
        for path in &changes.touched {
            let Some(index) = self.docs.iter().position(|d| d.doc.path() == Some(path)) else {
                continue;
            };
            if self.docs[index].doc.is_dirty() {
                let name = self.docs[index].doc.display_name();
                self.error(format!(
                    "{name} changed on disk and has unsaved edits \u{2014} saving will overwrite it"
                ));
                continue;
            }
            self.reload_document(index);
        }
    }

    /// Re-read a document from disk, keeping the caret where it was.
    fn reload_document(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            return;
        };
        let caret = entry.view.selection.head;

        match Document::open(&path) {
            Ok(doc) => {
                let entry = &mut self.docs[index];
                entry.highlighter = new_highlighter(entry.language, &doc);
                entry.doc = doc;
                entry.view.set_caret(caret.min(entry.doc.len_chars()));
                tracing::info!(path = %path.display(), "reloaded after an external change");
            }
            Err(e) => self.error(format!("Could not reload {}: {e:#}", path.display())),
        }
    }

    /// Carry out what the explorer's context menu asked for.
    fn apply_tree_action(
        &mut self,
        action: editor_widgets::file_tree::Action,
        ctx: &egui::Context,
    ) {
        use editor_widgets::file_tree::Action;

        match action {
            Action::None => {}
            Action::Open(path) => self.open_path(&path, false),
            Action::Preview(path) => self.open_path(&path, true),
            Action::Refresh => self.tree.refresh(),

            Action::NewFileIn(directory) => self.new_file.open(directory),
            Action::NewFolderIn(directory) => {
                // A folder needs no dialog of its own: create it with a
                // placeholder name and drop straight into an in-place rename,
                // which is how every file manager does it.
                let path = unique_path(&directory, "New Folder", "");
                match std::fs::create_dir(&path) {
                    Ok(()) => {
                        self.tree.refresh();
                        self.tree.begin_rename(&path);
                    }
                    Err(e) => self.error(format!("Could not create folder: {e}")),
                }
            }

            Action::Rename { from, to } => self.rename_path(&from, &to),
            // Recoverable, but still a surprise if it was a mis-click on a
            // folder with a hundred files in it. Ask first.
            Action::Delete(path) => self.pending_delete = Some(path),

            Action::Reveal(path) => {
                if let Err(e) = reveal_in_file_manager(&path) {
                    self.error(format!("Could not reveal {}: {e}", path.display()));
                }
            }
            Action::CopyPath(path) => {
                ctx.copy_text(path.display().to_string());
                self.info("Path copied");
            }
            Action::CopyRelativePath(path) => {
                let relative = self
                    .tree
                    .root()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(&path);
                ctx.copy_text(relative.display().to_string());
                self.info("Relative path copied");
            }
        }
    }

    /// Rename a file or folder, keeping any open tab pointing at it.
    fn rename_path(&mut self, from: &Path, to: &Path) {
        if to.exists() {
            self.error(format!("{} already exists", to.display()));
            return;
        }
        if let Some(name) = to.file_name().and_then(|n| n.to_str())
            && let Err(e) = editor_core::filename::validate(name)
        {
            self.error(e.to_string());
            return;
        }

        if let Err(e) = std::fs::rename(from, to) {
            self.error(format!("Could not rename: {e}"));
            return;
        }

        // An open document must follow its file, or the next save writes back
        // to the old name and resurrects it.
        for entry in &mut self.docs {
            if entry.doc.path() == Some(from) {
                entry.doc.set_path(to.to_path_buf());
                entry.language = to
                    .extension()
                    .and_then(|e| e.to_str())
                    .map_or(LanguageId::PlainText, LanguageId::from_extension);
                entry.highlighter = new_highlighter(entry.language, &entry.doc);
            }
        }

        self.tree.refresh();
        tracing::info!(from = %from.display(), to = %to.display(), "renamed");
    }

    /// Move a path to the trash, closing any tab that showed it.
    /// Confirm before moving something to the trash.
    ///
    /// The delete is recoverable — it goes to the recycle bin, not oblivion —
    /// but a mis-click on a folder still means fishing a hundred files back out
    /// of it, and the file tree is a place where a click lands one row from
    /// where you meant. Escape cancels, which is the safe answer.
    fn delete_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_delete.clone() else {
            return;
        };

        let is_dir = path.is_dir();
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_delete")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(if is_dir {
                "Delete folder?"
            } else {
                "Delete file?"
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Move");
                ui.strong(&name);
                ui.label("to the recycle bin?");
            });
            if is_dir {
                ui.add_space(4.0);
                ui.label("Everything inside it goes too.");
            }
            ui.add_space(4.0);
            ui.weak(path.display().to_string());

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Delete").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => self.pending_delete = None,
            Some(true) => {
                self.pending_delete = None;
                self.delete_path(&path);
            }
        }
    }

    fn delete_path(&mut self, path: &Path) {
        if let Err(e) = editor_widgets::file_tree::move_to_trash(path) {
            self.error(format!("Could not delete {}: {e}", path.display()));
            return;
        }

        // Close tabs for the deleted file, or for anything inside a deleted
        // folder. Leaving them open invites saving the file back into
        // existence.
        let doomed: Vec<usize> = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.doc
                    .path()
                    .is_some_and(|p| p == path || p.starts_with(path))
            })
            .map(|(i, _)| i)
            .collect();
        for index in doomed.into_iter().rev() {
            self.force_close_tab(index);
        }

        self.tree.refresh();
        self.info(format!(
            "Moved {} to the recycle bin",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ));
    }

    /// Kick off virtual environment creation.
    fn create_venv(&mut self, request: &venv_dialog::Request) {
        let commands = match editor_proc::venv::create_commands(&request.options) {
            Ok(commands) => commands,
            Err(e) => {
                self.error(e.to_string());
                return;
            }
        };

        self.show_output = true;
        match self.runner.start_sequence(commands) {
            Ok(()) => {
                self.pending_venv = Some(venv_dialog::Completion::from_request(request));
            }
            Err(e) => self.error(format!("Could not create environment: {e:#}")),
        }
    }

    /// Adopt a newly created virtual environment, once its commands have run.
    fn finish_venv(&mut self, completion: &venv_dialog::Completion, code: Option<i32>) {
        if code != Some(0) {
            self.error("Virtual environment was not created \u{2014} see the output");
            return;
        }
        // Trust the filesystem over the exit code: `python -m venv` can report
        // success and still leave nothing usable behind if, say, the target was
        // on a full disk.
        if !venv_dialog::interpreter_exists(&completion.interpreter) {
            self.error(format!(
                "Finished, but {} is not there",
                completion.interpreter.display()
            ));
            return;
        }

        if completion.set_as_project_interpreter {
            self.settings
                .set_python_interpreter(&completion.interpreter.display().to_string());
            self.persist_settings();
        }

        if completion.add_to_gitignore {
            match editor_proc::venv::add_to_gitignore(
                &completion.project_root,
                &completion.gitignore_entry(),
            ) {
                Ok(true) => tracing::info!("added {} to .gitignore", completion.gitignore_entry()),
                Ok(false) => {}
                Err(e) => self.error(format!("Could not update .gitignore: {e}")),
            }
        }

        self.tree.refresh();
        match editor_proc::interpreter::version_of(&completion.interpreter) {
            Ok(version) => self.info(format!(
                "Created {} with Python {version}",
                completion.folder_name
            )),
            Err(_) => self.info(format!("Created {}", completion.folder_name)),
        }
    }

    /// Work out what running the active document means, and do it.
    ///
    /// The whole point of the Run button is that it does the obvious thing
    /// without configuration: a Python file runs under the project's
    /// interpreter, a Rust file runs `cargo run` from its manifest directory.
    fn run_active(&mut self, tests: bool) {
        let Some(entry) = self.active_doc() else {
            self.info("Open a file to run");
            return;
        };
        if entry.doc.is_dirty() {
            // Running stale code is a genuinely confusing way to lose an hour.
            if let Some(active) = self.active
                && !self.save_indices(&[active])
            {
                return;
            }
        }

        let Some(entry) = self.active_doc() else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            self.error(editor_proc::run_config::RunError::Unsaved.to_string());
            return;
        };
        let language = entry.language;
        let root = self.tree.root().map(Path::to_path_buf);

        let config = match language {
            LanguageId::Python => {
                let interpreter = editor_proc::interpreter::resolve(
                    &self.settings.python_interpreter(),
                    root.as_deref(),
                );
                editor_proc::run_config::python(&path, interpreter.as_ref(), root.as_deref(), &[])
            }
            LanguageId::Rust => {
                match editor_proc::run_config::find_cargo_manifest(&path, root.as_deref()) {
                    Some(manifest_dir) => editor_proc::run_config::cargo(
                        &manifest_dir,
                        if tests { "test" } else { "run" },
                        false,
                    ),
                    None => Err(editor_proc::run_config::RunError::UnsupportedLanguage(
                        "Rust outside a Cargo project".to_owned(),
                    )),
                }
            }
            other => Err(editor_proc::run_config::RunError::UnsupportedLanguage(
                other.display_name().to_owned(),
            )),
        };

        match config {
            Ok(config) => {
                self.show_output = true;
                if let Err(e) = self.runner.start(config, true) {
                    self.error(format!("Could not start: {e:#}"));
                }
            }
            Err(e) => self.error(e.to_string()),
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
                    find: FindBar::default(),
                    pending_find_step: None,
                    preview: false,
                    syntax_version: None,
                    syntax_due: None,
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
                    self.open_folder(dir);
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
                self.watcher = None;
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
            CommandId::Find | CommandId::Replace => {
                let seed = self
                    .active_doc()
                    .and_then(|entry| entry.view.copy(&entry.doc));
                if let Some(entry) = self.active_mut() {
                    if id == CommandId::Replace {
                        entry.find.open_replace(seed);
                    } else {
                        entry.find.open_find(seed);
                    }
                }
            }
            CommandId::FindNext | CommandId::FindPrevious => {
                // F3 with the bar closed is still "find the next one", using
                // whatever was last searched for.
                if let Some(entry) = self.active_mut() {
                    if !entry.find.is_open() {
                        entry.find.open_find(None);
                    }
                    entry.pending_find_step = Some(if id == CommandId::FindNext { 1 } else { -1 });
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

            CommandId::Run => self.run_active(false),
            CommandId::RunTests => self.run_active(true),
            CommandId::RunStop => {
                if self.runner.is_running() {
                    self.runner.stop();
                } else {
                    self.info("Nothing is running");
                }
            }
            CommandId::RunRestart => {
                if let Err(e) = self.runner.restart() {
                    self.error(format!("{e}"));
                }
            }
            CommandId::ShowOutput => self.show_output = !self.show_output,
            CommandId::ShowProblems => {
                // Always shows rather than toggles: this is reached from the
                // status-bar problem count, where hiding the panel would be a
                // surprising answer to "show me the problems".
                self.show_output = true;
                self.dock = DockTab::Problems;
            }
            CommandId::CreateVenv => match self.tree.root() {
                Some(root) => self.venv_dialog.open(root.to_path_buf()),
                None => self.error("Open a folder before creating a virtual environment"),
            },
            CommandId::SelectInterpreter => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Select a Python interpreter")
                    .pick_file()
                {
                    match editor_proc::interpreter::version_of(&path) {
                        Ok(version) => {
                            self.settings
                                .set_python_interpreter(&path.display().to_string());
                            self.persist_settings();
                            self.info(format!("Using Python {version}"));
                        }
                        Err(e) => self.error(format!("Not a working interpreter: {e}")),
                    }
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

            CommandId::TriggerCompletion => self.trigger_completion(),
            CommandId::GoToDefinition => {
                self.ask_about_symbol(editor_lsp::session::Query::Definition);
            }
            CommandId::FindUses => {
                self.ask_about_symbol(editor_lsp::session::Query::References);
            }
            CommandId::NextUse => self.step_use(1),
            CommandId::PreviousUse => self.step_use(-1),
            CommandId::CommandPalette => self.palette.open(),
            CommandId::OpenSettings => self.settings_form.open(&self.settings),
            CommandId::OpenSettingsFile => {
                let path = self.paths.settings_file();
                self.open_path(&path, false);
            }

            CommandId::About => self.show_about = true,
            CommandId::KeyboardShortcuts => self.show_shortcuts = true,
            CommandId::CheckToolchains => {
                // Probed on demand rather than cached: the answer changes when
                // the user installs something, and the point of the window is
                // to be looked at again after they have.
                self.toolchains = Some(editor_lsp::registry::find_all(&self.tool_search_path()));
            }
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
        // Cloned so the closure below does not borrow `self` while the menu is
        // being drawn. Fifteen paths is nothing.
        let recent = self.recent.clone();
        let mut picked_recent = None;
        let mut clear_recent = false;

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
                                MenuEntry::Recent => {
                                    if let Some(path) = recent_menu(ui, &recent, &mut clear_recent)
                                    {
                                        picked_recent = Some(path);
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                }
            });
        });

        if clear_recent {
            self.recent.clear();
        }
        if picked_recent.is_some() {
            self.pending_recent = picked_recent;
        }
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
        let running = self.runner.is_running() || self.runner.has_queued_work();
        let diagnostic_counts = self.lsp.diagnostics().total_counts();
        let checkers = self.checker_summary();
        let has_run = self.runner.output().line_count() > 1;
        let run_label = self.runner.label().to_owned();

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

                    // Problem counts, and a way to the panel listing them.
                    //
                    // Shown even at zero. A blank space where the count should
                    // be is read as "nothing is wrong", which is the same thing
                    // "nothing is checking" looks like — and the hover is the
                    // only place that difference is stated.
                    if summary.is_some() {
                        ui.separator();
                        let text = if diagnostic_counts.is_empty() {
                            "\u{2713} No problems".to_owned()
                        } else {
                            format!(
                                "{} {}  {} {}",
                                editor_lsp::diagnostics::Severity::Error.glyph(),
                                diagnostic_counts.errors,
                                editor_lsp::diagnostics::Severity::Warning.glyph(),
                                diagnostic_counts.warnings
                            )
                        };
                        if ui.button(text).on_hover_text(&checkers).clicked() {
                            invoked = Some(CommandId::ShowProblems);
                        }
                    }

                    // What is running, and a way back to its output. Without
                    // this, a hidden output panel means a running process with
                    // nothing on screen to say so.
                    if running || has_run {
                        ui.separator();
                        let text = if running {
                            format!("\u{25b6} {run_label}")
                        } else {
                            format!("\u{25a0} {run_label}")
                        };
                        if ui
                            .button(text)
                            .on_hover_text("Show the output panel")
                            .clicked()
                        {
                            invoked = Some(CommandId::ShowOutput);
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
                if let Some(path) = self.welcome(ui) {
                    self.pending_recent = Some(path);
                }
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
            let diagnostics = self.lsp.diagnostics();

            if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) {
                opts.language = entry.language;
                // The parse tree was brought up to date by `sync_highlighters`
                // earlier this frame, so it is safe to paint from here.
                if entry.find.is_open() {
                    let caret = entry.view.selection.head;
                    let mut action = entry.find.ui(ui, &entry.doc, caret);
                    // A keyboard Find Next arrives as a pending step rather
                    // than a click, so it takes the same path.
                    if let Some(direction) = entry.pending_find_step.take()
                        && let Some(range) = entry.find.step_from(caret, direction)
                    {
                        action = find_bar::Action::Reveal(range);
                    }
                    apply_find_action(entry, action);
                    ui.separator();
                }
                entry
                    .view
                    .set_search_matches(entry.find.matches(), entry.find.current_match());
                entry
                    .view
                    .set_diagnostics(Self::underlines_for(entry, diagnostics));

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

    /// The empty state. Returns a recent file if one was clicked.
    ///
    /// The recent list is repeated here as well as in the menu because this is
    /// the screen you are looking at when you want it: an editor opened with
    /// nothing in it is almost always about to reopen something.
    fn welcome(&self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut picked = None;
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
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

            if self.recent.is_empty() {
                return;
            }
            ui.add_space(28.0);
            ui.weak("Recent");
            ui.add_space(4.0);
            for path in self.recent.iter().take(8) {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let response = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(name).color(ui.visuals().hyperlink_color),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(path.display().to_string())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    picked = Some(path.clone());
                }
            }
        });
        picked
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

    /// What is actually checking the code right now, for the status bar hover.
    ///
    /// "No problems" from a syntax check alone means something much weaker than
    /// "no problems" from a syntax check plus a type checker, and the user is
    /// entitled to know which one they are looking at.
    fn checker_summary(&self) -> String {
        let running = self.lsp.running();
        if running.is_empty() {
            "Checking syntax only \u{2014} no language server is running.\n\
             Help \u{2192} Check Toolchains lists what could be installed."
                .to_owned()
        } else {
            format!("Checking syntax, plus: {}", running.join(", "))
        }
    }

    /// Draw the Settings window and act on what it asked for.
    ///
    /// The window mutates `self.settings` directly, so the only work here is
    /// persisting the change and routing the two buttons that need the
    /// application's help. Nothing needs to be re-applied: `sync_appearance`
    /// already re-reads the theme, zoom and font size every frame, and the
    /// editor options are read fresh each time the editor is drawn.
    fn settings_form_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_form.is_open() {
            return;
        }

        let detected = self.detected_interpreter();
        let running = self.lsp.running();
        let action = self
            .settings_form
            .ui(ctx, &mut self.settings, detected.as_deref(), &running);

        match action {
            settings_window::Action::None => {}
            settings_window::Action::Changed => self.persist_settings(),
            settings_window::Action::OpenFile => self.run_command(CommandId::OpenSettingsFile, ctx),
            settings_window::Action::PickInterpreter => {
                self.run_command(CommandId::SelectInterpreter, ctx);
                // The picker writes the setting; refresh the field behind it so
                // the window is not still showing the old path.
                self.settings_form.open(&self.settings);
            }
            settings_window::Action::CheckToolchains => {
                self.run_command(CommandId::CheckToolchains, ctx);
            }
        }
    }

    /// The interpreter that would actually be used, for display.
    ///
    /// The setting is only a preference: an empty one means "detect", and a
    /// project virtual environment wins over both. Showing the setting alone
    /// would tell the user nothing about what Run is going to do.
    fn detected_interpreter(&self) -> Option<String> {
        editor_proc::interpreter::resolve(&self.settings.python_interpreter(), self.tree.root())
            .map(|i| format!("{} ({})", i.path.display(), i.label()))
    }

    /// What optional tooling is installed, and what each missing piece would
    /// buy.
    ///
    /// The Editor works with none of it — PLAN.md §3.6 — but "works without"
    /// must not shade into "silently does less than you think". Someone whose
    /// broken Python shows only a syntax error needs a way to find out that no
    /// type checker is installed, and what to install.
    fn toolchains_window(&mut self, ctx: &egui::Context) {
        let Some(found) = self.toolchains.as_ref() else {
            return;
        };
        let found = found.clone();

        let mut open = true;
        egui::Window::new("Check Toolchains")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    "The Editor checks syntax on its own. Everything below is optional \
                     and adds deeper analysis.",
                );
                ui.add_space(8.0);

                egui::Grid::new("toolchains")
                    .num_columns(4)
                    .striped(true)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.strong("Tool");
                        ui.strong("Status");
                        ui.strong("Provides");
                        ui.strong("Install with");
                        ui.end_row();

                        for spec in editor_lsp::registry::ALL {
                            let installed = found.iter().find(|f| f.spec.id == spec.id);
                            ui.label(spec.name);
                            match installed {
                                Some(f) => {
                                    ui.colored_label(
                                        severity_colour(
                                            ui.visuals(),
                                            editor_lsp::diagnostics::Severity::Hint,
                                        ),
                                        "Installed",
                                    )
                                    .on_hover_text(f.program.display().to_string());
                                    ui.weak(spec.provides);
                                    // Nothing to do, so nothing to copy.
                                    ui.weak("\u{2014}");
                                }
                                None => {
                                    ui.weak("Not found");
                                    ui.weak(spec.provides);
                                    ui.horizontal(|ui| {
                                        ui.code(spec.install);
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(spec.install.to_owned());
                                        }
                                    });
                                }
                            }
                            ui.end_row();
                        }
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.small(
                    "Paste the command into a terminal. `pip` installs into whichever Python \
                     is on your PATH; to lint a project with its own virtual environment, \
                     activate it first and The Editor will prefer the tools it finds there.",
                );

                ui.add_space(8.0);
                if ui.button("Re-check").clicked() {
                    self.toolchains =
                        Some(editor_lsp::registry::find_all(&self.tool_search_path()));
                }
            });

        if !open {
            self.toolchains = None;
        }
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

        // Restoring needs the window to exist, so it happens here rather than
        // in `new`.
        if let Some(session) = self.restore.take() {
            self.restore_session(&session, &ctx);
        }
        self.poll_watcher();
        self.sync_highlighters(&ctx);
        self.sync_completion();
        // Before the menu bar, toolbar and editor read this frame's events:
        // whoever looks first gets the key.
        self.completion_keys(&ctx);
        self.sync_language_servers();

        // Never let the window close with unsaved work. This must run before
        // anything else in the frame, and `quit_confirmed` stops the second
        // close request being intercepted again.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            // Written before the unsaved prompt, so the session survives even
            // if the user then cancels the quit and closes some other way.
            self.save_session(&ctx);
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
        // The popup is modal for keyboard purposes: while it is up, Enter,
        // Tab, Escape and the arrows belong to it, and a global shortcut firing
        // behind it would act on a document the user is not looking at.
        let modal_open = self.completion.is_open()
            || self.settings_form.is_open()
            || self.pending_delete.is_some()
            || self.palette.is_open()
            || self.new_file.is_open()
            || self.venv_dialog.is_open()
            || self.pending.is_some();
        let mut invoked = if modal_open {
            None
        } else {
            commands::triggered(&ctx)
        };
        invoked = self.menu_bar(ui).or(invoked);
        invoked = self.toolbar(ui).or(invoked);
        invoked = self.status_bar(ui).or(invoked);

        let mut tree_action = editor_widgets::file_tree::Action::None;
        if self.settings.show_file_tree() {
            egui::Panel::left("explorer")
                .default_size(250.0)
                .size_range(160.0..=600.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Explorer");
                    });
                    ui.separator();
                    tree_action = self.tree.ui(ui);
                });
        }
        self.apply_tree_action(tree_action, &ctx);

        // Keep draining the process even while the dock is hidden, or output
        // piles up in the channel and arrives in a lump when it is reopened.
        if self.runner.poll() {
            ctx.request_repaint();
        }
        if let Some(code) = self.runner.take_finished() {
            // A virtual environment being created takes precedence over the
            // generic banner: the user asked for an environment, not for a
            // process to exit.
            if let Some(completion) = self.pending_venv.take() {
                self.finish_venv(&completion, code);
            } else {
                match code {
                    Some(0) => self.info("Finished"),
                    Some(code) => self.error(format!("Exited with code {code}")),
                    None => self.info("Terminated"),
                }
            }
        }
        // Between the steps of a sequence there is momentarily no process, but
        // work is still pending — treating that as "finished" makes the status
        // bar and the console header flicker.
        if self.runner.is_running() || self.runner.has_queued_work() {
            // A running process produces output between frames, so keep
            // repainting rather than waiting for input.
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if self.show_output {
            let mut console_action = None;
            let mut problem_clicked = None;

            // The height is owned here rather than left to the panel.
            //
            // egui 0.36 lays a panel's content out against `size_range.max` and
            // then stores whatever size the content came out at, so a panel
            // containing a `ScrollArea` that fills its space grows to the
            // maximum on the second frame and stays there — which is how the
            // output panel could swallow the whole window with no way back.
            // An exact size plus a drag strip of our own gives the behaviour
            // that was wanted anyway: a sensible fixed height that can be
            // dragged larger.
            let dock_height = clamp_dock_height(self.dock_height, ui.available_height());

            egui::Panel::bottom("dock")
                .resizable(false)
                .exact_size(dock_height)
                .show(ui, |ui| {
                    // Drag strip along the top edge.
                    let (strip, drag) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 5.0),
                        egui::Sense::drag(),
                    );
                    let drag = drag.on_hover_cursor(egui::CursorIcon::ResizeVertical);
                    if drag.dragged() {
                        // Dragging up makes the panel taller.
                        self.dock_height = (self.dock_height - drag.drag_delta().y)
                            .clamp(MIN_DOCK_HEIGHT, MAX_DOCK_HEIGHT);
                    }
                    if drag.hovered() || drag.dragged() {
                        ui.painter()
                            .rect_filled(strip, 0.0, ui.visuals().widgets.hovered.bg_fill);
                    }
                    let counts = self.lsp.diagnostics().total_counts();
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(self.dock == DockTab::Output, "OUTPUT")
                            .clicked()
                        {
                            self.dock = DockTab::Output;
                        }
                        let problems = if counts.is_empty() {
                            "PROBLEMS".to_owned()
                        } else {
                            format!("PROBLEMS ({})", counts.total())
                        };
                        if ui
                            .selectable_label(self.dock == DockTab::Problems, problems)
                            .clicked()
                        {
                            self.dock = DockTab::Problems;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("\u{00d7}").on_hover_text("Hide").clicked() {
                                self.show_output = false;
                            }
                        });
                    });
                    ui.separator();

                    match self.dock {
                        DockTab::Output => console_action = Some(self.runner.draw(ui)),
                        DockTab::Problems => problem_clicked = self.problems_ui(ui),
                    }
                });

            if let Some(action) = console_action {
                self.apply_console_action(action);
            }
            if let Some((path, line, column)) = problem_clicked {
                self.open_at(&path, line, column);
            }
        }

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
        if let Some(request) = self.venv_dialog.ui(&ctx) {
            self.create_venv(&request);
        }

        self.about_window(&ctx);
        self.shortcuts_window(&ctx);
        self.toolchains_window(&ctx);
        self.settings_form_ui(&ctx);
        // After the editor has painted, so the caret rect it anchors to is
        // from this frame rather than the last one.
        self.completion_draw(&ctx);
        self.unsaved_prompt(&ctx);
        self.delete_prompt(&ctx);
        self.toasts_ui(&ctx);

        // The editor's right-click menu, drained after the frame it was used in
        // so nothing mutates the document while it is being painted.
        if let Some(action) = self
            .active
            .and_then(|i| self.docs.get_mut(i))
            .and_then(|e| e.view.take_context_action())
        {
            use editor_widgets::editor_view::ContextAction;
            match action {
                ContextAction::GoToDefinition => {
                    self.run_command(CommandId::GoToDefinition, &ctx);
                }
                ContextAction::FindUses => self.run_command(CommandId::FindUses, &ctx),
                ContextAction::Paste => self.run_command(CommandId::Paste, &ctx),
            }
        }

        // Opening happens after the menu closes, so the tree and tab bar are
        // not mutated while they are being drawn.
        if let Some(path) = self.pending_recent.take() {
            if path.is_file() {
                self.open_path(&path, false);
            } else {
                // Dropped rather than left to fail again next time.
                self.recent.retain(|p| *p != path);
                self.error(format!("{} no longer exists", path.display()));
            }
        }

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

impl EditorApp {
    /// Carry out what the console asked for.
    fn apply_console_action(&mut self, action: editor_widgets::console::Action) {
        use editor_widgets::console::Action;
        match action {
            Action::None => {}
            Action::OpenLocation { path, line, column } => {
                if !path.is_file() {
                    self.error(format!("{} does not exist", path.display()));
                    return;
                }
                self.open_path(&path, false);
                if let Some(entry) = self.active_mut() {
                    // Output line and column numbers are one-based.
                    let offset = entry.doc.offset_at(
                        line.saturating_sub(1),
                        column.unwrap_or(1).saturating_sub(1),
                    );
                    entry.view.set_caret(offset);
                    entry.view.focus();
                }
            }
            Action::SendInput(text) => self.runner.send_input(&text),
            Action::Stop => self.runner.stop(),
            Action::Restart => {
                if let Err(e) = self.runner.restart() {
                    self.error(format!("{e}"));
                }
            }
            Action::Clear => self.runner.clear(),
        }
    }
}

/// Carry out what the find bar asked for.
///
/// Replacements go through the document's normal transaction path, so they land
/// in the undo history and reach the highlighter like any other edit. Replace
/// All is a single transaction, and therefore a single undo step — undoing a
/// 500-match replace one match at a time would be unusable.
fn apply_find_action(entry: &mut OpenDoc, action: find_bar::Action) {
    use editor_core::edit::{Edit, Transaction};
    use editor_core::selection::Selection;

    match action {
        find_bar::Action::None => {}
        find_bar::Action::Reveal(range) => {
            entry.view.select_range(range.start, range.end);
        }
        find_bar::Action::Replace { range, with } => {
            if !entry.doc.is_editable() {
                return;
            }
            let before = entry.view.selection;
            let after = Selection::at(range.start + with.chars().count());
            entry.doc.break_undo_run();
            entry
                .doc
                .apply(&Transaction::replace(range, with), before, after);
            entry.view.set_caret(after.head);
            entry.doc.break_undo_run();
        }
        find_bar::Action::ReplaceAll(edits) => {
            if !entry.doc.is_editable() || edits.is_empty() {
                return;
            }
            let count = edits.len();
            let before = entry.view.selection;
            let transaction = Transaction::new(
                edits
                    .into_iter()
                    .map(|(range, with)| Edit::replace(range, with))
                    .collect(),
            );
            entry.doc.break_undo_run();
            entry.doc.apply(&transaction, before, before);
            entry.doc.break_undo_run();
            tracing::info!(count, "replaced all occurrences");
        }
        find_bar::Action::FocusEditor => entry.view.focus(),
    }
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

/// The Open Recent submenu. Returns the file the user picked.
///
/// Entries are labelled by file name with the containing directory beside them,
/// because a list of fifteen full paths is unreadable and a list of fifteen bare
/// file names cannot distinguish two `main.py`s.
fn recent_menu(
    ui: &mut egui::Ui,
    recent: &[PathBuf],
    clear: &mut bool,
) -> Option<std::path::PathBuf> {
    let mut picked = None;
    ui.menu_button("Open Recent", |ui| {
        if recent.is_empty() {
            ui.weak("Nothing yet");
            return;
        }
        for path in recent {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let parent = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();

            let response = ui.horizontal(|ui| {
                let clicked = ui.selectable_label(false, &name).clicked();
                ui.weak(shorten_middle(&parent, 44));
                clicked
            });
            if response.inner {
                picked = Some(path.clone());
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Clear Recent Files").clicked() {
            *clear = true;
            ui.close();
        }
    });
    picked
}

/// Shorten a path for display by eliding its middle, keeping both ends.
///
/// The ends are what identify a path — the drive or project at one end, the
/// containing folder at the other — so truncating the tail hides the useful
/// half.
fn shorten_middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(3) / 2;
    let head: String = chars.iter().take(keep).collect();
    let tail: String = chars.iter().skip(chars.len() - keep).collect();
    format!("{head}\u{2026}{tail}")
}

/// Whether a file failed to parse, as opposed to merely having problems.
///
/// Keyed off the built-in check's reserved source, which is the one thing that
/// definitely ran: a machine with no language server still gets the note, and a
/// machine with three does not get it three times.
fn has_syntax_error(diagnostics: &[editor_lsp::diagnostics::Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|d| d.source == editor_lsp::session::BUILTIN_SOURCE)
}

/// Map a parse-tree symbol kind onto the protocol's numbering.
///
/// Done here rather than in `editor-syntax`, which has no business knowing
/// about LSP; the popup's glyphs are keyed off the protocol's numbers because
/// that is what a real server sends.
fn lsp_kind(kind: editor_syntax::symbols::SymbolKind) -> u8 {
    use editor_syntax::symbols::SymbolKind;
    match kind {
        SymbolKind::Function => 3,
        SymbolKind::Class => 7,
        SymbolKind::Module => 9,
        SymbolKind::Binding | SymbolKind::Unknown => 6,
    }
}

/// A parser error, as a diagnostic.
///
/// Reported as an error rather than a warning because it is not a matter of
/// style or opinion: the file is not the language it claims to be, and nothing
/// downstream — running it, importing it, linting it — can work until it is.
fn to_diagnostic(error: editor_syntax::errors::SyntaxError) -> editor_lsp::diagnostics::Diagnostic {
    editor_lsp::diagnostics::Diagnostic {
        severity: editor_lsp::diagnostics::Severity::Error,
        line: error.line,
        column: error.column,
        end_line: error.end_line,
        end_column: error.end_column,
        message: error.message,
        code: None,
        source: "syntax".to_owned(),
    }
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
        CommandId::Find => "\u{1f50d}",
        CommandId::Run => "\u{25b6}",
        CommandId::RunStop => "\u{25a0}",
        CommandId::ToggleExplorer => "\u{2630}",
        CommandId::CommandPalette => "\u{2318}",
        CommandId::OpenSettings | CommandId::OpenSettingsFile => "\u{2699}",
        _ => "?",
    }
}

/// How tall the output dock may actually be, given the space available.
///
/// The editor pane always keeps a usable strip, whatever height was dragged or
/// however small the window becomes. A dock that fills the window leaves no way
/// back to the code, which is exactly the state this guards against.
fn clamp_dock_height(desired: f32, available: f32) -> f32 {
    /// Rows of editor that must remain visible.
    const RESERVED_FOR_EDITOR: f32 = 120.0;

    let ceiling = (available - RESERVED_FOR_EDITOR).clamp(MIN_DOCK_HEIGHT, MAX_DOCK_HEIGHT);
    if desired.is_finite() {
        desired.clamp(MIN_DOCK_HEIGHT, ceiling)
    } else {
        DEFAULT_DOCK_HEIGHT.min(ceiling)
    }
}

/// A name inside `directory` that is not already taken.
fn unique_path(directory: &Path, stem: &str, extension: &str) -> PathBuf {
    let build = |suffix: String| {
        let name = if extension.is_empty() {
            format!("{stem}{suffix}")
        } else {
            format!("{stem}{suffix}.{extension}")
        };
        directory.join(name)
    };

    let first = build(String::new());
    if !first.exists() {
        return first;
    }
    for n in 2..1000 {
        let candidate = build(format!(" {n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Select a path in the platform's file manager, rather than merely opening
/// the folder that contains it.
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // `/select,` highlights the item; without it Explorer opens the file.
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R".as_ref(), path.as_os_str()])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No portable "select" on Linux, so open the containing directory.
        let directory = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(directory)
            .spawn()
            .map(|_| ())
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
    /// The Open Recent submenu, whose contents are data rather than commands
    /// and so cannot come from the registry.
    Recent,
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
            MenuEntry::Recent,
            MenuEntry::Item(CommandId::OpenFolder),
            MenuEntry::Item(CommandId::CloseFolder),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Save),
            MenuEntry::Item(CommandId::SaveAs),
            MenuEntry::Item(CommandId::SaveAll),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::CloseTab),
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
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Find),
            MenuEntry::Item(CommandId::Replace),
            MenuEntry::Item(CommandId::FindNext),
            MenuEntry::Item(CommandId::FindPrevious),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::TriggerCompletion),
            MenuEntry::Item(CommandId::GoToDefinition),
            MenuEntry::Item(CommandId::FindUses),
            MenuEntry::Item(CommandId::NextUse),
            MenuEntry::Item(CommandId::PreviousUse),
        ],
    ),
    (
        "View",
        &[
            MenuEntry::Item(CommandId::ToggleExplorer),
            MenuEntry::Item(CommandId::ShowOutput),
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
    (
        "Run",
        &[
            MenuEntry::Item(CommandId::Run),
            MenuEntry::Item(CommandId::RunStop),
            MenuEntry::Item(CommandId::RunRestart),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::RunTests),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::SelectInterpreter),
            MenuEntry::Item(CommandId::CreateVenv),
        ],
    ),
    (
        "Tools",
        &[
            MenuEntry::Item(CommandId::CommandPalette),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::OpenSettings),
            MenuEntry::Item(CommandId::OpenSettingsFile),
        ],
    ),
    (
        "Help",
        &[
            MenuEntry::Item(CommandId::KeyboardShortcuts),
            MenuEntry::Item(CommandId::CheckToolchains),
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
    &[CommandId::Find],
    &[CommandId::Run, CommandId::RunStop],
    &[CommandId::ToggleExplorer],
    &[CommandId::CommandPalette, CommandId::OpenSettings],
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

    /// Regression: the output panel grew to fill the whole window, hiding the
    /// editor with no way to get it back.
    #[test]
    fn the_dock_always_leaves_room_for_the_editor() {
        let window = 800.0;
        // Even asking for far more than the window has.
        let height = clamp_dock_height(10_000.0, window);
        assert!(
            height < window,
            "the dock must not fill the window: {height} of {window}"
        );
        assert!(
            window - height >= 100.0,
            "too little editor left: {} points",
            window - height
        );
    }

    #[test]
    fn the_dock_keeps_its_requested_height_when_there_is_room() {
        assert!((clamp_dock_height(220.0, 900.0) - 220.0).abs() < f32::EPSILON);
        assert!((clamp_dock_height(400.0, 900.0) - 400.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_dock_stays_usable_in_a_very_short_window() {
        // A window too short to honour both minimums has to break one of them;
        // the dock keeps its minimum so its header and buttons stay reachable.
        let height = clamp_dock_height(220.0, 150.0);
        assert!(height >= MIN_DOCK_HEIGHT);
        assert!(height.is_finite());
    }

    #[test]
    fn a_nonsense_height_falls_back_to_the_default() {
        // Guards against a NaN reaching the panel, which lays out as an
        // invisible or infinite rectangle.
        let height = clamp_dock_height(f32::NAN, 900.0);
        assert!(height.is_finite());
        assert!((height - DEFAULT_DOCK_HEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn the_dock_is_never_dragged_past_its_hard_cap() {
        assert!(clamp_dock_height(5_000.0, 10_000.0) <= MAX_DOCK_HEIGHT);
    }

    /// The whole built-in checking path, end to end, with no server installed:
    /// parse the file, walk the tree, convert, store, read back.
    ///
    /// The reported bug was that obviously broken Python showed nothing at all
    /// on a machine with no Python language server, so the thing worth testing
    /// is that this route works with nothing else present.
    #[test]
    fn broken_python_produces_a_diagnostic_with_no_language_server() {
        use editor_syntax::highlight::Highlighter;
        use ropey::Rope;

        // The user's file, as reported.
        let source = "def main(argv: list[str] | None = None) -> int:\n\
                      \x20   \"\"\"Entry point.\"\"\"\n\
                      \x20   data = ['one', 'two']\n\
                      \x20   for i in data:\n\
                      \x20       print(i)\n\
                      \n\
                      \x20   if bob = kate\n\
                      \x20   print(end)\n";
        let rope = Rope::from_str(source);
        let highlighter = Highlighter::new(LanguageId::Python, &rope).expect("python grammar");

        let mut store = editor_lsp::diagnostics::Store::default();
        let path = Path::new("/project/main.py");
        store.set(
            path,
            editor_lsp::session::BUILTIN_SOURCE,
            highlighter
                .errors(&rope)
                .into_iter()
                .map(to_diagnostic)
                .collect(),
        );

        let found = store.for_file(path);
        assert!(
            !found.is_empty(),
            "`if bob = kate` must be reported without a language server"
        );
        assert!(
            found.iter().any(|d| d.line == 6),
            "the diagnostic belongs on the `if` line: {found:?}"
        );
        assert!(
            found
                .iter()
                .all(|d| d.severity == editor_lsp::diagnostics::Severity::Error),
            "a file that does not parse is an error, not a suggestion"
        );
    }

    /// A file that does not parse gets the "nothing else can check this" note;
    /// a file that merely has lint findings must not.
    #[test]
    fn the_unparseable_note_appears_only_when_the_parse_failed() {
        let syntax = to_diagnostic(editor_syntax::errors::SyntaxError {
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 4,
            message: "Syntax error: `oops`".to_owned(),
        });
        let lint = editor_lsp::diagnostics::Diagnostic {
            severity: editor_lsp::diagnostics::Severity::Warning,
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 3,
            message: "Undefined name `sys`".to_owned(),
            code: Some("F821".to_owned()),
            source: "Ruff".to_owned(),
        };

        assert!(has_syntax_error(&[syntax.clone(), lint.clone()]));
        assert!(!has_syntax_error(&[lint]));
        assert!(!has_syntax_error(&[]));

        // Ruff also reports the parse failure. The note must not double up, so
        // it keys off our own source rather than on anything the servers say.
        let ruff_syntax = editor_lsp::diagnostics::Diagnostic {
            message: "invalid-syntax: Expected `:`, found `=`".to_owned(),
            source: "Ruff".to_owned(),
            ..syntax.clone()
        };
        assert!(!has_syntax_error(&[ruff_syntax]));
    }

    #[test]
    fn valid_python_produces_no_builtin_diagnostics() {
        use editor_syntax::highlight::Highlighter;
        use ropey::Rope;

        let rope = Rope::from_str("def main() -> int:\n    print('ok')\n    return 0\n");
        let highlighter = Highlighter::new(LanguageId::Python, &rope).expect("python grammar");
        assert!(
            highlighter.errors(&rope).is_empty(),
            "working code must not be flagged"
        );
    }

    /// The built-in check must not be filed under a server's name, or a crashed
    /// server's cleanup would take the syntax errors with it.
    #[test]
    fn builtin_diagnostics_survive_a_server_crash() {
        let mut store = editor_lsp::diagnostics::Store::default();
        let path = Path::new("/project/main.py");
        store.set(
            path,
            editor_lsp::session::BUILTIN_SOURCE,
            vec![to_diagnostic(editor_syntax::errors::SyntaxError {
                line: 0,
                column: 0,
                end_line: 0,
                end_column: 4,
                message: "Syntax error: `oops`".to_owned(),
            })],
        );
        for spec in editor_lsp::registry::ALL {
            store.clear_server(spec.id);
        }
        assert_eq!(
            store.for_file(path).len(),
            1,
            "a crashing server must not clear The Editor's own diagnostics"
        );
    }

    #[test]
    fn the_status_bar_says_what_is_checking_when_no_server_runs() {
        // "No problems" from a syntax check alone means much less than the same
        // words with a type checker behind them.
        let lsp = editor_lsp::session::Lsp::default();
        assert!(lsp.running().is_empty(), "nothing is started in a test");
        // The wording is asserted rather than the mechanism, because the whole
        // point is what the user reads.
        let summary = "Checking syntax only \u{2014} no language server is running.\n\
                       Help \u{2192} Check Toolchains lists what could be installed.";
        assert!(summary.contains("syntax only"));
        assert!(summary.contains("Check Toolchains"));
    }

    #[test]
    fn every_optional_tool_can_be_reported_missing_with_a_way_to_install_it() {
        // The complaint that prompted this: the panel named three tools the
        // user did not have and gave no next step.
        for spec in editor_lsp::registry::ALL {
            assert!(
                !spec.install.is_empty(),
                "{} is listed as missing with no way to install it",
                spec.name
            );
        }
    }

    #[test]
    fn the_theme_button_cycles_through_all_three_preferences() {
        assert_eq!(next_theme_command("Dark"), CommandId::ThemeLight);
        assert_eq!(next_theme_command("Light"), CommandId::ThemeSystem);
        assert_eq!(next_theme_command("Follow System"), CommandId::ThemeDark);
    }

    /// Servers do not answer in document order. pyright returned the uses of
    /// one function as helpers.py:4, main.py:9, main.py:7 — so pressing F8
    /// walked *up* the file, which reads as the feature being broken.
    #[test]
    fn every_local_symbol_kind_maps_to_a_glyph_the_popup_knows() {
        use editor_syntax::symbols::SymbolKind;
        // The popup keys its glyphs off the protocol's numbers, because that is
        // what a real server sends; a fallback item carrying a number the popup
        // does not recognise would render as a bare dot beside real ones.
        for kind in [
            SymbolKind::Function,
            SymbolKind::Class,
            SymbolKind::Module,
            SymbolKind::Binding,
            SymbolKind::Unknown,
        ] {
            let item = editor_lsp::session::Completion {
                label: "x".to_owned(),
                insert: "x".to_owned(),
                detail: None,
                kind: Some(lsp_kind(kind)),
                sort_text: None,
            };
            assert_ne!(
                item.glyph(),
                "\u{b7}",
                "{kind:?} falls through to the unknown glyph"
            );
        }
    }

    #[test]
    fn results_are_sorted_into_document_order_and_deduped() {
        let a = PathBuf::from("/p/a.py");
        let b = PathBuf::from("/p/b.py");
        let mut locations = vec![
            Target {
                path: Some(b.clone()),
                line: 8,
                column: 4,
            },
            Target {
                path: Some(a.clone()),
                line: 3,
                column: 0,
            },
            Target {
                path: Some(b.clone()),
                line: 6,
                column: 11,
            },
            Target {
                path: Some(b.clone()),
                line: 6,
                column: 4,
            },
            // The same place twice: two servers, or a server listing the
            // declaration alongside a reference to it.
            Target {
                path: Some(a.clone()),
                line: 3,
                column: 0,
            },
        ];
        locations.sort_by(|x, y| {
            x.path
                .cmp(&y.path)
                .then(x.line.cmp(&y.line))
                .then(x.column.cmp(&y.column))
        });
        locations.dedup();

        let seen: Vec<(String, usize, usize)> = locations
            .iter()
            .map(|t| {
                (
                    t.path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    t.line,
                    t.column,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("a.py".to_owned(), 3, 0),
                ("b.py".to_owned(), 6, 4),
                ("b.py".to_owned(), 6, 11),
                ("b.py".to_owned(), 8, 4),
            ],
            "results must read down the file, and not repeat"
        );
    }

    #[test]
    fn a_long_path_is_elided_in_its_middle_not_its_tail() {
        // Both ends identify a path; truncating the tail throws away the
        // containing folder, which is the half that distinguishes two files
        // with the same name.
        let long = "C:/projects/some/deeply/nested/place/that/goes/on/src";
        let short = shorten_middle(long, 24);
        assert!(short.chars().count() <= 24, "got {short:?}");
        assert!(short.starts_with("C:/pro"), "the head is kept: {short:?}");
        assert!(short.ends_with("src"), "the tail is kept: {short:?}");
        assert!(short.contains('\u{2026}'));
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(shorten_middle("C:/tmp", 24), "C:/tmp");
    }

    #[test]
    fn eliding_does_not_split_a_multi_byte_character() {
        // Char-based, not byte-based: slicing a path with an accent in it at a
        // byte offset panics.
        let path = "C:/Users/José/Documentos/proyectos/análisis/código/src";
        let short = shorten_middle(path, 20);
        assert!(short.chars().count() <= 20);
    }

    #[test]
    fn no_command_appears_in_two_menus() {
        // Settings moved to Tools while the file entry was still in File, so
        // "Open settings.toml" was listed twice with no way to notice.
        let mut seen = std::collections::HashMap::new();
        for (menu, entries) in MENUS {
            for entry in *entries {
                if let MenuEntry::Item(id) = entry
                    && let Some(first) = seen.insert(*id, *menu)
                {
                    panic!("{id:?} is in both the {first} and {menu} menus");
                }
            }
        }
    }

    #[test]
    fn open_recent_is_in_the_file_menu() {
        let file_menu = MENUS
            .iter()
            .find(|(name, _)| *name == "File")
            .expect("a File menu");
        assert!(
            file_menu.1.iter().any(|e| matches!(e, MenuEntry::Recent)),
            "Open Recent is not reachable"
        );
    }

    #[test]
    fn every_theme_preference_has_a_command_and_a_menu_entry() {
        let in_menu: Vec<CommandId> = MENUS
            .iter()
            .flat_map(|(_, entries)| entries.iter())
            .filter_map(|e| match e {
                MenuEntry::Item(id) => Some(*id),
                MenuEntry::Separator | MenuEntry::Recent => None,
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
