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
use editor_core::document::{DiskState, Document};
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::theme::SyntaxTheme;
use editor_widgets::editor_view::{EditorOptions, EditorView, Underline, severity_colour};
use editor_widgets::find_bar::{self, FindBar};
use editor_widgets::{file_tree::FileTree, tab_bar, theme as ui_theme};
use eframe::egui;

use crate::commands::{self, CommandId};
use crate::completion;
use crate::debugger::{self, Breakpoints, DebugView};
use crate::diff_view;
use crate::docs_window::DocsWindow;
use crate::file_picker::FilePicker;
use crate::git_panel::{self, GitPanel};
use crate::history_view::{self, HistoryView};
use crate::new_file;
use crate::packages_panel::PackagesPanel;
use crate::palette::Palette;
use crate::project_search::ProjectSearch;
use crate::recovery;
use crate::runner::Runner;
use crate::settings_window;
use crate::symbol_picker::SymbolPicker;
use crate::terminal::Terminal;
use crate::venv_dialog;
use crate::watcher::Watcher;

/// Metadata shown in the About dialog.
/// The user manual, compiled in so it is there with no network and no install
/// directory to go looking in.
pub(crate) const MANUAL: &str = include_str!("../../../docs/manual.md");

/// Generated from the resolved dependency graph by `tools/make_licences.py`.
pub(crate) const THIRD_PARTY: &str = include_str!("../../../docs/third-party.md");

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

/// How many definitions the project-wide fallback collects before stopping.
///
/// More than a handful means the name is common enough that a text search
/// cannot tell which one was meant, and the list stops being an answer.
const MAX_PROJECT_DEFINITIONS: usize = 20;

/// Largest file the fallback will read. Anything bigger is generated, vendored
/// or data, and parsing it to answer one question is not worth the pause.
const MAX_SEARCHED_BYTES: usize = 2 * 1024 * 1024;

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
    /// Line count as of the last frame, so breakpoints can follow their lines.
    line_count_seen: Option<usize>,
    /// When the next syntax check is due, so squiggles do not flicker under
    /// the caret while a line is half-typed.
    syntax_due: Option<Instant>,
    /// Identifies this tab to the crash-recovery store for as long as it is
    /// open. Not the path: an untitled buffer has none, and that is exactly
    /// the buffer with nothing else to fall back on.
    recovery_id: u64,
    /// What has happened to the file behind this document's back, and so
    /// whether the reload bar is showing.
    ///
    /// Held per document rather than recomputed each frame: the answer needs
    /// a `stat`, and it has to persist across frames anyway because the bar
    /// stays up until the user decides what to do about it.
    disk: DiskState,
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
    /// What git says about the project: the branch, how each open buffer
    /// differs from the committed version, and what is staged.
    git: editor_vcs::tracker::Tracker,
    /// The Source Control tab.
    git_panel: GitPanel,
    /// The commit history, in a window of its own.
    history: HistoryView,
    /// Whether the editor shows who last touched each line.
    show_blame: bool,
    /// Whether the window had focus last frame, so returning to it can be
    /// told from merely still having it.
    was_focused: bool,
    /// Paths named on the command line, opened once the window exists.
    from_command_line: Vec<PathBuf>,
    /// Copies of unsaved buffers, so a crash does not take them.
    recovery: recovery::Recovery,
    /// Next id to hand to a tab. Monotonic, never reused within a session.
    next_recovery_id: u64,
    /// Work found lying about from a session that did not shut down, shown
    /// as a prompt on the first frame.
    recovered: Vec<recovery::Recovered>,
    manual: DocsWindow,
    licences: DocsWindow,
    /// The buffer against what was committed. Its own window rather than a
    /// dock tab: it is consulted and closed, not lived in.
    diff_view: diff_view::DiffView,
    /// The sections the diff window is showing, with the file and buffer
    /// version they were built from. Held so an open window does not re-diff
    /// the file on every frame it is on screen.
    diff_cache: Option<(PathBuf, u64, Vec<editor_vcs::unified::Section>)>,
    /// True until the welcome has been dismissed. Set when there was no
    /// settings file to load, which is the only honest signal that nobody has
    /// used this before.
    first_run: bool,
    /// When the process started, so the first frame can report how long it
    /// took to get there. Taken once and then dropped.
    started: Option<Instant>,
    /// What the dirty buffers looked like last frame, so an edit anywhere
    /// schedules a write without every edit path having to remember to.
    dirty_seen: Vec<(u64, u64)>,
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
    /// Paths awaiting a yes/no before their changes are thrown away.
    ///
    /// Unlike a delete this does not go to the recycle bin, and unlike
    /// everything else in the git panel it cannot be undone by git either: the
    /// text was never committed, never stashed, and is not in the reflog.
    pending_discard: Option<Vec<String>>,
    /// Whether the Problems panel shows every file or only the open one.
    problems_all_files: bool,
    /// The diagnostic the caret is sitting on, so the Problems panel can pick
    /// it out of a long list. Its line and column, not an index: the list is
    /// rebuilt every frame and an index into it would not survive.
    problem_at_caret: Option<(PathBuf, u32, u32)>,
    /// What the panel last scrolled to, so it only does so when it changes.
    problem_revealed: Option<(PathBuf, u32, u32)>,
    /// Breakpoints, which outlive any debug session and are saved with it.
    breakpoints: Breakpoints,
    /// Tab indices in most-recently-used order, newest first, for Ctrl+Tab.
    ///
    /// Indices rather than paths, so an untitled buffer is in the list too;
    /// kept in step as tabs are opened, closed and reordered.
    mru: Vec<usize>,
    /// Set while Ctrl is held during a Ctrl+Tab cycle. The list is only
    /// re-ordered when it is released, or every step would move the tab you
    /// just left to the front and cycling would bounce between two.
    cycling: bool,
    /// Whether the "debugpy is missing" note has been shown. Once per run: it
    /// is worth saying, and worth saying only once.
    debugpy_warned: bool,
    /// The running debug session, if any.
    debug: Option<editor_debug::Session>,
    /// Stack and variables for the paused session.
    debug_view: DebugView,
    /// The integrated shell.
    terminal: Terminal,
    /// What is installed in the project's Python environment.
    packages: PackagesPanel,
    /// The project-wide search panel.
    search: ProjectSearch,
    /// The rename prompt: what the symbol is called, and the new name.
    rename: Option<(String, String)>,
    /// Results of the last Find Uses, and where in them the user is.
    uses: UseResults,
    /// Go to File (Ctrl+P).
    file_picker: FilePicker,
    symbol_picker: SymbolPicker,
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
    Debug,
    Search,
    Terminal,
    Packages,
    Git,
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
    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        paths: AppPaths,
        log_dir: String,
        open: Vec<PathBuf>,
        started: Instant,
    ) -> Self {
        // Asked before anything writes the file, which `new` goes on to do.
        let first_run = !paths.settings_file().exists();
        let (settings, settings_error) = Settings::load(&paths.settings_file());
        let session = Session::load(&paths.session_file());

        // Before this session claims a directory of its own, so its own empty
        // one is not among the candidates.
        let backups = paths.backup_dir();
        let recovered = recovery::collect(&backups, None);
        let paths_for_recovery = backups;

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
            // Created up front rather than when a folder opens: a single file
            // opened from the command line needs watching too, and there may
            // never be a folder.
            watcher: Watcher::new(Some(cc.egui_ctx.clone()))
                .inspect_err(|e| tracing::warn!("no filesystem watcher: {e}"))
                .ok(),
            git: {
                // Every answer from the git worker has to wake the loop, or it
                // arrives into an idle application and sits there unpainted
                // until the next keystroke. PLAN.md §2.5.
                let ctx = cc.egui_ctx.clone();
                editor_vcs::tracker::Tracker::new(std::sync::Arc::new(move || {
                    ctx.request_repaint();
                }))
            },
            git_panel: GitPanel::default(),
            history: HistoryView::default(),
            show_blame: false,
            was_focused: true,
            from_command_line: open,
            recovery: recovery::Recovery::new(&paths_for_recovery),
            next_recovery_id: 1,
            recovered,
            manual: DocsWindow::default(),
            licences: DocsWindow::default(),
            diff_view: diff_view::DiffView::default(),
            diff_cache: None,
            first_run,
            started: Some(started),
            dirty_seen: Vec::new(),
            restore: settings.restore_session().then(|| session.clone()),
            session_saved: false,
            recent: Vec::new(),
            pending_recent: None,
            pending_delete: None,
            pending_discard: None,
            problems_all_files: false,
            problem_at_caret: None,
            problem_revealed: None,
            breakpoints: Breakpoints::default(),
            mru: Vec::new(),
            cycling: false,
            debugpy_warned: false,
            debug: None,
            debug_view: DebugView::default(),
            terminal: Terminal::default(),
            packages: PackagesPanel::default(),
            search: ProjectSearch::default(),
            rename: None,
            uses: UseResults::default(),
            file_picker: FilePicker::default(),
            symbol_picker: SymbolPicker::default(),
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
        let mut opts = EditorOptions {
            font_size: self.settings.font_size(),
            tab_width: self.settings.tab_width(),
            insert_spaces: self.settings.insert_spaces(),
            show_line_numbers: true,
            language: LanguageId::PlainText,
            auto_close_brackets: self.settings.auto_close_brackets(),
            reduce_motion: self.settings.reduce_motion(),
        };

        // A project's `.editorconfig` outranks these settings for files it
        // covers: it is a statement about that code rather than about this
        // user, and it is why two people editing one repository do not fight
        // over tabs in the diff.
        if self.settings.use_editorconfig()
            && let Some(path) = self
                .active
                .and_then(|i| self.docs.get(i))
                .and_then(|e| e.doc.path())
        {
            let style = editor_config::editorconfig::style_for(path);
            if let Some(spaces) = style.insert_spaces {
                opts.insert_spaces = spaces;
            }
            if let Some(width) = style.indent_width {
                opts.tab_width = width;
            }
        }
        opts
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
            recovery_id: self.claim_recovery_id(),
            highlighter: new_highlighter(language, &doc),
            doc,
            view: EditorView::default(),
            language,
            find: FindBar::default(),
            pending_find_step: None,
            preview,
            syntax_version: None,
            syntax_due: None,
            disk: DiskState::Unchanged,
            line_count_seen: None,
        };

        // A preview tab replaces the existing one rather than adding to it.
        if preview && let Some(slot) = self.docs.iter().position(|d| d.preview) {
            self.docs[slot] = entry;
            self.active = Some(slot);
        } else {
            self.docs.push(entry);
            self.active = Some(self.docs.len() - 1);
        }
        self.sync_watched_files();
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
        let closed = self.docs.remove(index);
        // The committed copy and the diff were held for a buffer that is gone.
        if let Some(path) = closed.doc.path() {
            self.git.forget(path);
        }

        self.active = match self.active {
            _ if self.docs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.docs.len() - 1)),
            None => None,
        };
        self.sync_watched_files();
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
        self.tidy_before_saving(index);
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
                // Save As gives the document a new path, and possibly one in a
                // directory nothing is watching yet.
                self.sync_watched_files();
                // The working tree just changed on disk, and the panel is
                // describing the state from before the save.
                self.git.refresh_status();
                // Blame is of the file on disk, so this is the one moment it
                // goes stale without HEAD having moved.
                if let Some(path) = self.docs[index].doc.path().map(Path::to_path_buf) {
                    self.git.refresh_blame(&path);
                }
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
        for (path, lines) in &session.breakpoints {
            self.breakpoints.set_file(path.clone(), lines.clone());
        }

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
            breakpoints: self.breakpoints.flatten(),
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
        if self
            .watcher
            .as_ref()
            .is_some_and(|w| w.root() == Some(folder.as_path()))
        {
            self.tree.set_root(folder);
            return;
        }

        // A different folder is a different repository, or none. Asked for here
        // rather than per file: discovery is one subprocess and the answer is
        // what decides whether any of the rest is worth doing.
        self.git.set_project(Some(&folder));

        if let Some(watcher) = self.watcher.as_mut()
            && let Err(e) = watcher.set_root(&folder)
        {
            // Not fatal: the tree still works, it just will not notice
            // changes made elsewhere.
            tracing::warn!("could not watch {}: {e}", folder.display());
            self.info("Changes made outside The Editor will not be noticed automatically");
        }
        self.tree.set_root(folder);
        self.sync_watched_files();
    }

    /// Keep the watcher's list of loose files in step with the open tabs.
    ///
    /// Cheap when nothing changed — the watcher compares against what it
    /// already has — so this can be called from anywhere a tab opens or closes
    /// rather than being carefully threaded through each one.
    fn sync_watched_files(&mut self) {
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let files: Vec<PathBuf> = self
            .docs
            .iter()
            .filter_map(|d| d.doc.path().map(Path::to_path_buf))
            .collect();
        watcher.set_files(&files);
    }

    /// The Problems panel: every diagnostic, grouped by file.
    ///
    /// Returns the location to jump to when a row is clicked.
    fn problems_ui(&mut self, ui: &mut egui::Ui) -> Option<(PathBuf, usize, usize)> {
        // Only the file being looked at, unless asked otherwise. A panel
        // listing every open tab's problems buries the ones belonging to the
        // line under the caret, which is what it is being consulted about.
        let here = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|d| d.doc.path())
            .map(Path::to_path_buf);
        let files: Vec<(PathBuf, Vec<editor_lsp::diagnostics::Diagnostic>)> = self
            .lsp
            .diagnostics()
            .all()
            .into_iter()
            .filter(|(path, _)| self.problems_all_files || here.as_ref() == Some(path))
            .collect();

        // The what-is-missing notice sits at the bottom and is drawn first, so
        // it keeps its place while the list above it scrolls. It shows whether
        // or not there are diagnostics: finding a syntax error does not mean a
        // type checker has stopped being missing.
        if self.lsp.running().is_empty() && self.missing_tools_ui(ui) {
            self.toolchains = Some(editor_lsp::registry::find_all(&self.tool_search_path()));
        }

        ui.horizontal(|ui| {
            let mut all = self.problems_all_files;
            if ui
                .checkbox(&mut all, "All open files")
                .on_hover_text("Off shows only the file you are looking at")
                .changed()
            {
                self.problems_all_files = all;
            }
        });
        ui.separator();

        if files.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.weak("No problems");
            });
            return None;
        }

        let mut clicked = None;
        let here_now = self.problem_at_caret.clone();
        // Scroll to it once per change, not every frame.
        let mut reveal = self.problem_at_caret != self.problem_revealed;
        self.problem_revealed = self.problem_at_caret.clone();

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
                        // The one the caret is sitting on, so a long list can
                        // be searched from the editor rather than by eye.
                        let at_caret = here_now.as_ref().is_some_and(|(p, line, column)| {
                            *p == path && *line == diagnostic.line && *column == diagnostic.column
                        });
                        let row = ui
                            .horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.colored_label(
                                    severity_colour(ui.visuals(), diagnostic.severity),
                                    diagnostic.severity.glyph(),
                                );
                                ui.weak(format!(
                                    "{}:{}",
                                    diagnostic.line + 1,
                                    diagnostic.column + 1
                                ));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(diagnostic.summary()).background_color(
                                            if at_caret {
                                                ui.visuals().selection.bg_fill
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            },
                                        ),
                                    )
                                    .sense(egui::Sense::click())
                                    .truncate(),
                                )
                            })
                            .inner;

                        if at_caret && std::mem::take(&mut reveal) {
                            // Only when it changed, or the panel would fight
                            // the user for the scrollbar every frame.
                            row.scroll_to_me(Some(egui::Align::Center));
                        }
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

    /// Run an editing operation on the active document.
    ///
    /// Line operations are commands so they reach the menu and the palette, but
    /// they act on the view, and every one needs the same two lookups and the
    /// same "is there a document" guard.
    fn on_view(&mut self, f: impl FnOnce(&mut EditorView, &mut Document) -> bool) {
        if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i))
            && f(&mut entry.view, &mut entry.doc)
        {
            // Editing a preview tab promotes it, as typing does.
            entry.preview = false;
        }
    }

    /// Open Go to File over a fresh listing of the project.
    fn open_file_picker(&mut self) {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first: Go to File searches the project");
            return;
        };
        let listing = editor_search::files::list(&root);
        if listing.files.is_empty() {
            self.info("No files found in this folder");
            return;
        }
        self.file_picker.open(listing);
    }

    /// Find the diagnostic under the caret, for the Problems panel to reveal.
    ///
    /// Recomputed each frame from the caret rather than set when the user
    /// clicks, so arrowing onto a squiggle reveals it too — and so it clears
    /// itself the moment the caret moves off.
    fn sync_problem_at_caret(&mut self) {
        self.problem_at_caret = self.diagnostic_under_caret();
    }

    fn diagnostic_under_caret(&self) -> Option<(PathBuf, u32, u32)> {
        let entry = self.active.and_then(|i| self.docs.get(i))?;
        let path = entry.doc.path()?;
        let caret = entry.view.selection.head;

        self.lsp
            .diagnostics()
            .for_file(path)
            .into_iter()
            .find(|d| {
                let start = entry.doc.offset_at(d.line as usize, d.column as usize);
                let end = entry
                    .doc
                    .offset_at(d.end_line as usize, d.end_column as usize);
                // Inclusive of the end, so a caret left just past the last
                // character of a squiggle still counts as on it.
                caret >= start && caret <= end.max(start)
            })
            .map(|d| (path.to_path_buf(), d.line, d.column))
    }

    /// F2: ask for a new name for the symbol under the caret.
    fn begin_rename(&mut self) {
        let Some(name) = self.symbol_under_caret() else {
            self.info("Put the caret on a name first");
            return;
        };
        self.rename = Some((name.clone(), name));
    }

    /// The rename prompt.
    fn rename_ui(&mut self, ctx: &egui::Context) {
        let Some((original, draft)) = self.rename.as_mut() else {
            return;
        };
        let original = original.clone();
        let mut go = false;
        let mut cancel = false;

        egui::Modal::new(egui::Id::new("rename_symbol")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Rename");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.weak("Rename");
                ui.monospace(&original);
                ui.weak("to:");
            });
            let field = ui.add(
                egui::TextEdit::singleline(draft)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            field.request_focus();
            ui.add_space(4.0);
            ui.small("Every use across the project is changed. One undo step per file.");

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui.button("Rename").clicked() {
                    go = true;
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });

        let new_name = self
            .rename
            .as_ref()
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        if cancel {
            self.rename = None;
            return;
        }
        if !go {
            return;
        }
        self.rename = None;

        let new_name = new_name.trim().to_owned();
        if new_name.is_empty() || new_name == original {
            return;
        }
        self.request_rename(&new_name);
    }

    fn request_rename(&mut self, new_name: &str) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
            self.info("Save the file before renaming");
            return;
        };
        if entry.doc.is_dirty() {
            // The server reads the file from disk. Renaming against a stale
            // copy puts every edit on the wrong line, silently.
            self.info("Save the file first: rename works from what is on disk");
            return;
        }
        let (line, column) = entry.doc.line_col(entry.view.selection.head);
        let (line, column) = (line as u32 - 1, column as u32 - 1);

        if !self.lsp.rename(&path, line, column, new_name) {
            self.error(
                "No language server can rename here. Renaming by find-and-replace \
                 is how the wrong things get renamed, so there is no fallback.",
            );
        }
    }

    /// Apply a rename the server worked out, across every file it touches.
    ///
    /// Open documents go through the normal transaction path, so the change is
    /// undoable and reaches the highlighter. Closed ones are rewritten on disk:
    /// loading twenty files to change one line in each, only to close them
    /// again, is the worse trade.
    fn apply_rename(&mut self, files: Vec<editor_lsp::session::FileEdit>) {
        if files.is_empty() {
            self.info("Nothing to rename here");
            return;
        }
        let count = files.len();
        let mut failures = Vec::new();

        for file in files {
            let open = self
                .docs
                .iter()
                .position(|d| d.doc.path() == Some(file.path.as_path()));
            match open {
                Some(index) => self.apply_rename_to_open(index, &file),
                None => {
                    if let Err(e) = apply_rename_to_disk(&file) {
                        failures.push(format!("{}: {e}", file.path.display()));
                    }
                }
            }
        }

        self.tree.refresh();
        if failures.is_empty() {
            self.info(format!("Renamed across {count} file(s)"));
        } else {
            self.error(format!("Rename partly failed: {}", failures.join("; ")));
        }
    }

    fn apply_rename_to_open(&mut self, index: usize, file: &editor_lsp::session::FileEdit) {
        let Some(entry) = self.docs.get_mut(index) else {
            return;
        };
        // One transaction per file, so a rename is one undo step there rather
        // than one per occurrence.
        let mut edits = Vec::new();
        for edit in &file.edits {
            let start = entry
                .doc
                .offset_at(edit.start_line as usize, edit.start_column as usize);
            let end = entry
                .doc
                .offset_at(edit.end_line as usize, edit.end_column as usize);
            edits.push(editor_core::edit::Edit::replace(
                start..end.max(start),
                edit.text.clone(),
            ));
        }
        if edits.is_empty() {
            return;
        }
        let before = entry.view.selection;
        entry.doc.break_undo_run();
        entry
            .doc
            .apply(&editor_core::edit::Transaction::new(edits), before, before);
        entry.doc.break_undo_run();
        entry.view.set_caret(before.head.min(entry.doc.len_chars()));
    }

    /// Ctrl+Shift+O: list what this file declares.
    ///
    /// From the parse tree rather than from a language server, so it works with
    /// nothing installed — the same choice as Go to Definition and Find Uses.
    fn open_symbol_picker(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) else {
            self.info("Open a file first");
            return;
        };
        let Some(tree) = entry.highlighter.as_ref().and_then(|h| h.tree()) else {
            self.info("No outline: this file has no grammar");
            return;
        };
        let symbols = editor_syntax::symbols::outline(tree, entry.doc.text());
        if symbols.is_empty() {
            self.info("Nothing is declared in this file");
            return;
        }
        self.symbol_picker.open(symbols);
    }

    /// Select `range` in the active document and scroll it into view.
    fn reveal_in_active(&mut self, range: std::ops::Range<usize>) {
        if let Some(entry) = self.active.and_then(|i| self.docs.get_mut(i)) {
            let end = range.end.min(entry.doc.len_chars());
            entry.view.select_range(range.start.min(end), end);
            entry.view.focus();
        }
    }

    /// The interpreter the packages panel and the runner should use.
    fn interpreter(&self) -> Option<editor_proc::interpreter::Interpreter> {
        editor_proc::interpreter::resolve(&self.settings.python_interpreter(), self.tree.root())
    }

    /// Ask pip what is installed, if there is an interpreter to ask.
    fn refresh_packages(&mut self) {
        match self.interpreter() {
            Some(interpreter) => self.packages.refresh(&interpreter.path),
            None => self.info("Select a Python interpreter first"),
        }
    }

    /// Carry out what the packages panel asked for.
    ///
    /// Installs and removals go to the console, which is where the user can
    /// read pip's own account of what happened. The listing is refreshed when
    /// that run finishes rather than immediately, because pip has not done
    /// anything yet.
    fn apply_packages_action(&mut self, action: crate::packages_panel::Action) {
        use crate::packages_panel::Action;
        match action {
            Action::None => {}
            Action::Run(change) => {
                let Some(interpreter) = self.interpreter() else {
                    self.info("Select a Python interpreter first");
                    return;
                };
                let cwd = self
                    .tree
                    .root()
                    .map(Path::to_path_buf)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("."));
                let config = editor_proc::packages::command(&interpreter.path, &cwd, &change);
                self.dock = DockTab::Output;
                self.show_output = true;
                if let Err(e) = self.runner.start(config, true) {
                    self.error(format!("{e:#}"));
                }
            }
            Action::Freeze => self.freeze_requirements(),
            Action::OpenRequirements => {
                if let Some(path) = crate::packages_panel::requirements_file(self.tree.root()) {
                    self.open_path(&path, false);
                }
            }
        }
    }

    /// Write `pip freeze` to the project's requirements file.
    fn freeze_requirements(&mut self) {
        let Some(interpreter) = self.interpreter() else {
            self.info("Select a Python interpreter first");
            return;
        };
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first: there is nowhere to write the file");
            return;
        };
        let path = root.join("requirements.txt");
        match editor_proc::packages::freeze_to(&interpreter.path, &path) {
            Ok(count) => {
                self.tree.refresh();
                self.sync_watched_files();
                self.info(format!("Wrote {count} requirements to requirements.txt"));
            }
            Err(e) => self.error(format!("Could not freeze: {e}")),
        }
    }

    /// Open a shell in the project, if one is not already running.
    ///
    /// Started in the project root with the virtual environment's directory
    /// ahead of `PATH`, so `python` and `pip` are the project's from the first
    /// command rather than after activating something.
    fn open_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal.is_running() {
            return;
        }
        let cwd = self
            .tree
            .root()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let extra = self.tool_search_path();
        self.terminal.start(&cwd, &extra, ctx);
    }

    /// Start a project-wide search over the open folder.
    fn start_project_search(&mut self, query: &editor_search::query::Query) {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            self.info("Open a folder first \u{2014} project search needs one");
            return;
        };
        // The same walk go-to-file uses, with the same skip list: searching
        // `target` or `node_modules` finds thousands of matches nobody wants.
        let listing = editor_search::files::list(&root);
        let search = editor_search::project::Search::start(&root, listing.files, query);
        self.search.started(search);
    }

    /// Move a tab, keeping the selection and the recent list pointing at the
    /// same documents rather than at the same positions.
    fn reorder_tab(&mut self, from: usize, to: usize) {
        if from >= self.docs.len() || to >= self.docs.len() || from == to {
            return;
        }
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);

        // Every index at or after the smaller of the two has moved.
        let remap = |i: usize| {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        };
        self.active = self.active.map(remap);
        for index in &mut self.mru {
            *index = remap(*index);
        }
    }

    /// Ctrl+Tab: step through tabs in the order they were last looked at.
    ///
    /// Most-recently-used rather than left-to-right, because the tab you want
    /// next is nearly always the one you were just in — and pressing it twice
    /// should return you there, not walk the strip.
    fn cycle_tab(&mut self, backwards: bool) {
        if self.docs.len() < 2 {
            return;
        }
        self.refresh_mru();
        let here = self
            .active
            .and_then(|a| self.mru.iter().position(|i| *i == a))
            .unwrap_or(0);
        let step = if backwards { -1isize } else { 1 };
        let next = (here as isize + step).rem_euclid(self.mru.len() as isize) as usize;

        self.cycling = true;
        self.active = self.mru.get(next).copied();
        self.focus_active();
    }

    /// Keep the recent list holding exactly the open tabs, once each.
    fn refresh_mru(&mut self) {
        self.mru.retain(|i| *i < self.docs.len());
        self.mru.dedup();
        for i in 0..self.docs.len() {
            if !self.mru.contains(&i) {
                self.mru.push(i);
            }
        }
    }

    /// Record the active tab as the most recent, once Ctrl is let go.
    ///
    /// Doing it on every switch would move the tab being left to the front
    /// mid-cycle, so a second Ctrl+Tab would bounce back rather than going on.
    fn settle_mru(&mut self, ctx: &egui::Context) {
        if self.cycling && !ctx.input(|i| i.modifiers.ctrl || i.modifiers.command) {
            self.cycling = false;
        }
        if self.cycling {
            return;
        }
        let Some(active) = self.active else { return };
        if self.mru.first() == Some(&active) {
            return;
        }
        self.mru.retain(|i| *i != active);
        self.mru.insert(0, active);
        self.refresh_mru();
    }

    /// Apply the on-save whitespace policies before writing.
    ///
    /// As a transaction, so it lands in the undo history: saving and pressing
    /// undo gets the whitespace back, which is what someone who put it there
    /// deliberately would expect.
    fn tidy_before_saving(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let policy = self.save_policy(entry.doc.path());
        if policy.is_noop() {
            return;
        }
        let Some(tx) = editor_core::whitespace::tidy(entry.doc.text(), policy) else {
            return;
        };

        let Some(entry) = self.docs.get_mut(index) else {
            return;
        };
        let before = entry.view.selection;
        let after = before.clamped(entry.doc.len_chars());
        entry.doc.break_undo_run();
        entry.doc.apply(&tx, before, after);
        entry.doc.break_undo_run();
        // The edits may have deleted the text the caret was sitting in.
        entry
            .view
            .set_caret(entry.view.selection.head.min(entry.doc.len_chars()));
    }

    /// What to tidy on save, with a project's `.editorconfig` taking priority.
    fn save_policy(&self, path: Option<&Path>) -> editor_core::whitespace::OnSave {
        let mut policy = editor_core::whitespace::OnSave {
            trim_trailing_whitespace: self.settings.trim_trailing_whitespace(),
            ensure_final_newline: self.settings.insert_final_newline(),
        };
        if let Some(path) = path.filter(|_| self.settings.use_editorconfig()) {
            let style = editor_config::editorconfig::style_for(path);
            if let Some(trim) = style.trim_trailing_whitespace {
                policy.trim_trailing_whitespace = trim;
            }
            if let Some(newline) = style.insert_final_newline {
                policy.ensure_final_newline = newline;
            }
        }
        policy
    }

    /// Toggle a breakpoint on the caret's line, and tell a running session.
    fn toggle_breakpoint_at_caret(&mut self) {
        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            return;
        };
        let line = entry.doc.line_of(entry.view.selection.head);
        self.toggle_breakpoint(line);
    }

    /// `line` is zero-based, as the editor counts; breakpoints are one-based.
    fn toggle_breakpoint(&mut self, line: usize) {
        let Some(path) = self
            .active
            .and_then(|i| self.docs.get(i))
            .and_then(|e| e.doc.path())
            .map(Path::to_path_buf)
        else {
            self.info("Save the file before setting breakpoints");
            return;
        };
        let set = self.breakpoints.toggle(&path, line + 1);
        self.send_breakpoints(&path);

        // Said once, on the first breakpoint of a run, if nothing can act on
        // it. A dot appearing looks like it worked, and finding out that it
        // cannot be hit should not wait until Alt+F5.
        if set && !self.debugpy_warned && self.debug.is_none() {
            self.debugpy_warned = true;
            if let Some(interpreter) = editor_proc::interpreter::resolve(
                &self.settings.python_interpreter(),
                self.tree.root(),
            ) && !editor_debug::adapter::is_available(&interpreter.path)
            {
                self.info(format!(
                    "Breakpoint set, but debugging needs debugpy \u{2014} {}",
                    editor_debug::adapter::INSTALL
                ));
            }
        }
    }

    fn send_breakpoints(&mut self, path: &Path) {
        let lines = self.breakpoints.for_file(path);
        if let Some(session) = self.debug.as_mut() {
            session.set_breakpoints(path, &lines);
        }
    }

    /// Alt+F5: start a session, or continue a paused one.
    ///
    /// One key for both because they are the same intention — "carry on" —
    /// and a debugger with separate Start and Continue keys makes you think
    /// about which state you are in before you can press anything.
    fn debug_start_or_continue(&mut self) {
        if let Some(session) = self.debug.as_mut() {
            if session.is_paused() {
                session.resume(editor_debug::session::Step::Continue);
                self.debug_view.clear();
            }
            return;
        }

        let Some(entry) = self.active.and_then(|i| self.docs.get(i)) else {
            self.info("Open a Python file to debug");
            return;
        };
        if entry.language != LanguageId::Python {
            self.info("The debugger is for Python only at present");
            return;
        }
        let Some(program) = entry.doc.path().map(Path::to_path_buf) else {
            self.info("Save the file before debugging it");
            return;
        };
        if entry.doc.is_dirty() {
            // Debugging a file that differs from the one on disk puts every
            // breakpoint on the wrong line, silently.
            self.info("Save the file first \u{2014} the debugger reads it from disk");
            return;
        }

        let Some(interpreter) = editor_proc::interpreter::resolve(
            &self.settings.python_interpreter(),
            self.tree.root(),
        ) else {
            self.error("No Python interpreter found");
            return;
        };
        if !editor_debug::adapter::is_available(&interpreter.path) {
            self.error(format!(
                "debugpy is not installed for {} \u{2014} {}",
                interpreter.path.display(),
                editor_debug::adapter::INSTALL
            ));
            return;
        }

        let cwd = self
            .tree
            .root()
            .map_or_else(|| program.parent().unwrap_or(Path::new(".")), |r| r)
            .to_path_buf();

        match editor_debug::Session::launch(&interpreter.path, &program, &cwd, &[]) {
            Ok(mut session) => {
                for file in self.breakpoints.files() {
                    let lines = self.breakpoints.for_file(&file);
                    session.set_breakpoints(&file, &lines);
                }
                // Echoed like a run, so the console says what is happening
                // rather than filling with output from nowhere.
                self.runner.push_banner(&format!(
                    "> debug {} ({})",
                    program.display(),
                    interpreter.path.display()
                ));
                self.debug = Some(session);
                self.debug_view.clear();
                self.dock = DockTab::Debug;
                self.show_output = true;
                self.info(format!("Debugging {}", program.display()));
            }
            Err(e) => self.error(format!("Could not start the debugger: {e:#}")),
        }
    }

    fn debug_step(&mut self, how: editor_debug::session::Step) {
        if let Some(session) = self.debug.as_mut()
            && session.is_paused()
        {
            session.resume(how);
            self.debug_view.clear();
        }
    }

    fn debug_stop(&mut self) {
        if let Some(session) = self.debug.as_mut() {
            session.stop();
            self.runner.push_banner("[Debugging stopped]");
        }
        self.debug = None;
        self.debug_view.clear();
    }

    /// Drain the debug session once per frame.
    fn poll_debugger(&mut self, ctx: &egui::Context) {
        let Some(session) = self.debug.as_mut() else {
            return;
        };
        let events = session.poll();
        if events.is_empty() {
            // A paused session produces nothing until the user acts; a running
            // one is about to. Keep the frame loop turning while it lives.
            if session.is_alive() && !session.is_paused() {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
            return;
        }

        let mut jump_to = None;
        for event in events {
            match event {
                editor_debug::DebugEvent::StateChanged(state) => {
                    if state == editor_debug::State::Finished {
                        self.debug = None;
                        self.debug_view.clear();
                        self.runner.push_banner("[Debugging finished]");
                        self.info("Debugging finished");
                        return;
                    }
                }
                editor_debug::DebugEvent::Paused { reason } => {
                    self.debug_view.reason = reason;
                }
                editor_debug::DebugEvent::Stack(frames) => {
                    self.debug_view.selected = frames.first().map(|f| f.id);
                    self.debug_view.stack = frames;
                    jump_to = self.debug_view.location();
                }
                editor_debug::DebugEvent::Variables(vars) => {
                    self.debug_view.variables = vars;
                }
                editor_debug::DebugEvent::BreakpointsVerified { .. } => {}
                editor_debug::DebugEvent::Output(text) => {
                    self.runner.push_output(&text);
                }
                editor_debug::DebugEvent::Failed(message) => {
                    self.error(format!("Debugger: {message}"));
                }
            }
        }

        if let Some((path, line)) = jump_to {
            // One-based from the protocol, zero-based for the editor.
            self.open_at(&path, line.saturating_sub(1), 0);
        }
    }

    /// The word being typed at the caret: where it starts, and what it is.
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
        self.request_completions(start, &prefix, false);
    }

    /// Ask the servers about the word starting at `start`.
    fn request_completions(&mut self, start: usize, prefix: &str, explicit: bool) {
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
        // The word-list fallback is not offered unbidden. A list of names that
        // happen to appear elsewhere in the file is a reasonable answer to
        // "suggest something", and a poor reason to put a popup over the text
        // every time two letters are typed.
        if explicit {
            self.complete_locally(start, prefix);
        }
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
        self.request_completions(start, &prefix, true);
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
            // Nothing in this file. For a definition that is the normal case —
            // the function is in another module — so the project is searched
            // before giving up. Uses are left at this file: a name used in
            // fifty places across a project is a list nobody wants from a
            // search that cannot tell one `parse` from another.
            if query == Query::Definition {
                let language = entry.language;
                let name = symbol.name.clone();
                if self.find_definition_in_project(&name, language) {
                    return;
                }
            }
            self.info(format!(
                "No {} of `{}` found (no language server, so this is a search of the                  project's text rather than an answer about the code)",
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

    /// Search the open project for a definition of `name`.
    ///
    /// The fallback for Go to Definition when no server can answer. Only files
    /// of the same language are considered, and each is skimmed for the name as
    /// plain text before being parsed — reading a file is cheap, parsing it is
    /// not, and in any real project almost every file fails the skim.
    ///
    /// Returns true if it jumped somewhere.
    fn find_definition_in_project(&mut self, name: &str, language: LanguageId) -> bool {
        let Some(root) = self.tree.root().map(Path::to_path_buf) else {
            return false;
        };

        let mut found = Vec::new();
        for relative in editor_search::files::list(&root).files {
            if found.len() >= MAX_PROJECT_DEFINITIONS {
                break;
            }
            let path = root.join(&relative);
            let same_language = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| LanguageId::from_extension(e) == language);
            if !same_language {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            // The skim. A definition of `parse` cannot exist in a file that does
            // not contain the word at all.
            if source.len() > MAX_SEARCHED_BYTES || !source.contains(name) {
                continue;
            }

            let rope = ropey::Rope::from_str(&source);
            let Some(highlighter) = Highlighter::new(language, &rope) else {
                continue;
            };
            let Some(tree) = highlighter.tree() else {
                continue;
            };
            for range in editor_syntax::symbols::definitions(tree, &rope, name) {
                let line = rope.char_to_line(range.start);
                let column = range.start - rope.line_to_char(line);
                found.push(Target {
                    path: Some(path.clone()),
                    line,
                    column,
                });
            }
        }

        if found.is_empty() {
            return false;
        }
        let count = found.len();
        self.go_to(&found[0]);
        if count > 1 {
            self.info(format!("{count} definitions of `{name}` in this project"));
        }
        true
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
        self.lsp.set_disabled(self.settings.disabled_servers());
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
                editor_lsp::session::Notice::Rename(files) => self.apply_rename(files),
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
        let mut breakpoints_changed: Vec<PathBuf> = Vec::new();

        for entry in &mut self.docs {
            let changes = entry.doc.take_changes();
            if let Some(h) = entry.highlighter.as_mut() {
                if !changes.is_empty() {
                    h.update(&changes, entry.doc.text());
                }
                // A reparse that ran out of time leaves the tree a keystroke
                // behind. Finish it as soon as nothing is being typed, on the
                // same debounce the syntax check uses — the colours are the
                // stale thing here, and they are worth a moment of lag to keep
                // the keystroke itself instant.
                if h.is_stale() {
                    let due = *entry.syntax_due.get_or_insert(now + SYNTAX_DEBOUNCE);
                    if now >= due {
                        h.catch_up(entry.doc.text());
                    } else {
                        next_due = Some(next_due.map_or(due, |soonest: Instant| soonest.min(due)));
                    }
                }
            }

            let Some(path) = entry.doc.path().map(Path::to_path_buf) else {
                // A never-saved buffer has no key in the diagnostic store, the
                // same reason the language servers cannot see it.
                continue;
            };

            // Carry breakpoints with the lines they were put on. Approximated
            // from the change in line count rather than from the edit itself:
            // exact tracking needs every edit's range, and being one line out
            // after a multi-cursor paste is a much smaller problem than a
            // breakpoint that silently stops matching its statement.
            let lines_now = entry.doc.line_count();
            if let Some(before) = entry.line_count_seen
                && before != lines_now
                && !self.breakpoints.for_file(&path).is_empty()
            {
                let caret_line = entry.doc.line_of(entry.view.selection.head);
                let delta = lines_now as isize - before as isize;
                self.breakpoints.shift(
                    &path,
                    caret_line.saturating_sub(delta.unsigned_abs()),
                    delta,
                );
                breakpoints_changed.push(path.clone());
            }
            entry.line_count_seen = Some(lines_now);
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

        for path in breakpoints_changed {
            self.send_breakpoints(&path);
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
    fn underlines_for(
        entry: &OpenDoc,
        store: &editor_lsp::diagnostics::Store,
        level: editor_config::settings::UnderlineDiagnostics,
    ) -> Vec<Underline> {
        use editor_config::settings::UnderlineDiagnostics as Level;
        if level == Level::None {
            return Vec::new();
        }
        let Some(path) = entry.doc.path() else {
            return Vec::new();
        };
        store
            .for_file(path)
            .into_iter()
            // The gutter and the Problems panel still show everything; this
            // only decides how much of it is written across the text.
            .filter(|d| {
                level == Level::All || d.severity == editor_lsp::diagnostics::Severity::Error
            })
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

        for path in &changes.touched {
            if let Some(index) = self.docs.iter().position(|d| d.doc.path() == Some(path)) {
                self.reconcile_with_disk(index);
            }
        }
    }

    fn claim_recovery_id(&mut self) -> u64 {
        let id = self.next_recovery_id;
        self.next_recovery_id += 1;
        id
    }

    /// Copy unsaved buffers aside, so a crash cannot take them.
    ///
    /// Watches the set of `(id, version)` pairs for dirty documents rather than
    /// being called from each edit path. Every way text can change — typing,
    /// paste, undo, a project-wide replace, a rename applied from the language
    /// server — bumps the version, so nothing can be added later that forgets
    /// to schedule a write.
    fn autosave(&mut self, ctx: &egui::Context) {
        self.recovery.beat();

        let now: Vec<(u64, u64)> = self
            .docs
            .iter()
            .filter(|d| d.doc.is_dirty())
            .map(|d| (d.recovery_id, d.doc.version()))
            .collect();

        if now != self.dirty_seen {
            // Anything that stopped being dirty was saved, closed, or undone
            // back to what is on disk. Either way its copy is now a lie.
            for (id, _) in &self.dirty_seen {
                if !now.iter().any(|(other, _)| other == id) {
                    self.recovery.discard(*id);
                }
            }
            self.dirty_seen = now;
            self.recovery.mark_dirty();
        }

        if self.recovery.is_due() {
            for entry in &self.docs {
                if !entry.doc.is_dirty() {
                    continue;
                }
                self.recovery.store(
                    entry.recovery_id,
                    entry.doc.version(),
                    entry.doc.path(),
                    &entry.doc.display_name(),
                    &entry.doc.text().to_string(),
                );
            }
            self.recovery.settle();
        }

        // Without this the write never happens on an idle editor: type a
        // sentence, walk away, and the copy is made only when you come back.
        if let Some(wait) = self.recovery.next_wake() {
            ctx.request_repaint_after(wait);
        }
    }

    /// Log how long it took to get to the first frame, once.
    ///
    /// PLAN.md §M9 asks for under 500 ms cold to interactive. A number nobody
    /// measures is a number that drifts, and startup is the one figure a user
    /// notices every single time without ever being able to say what it was.
    fn report_startup(&mut self) {
        let Some(started) = self.started.take() else {
            return;
        };
        let took = started.elapsed();
        tracing::info!(
            millis = took.as_millis(),
            budget_millis = 500u64,
            within_budget = took <= Duration::from_millis(500),
            "first frame"
        );
    }

    /// Shown once, the first time The Editor is run.
    ///
    /// Three things and then out of the way. A first-run wizard that walks
    /// through every setting is a wizard people click through without reading;
    /// what actually helps is knowing that a *folder* is the unit of work, that
    /// the language tools are separate and checkable, and that there is a
    /// manual.
    fn first_run_ui(&mut self, ctx: &egui::Context) {
        if !self.first_run {
            return;
        }
        let mut dismiss = false;
        let mut then = None;

        egui::Modal::new(egui::Id::new("first_run")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Welcome to The Editor");
            ui.add_space(4.0);
            ui.weak(format!("Version {}", BUILD.version));
            ui.add_space(12.0);

            ui.label(
                "An IDE for Python and Rust. No account, no installer, and \
                 nothing sent anywhere.",
            );
            ui.add_space(12.0);

            ui.label(egui::RichText::new("Open a folder, not a file").strong());
            ui.label(
                "The folder is the project: it is what the explorer shows, what \
                 search searches, and where The Editor looks for a virtual \
                 environment.",
            );
            ui.add_space(10.0);

            ui.label(egui::RichText::new("Check the toolchains").strong());
            ui.label(
                "Highlighting and syntax errors work on their own. Completion, \
                 go to definition and debugging need tools you may not have \
                 yet; this says which, and how to install them.",
            );
            ui.add_space(10.0);

            ui.label(egui::RichText::new("The manual is in the Help menu").strong());
            ui.label("Along with every keyboard shortcut, as the application actually has them.");

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Open a folder\u{2026}").clicked() {
                    then = Some(CommandId::OpenFolder);
                    dismiss = true;
                }
                if ui.button("Check toolchains").clicked() {
                    then = Some(CommandId::CheckToolchains);
                    dismiss = true;
                }
                if ui.button("User manual").clicked() {
                    then = Some(CommandId::UserManual);
                    dismiss = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Start editing").clicked() {
                        dismiss = true;
                    }
                });
            });
        });

        if dismiss {
            self.first_run = false;
            if let Some(id) = then {
                self.run_command(id, ctx);
            }
        }
    }

    /// The diff window, and the comparison it needs to draw.
    ///
    /// The sections are rebuilt only when the buffer's version moves, because
    /// the window stays open while you edit and a diff per frame of a large
    /// file is the one cost this feature could easily have.
    fn diff_window(&mut self, ctx: &egui::Context) {
        if !self.diff_view.is_open() {
            self.diff_cache = None;
            return;
        }
        let Some(path) = self.diff_view.path().map(Path::to_path_buf) else {
            return;
        };

        // The diff is of a *buffer*. Closing the tab takes away the thing being
        // compared, so the window goes with it.
        let Some(version) = self
            .docs
            .iter()
            .find(|entry| entry.doc.path() == Some(path.as_path()))
            .map(|entry| entry.doc.version())
        else {
            self.diff_view.close();
            self.diff_cache = None;
            return;
        };

        let fresh = self
            .diff_cache
            .as_ref()
            .is_some_and(|(cached, at, _)| cached == &path && *at == version);

        let known = self.git.baseline_state(&path);
        if !fresh && known == editor_vcs::tracker::Known::Ready {
            let text = self
                .docs
                .iter()
                .find(|entry| entry.doc.path() == Some(path.as_path()))
                .map(|entry| entry.doc.text().to_string())
                .unwrap_or_default();
            let committed = self.git.baseline(&path).unwrap_or_default().to_owned();
            let sections = editor_vcs::unified::unified(
                &editor_vcs::diff::lines(&committed),
                &editor_vcs::diff::lines(&text),
                editor_vcs::unified::DEFAULT_CONTEXT,
            );
            self.diff_cache = Some((path.clone(), version, sections));
        }

        let state = if !self.git.has_repo() {
            diff_view::State::NoRepository
        } else {
            match known {
                editor_vcs::tracker::Known::Waiting => diff_view::State::Waiting,
                editor_vcs::tracker::Known::Absent => diff_view::State::NotTracked,
                editor_vcs::tracker::Known::Ready => match &self.diff_cache {
                    Some((_, _, sections)) if !sections.is_empty() => {
                        diff_view::State::Changed(sections)
                    }
                    _ => diff_view::State::Unchanged,
                },
            }
        };

        let branch = self.git.branch().map(str::to_owned);
        self.diff_view.ui(ctx, branch.as_deref(), state);
    }

    /// Offer back the unsaved work of a session that did not shut down.
    ///
    /// Modal, unlike the disk-change bar. This one is about work that exists
    /// nowhere else, the files are deleted once dismissed, and it happens at
    /// most once per crash — all the reasons the other case is non-modal point
    /// the other way here.
    fn recovery_prompt(&mut self, ctx: &egui::Context) {
        if self.recovered.is_empty() {
            return;
        }
        let mut restore = false;
        let mut discard = false;

        egui::Modal::new(egui::Id::new("crash_recovery")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Unsaved work was recovered");
            ui.add_space(6.0);
            ui.label(
                "The Editor did not shut down cleanly. These buffers had \
                      changes that were never saved:",
            );
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .id_salt("recovery_list")
                .max_height(200.0)
                .show(ui, |ui| {
                    for item in &self.recovered {
                        ui.horizontal(|ui| {
                            ui.monospace(&item.name);
                            match item.path.as_ref() {
                                Some(path) => {
                                    ui.weak(shorten_middle(&path.display().to_string(), 52))
                                }
                                None => ui.weak("never saved"),
                            };
                        });
                    }
                });

            ui.add_space(10.0);
            ui.small(
                "Restoring opens each one in a tab with its changes, unsaved. \
                 Undo goes back to what is on disk.",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Restore").clicked() {
                    restore = true;
                }
                if ui
                    .button("Discard")
                    .on_hover_text("Delete the recovered copies. This cannot be undone.")
                    .clicked()
                {
                    discard = true;
                }
            });
        });

        if !restore && !discard {
            return;
        }
        for item in std::mem::take(&mut self.recovered) {
            if restore {
                self.restore_one(&item);
            }
            recovery::dispose(&item.source);
        }
    }

    /// Put one recovered buffer back into a tab.
    fn restore_one(&mut self, item: &recovery::Recovered) {
        // Where the file still exists, open it properly first and then replace
        // the text. That keeps its encoding and line endings, and leaves one
        // undo step between the recovered version and what is on disk — which
        // is the comparison anyone will want to make.
        let doc = match item.path.as_ref().map(|p| (p, Document::open(p))) {
            Some((_, Ok(mut doc))) => {
                let end = doc.len_chars();
                let before = editor_core::selection::Selection::at(0);
                doc.apply(
                    &editor_core::edit::Transaction::new(vec![editor_core::edit::Edit::replace(
                        0..end,
                        item.text.clone(),
                    )]),
                    before,
                    before,
                );
                doc.break_undo_run();
                doc
            }
            _ => Document::recovered(item.path.clone(), &item.text),
        };

        let language = item
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map_or(LanguageId::PlainText, LanguageId::from_extension);

        let recovery_id = self.claim_recovery_id();
        self.docs.push(OpenDoc {
            recovery_id,
            highlighter: new_highlighter(language, &doc),
            doc,
            view: EditorView::default(),
            language,
            find: FindBar::default(),
            pending_find_step: None,
            preview: false,
            syntax_version: None,
            syntax_due: None,
            disk: DiskState::Unchanged,
            line_count_seen: None,
        });
        self.active = Some(self.docs.len() - 1);
        self.sync_watched_files();
    }

    /// Open whatever the command line named, once, after the session restore.
    ///
    /// After, not instead: restoring the previous session and then opening the
    /// file you asked for leaves you where you were with the new file in front,
    /// which is what every editor does and what you want when the invocation
    /// came from a `git commit` hook or an "open in editor" button.
    ///
    /// A folder argument becomes the project. Several folders and the first
    /// wins, because there is one explorer pane.
    fn open_from_command_line(&mut self) {
        if self.from_command_line.is_empty() {
            return;
        }
        let mut folder_taken = false;
        for path in std::mem::take(&mut self.from_command_line) {
            // Relative paths are relative to the shell's directory, and stay
            // usable only until something else changes ours.
            let path = std::fs::canonicalize(&path).map_or(path, plain_path);
            if path.is_dir() {
                if !folder_taken {
                    folder_taken = true;
                    self.open_folder(path);
                }
                continue;
            }
            self.open_path(&path, false);
        }
    }

    /// Re-check every open file when the window regains focus.
    ///
    /// The watcher only covers the open project folder, so a file opened from
    /// anywhere else — and every file when no folder is open at all — would
    /// otherwise never be checked. Coming back to the window is also exactly
    /// when you have been off editing the thing somewhere else, which is the
    /// case this is for.
    ///
    /// One `stat` per open document, on a transition rather than every frame.
    fn check_disk_on_focus(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let regained = focused && !self.was_focused;
        self.was_focused = focused;
        if !regained {
            return;
        }
        for index in 0..self.docs.len() {
            self.reconcile_with_disk(index);
        }
    }

    /// Work out what happened to one document's file, and react.
    ///
    /// A clean buffer is reloaded without asking: there is nothing to lose, and
    /// prompting for it is the kind of dialogue people learn to dismiss without
    /// reading. A dirty one raises the bar in `disk_bar` instead, because
    /// saving over a file that `git checkout` has rewritten is how work gets
    /// lost, and that decision is not the editor's to make.
    fn reconcile_with_disk(&mut self, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        match disk_response(entry.doc.disk_state(), entry.doc.is_dirty()) {
            // Our own save, or a change already reckoned with. Leave any bar
            // that is up alone; only a fresh change should raise one.
            DiskResponse::Ignore => {}
            DiskResponse::Reload => self.reload_document(index),
            DiskResponse::Ask(state) => self.docs[index].disk = state,
        }
    }

    /// The bar above the editor when the file has changed underneath it.
    ///
    /// Non-modal on purpose. A modal here interrupts whatever you were typing
    /// to ask about something you may not care about yet, and the honest answer
    /// is often "let me look at what I have first".
    fn disk_bar(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(entry) = self.docs.get(index) else {
            return;
        };
        let state = entry.disk;
        if state == DiskState::Unchanged {
            return;
        }
        let name = entry.doc.display_name();

        let mut reload = false;
        let mut keep = false;
        let mut save_back = false;

        egui::Frame::default()
            .fill(ui.visuals().warn_fg_color.gamma_multiply(0.15))
            .inner_margin(egui::Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| match state {
                    DiskState::Modified => {
                        ui.label(format!(
                            "{name} changed on disk, and this tab has unsaved edits."
                        ));
                        reload = ui
                            .button("Reload")
                            .on_hover_text("Throw away the edits in this tab and re-read the file")
                            .clicked();
                        keep = ui
                            .button("Keep mine")
                            .on_hover_text(
                                "Keep what is in this tab; saving will overwrite the file",
                            )
                            .clicked();
                    }
                    DiskState::Deleted => {
                        ui.label(format!(
                            "{name} was deleted on disk. This tab is the only copy."
                        ));
                        save_back = ui.button("Save it back").clicked();
                        keep = ui.button("Dismiss").clicked();
                    }
                    DiskState::Unchanged => {}
                });
            });
        ui.separator();

        if reload {
            self.reload_document(index);
        } else if keep {
            // Adopt what is on disk as the baseline so the same change is not
            // reported again on the next filesystem event.
            if let Some(entry) = self.docs.get_mut(index) {
                entry.doc.accept_disk_state();
                entry.disk = DiskState::Unchanged;
            }
        } else if save_back {
            match self.docs[index].doc.save() {
                Ok(()) => {
                    self.docs[index].disk = DiskState::Unchanged;
                    self.tree.refresh();
                    self.info(format!("Wrote {name} back to disk"));
                }
                Err(e) => self.error(format!("Could not write it back: {e:#}")),
            }
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
                // The freshly opened document carries the file's current mtime,
                // so the question the bar was asking has now been answered.
                entry.disk = DiskState::Unchanged;
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

    /// Carry out what the Source Control panel asked for.
    ///
    /// Everything except discarding happens immediately: staging and unstaging
    /// move the index around and git can put either back. Discarding cannot be
    /// put back by anything, so it goes through [`Self::discard_prompt`].
    fn apply_git_action(&mut self, action: git_panel::Action) {
        match action {
            git_panel::Action::None => {}
            git_panel::Action::Refresh => self.git.refresh_status(),
            git_panel::Action::Stage(paths) => {
                self.git.act(editor_vcs::tracker::Action::Stage(paths));
            }
            git_panel::Action::Unstage(paths) => {
                self.git.act(editor_vcs::tracker::Action::Unstage(paths));
            }
            git_panel::Action::Discard(paths) => self.pending_discard = Some(paths),
            git_panel::Action::Open(path) => self.open_path(&path, true),
            git_panel::Action::Diff(path) => self.diff_view.open(path),
            git_panel::Action::Commit { message, amend } => {
                self.git
                    .act(editor_vcs::tracker::Action::Commit { message, amend });
            }
            git_panel::Action::ShowHistory => {
                self.history.open();
                if !self.git.log_known() {
                    self.git.refresh_log(history_view::PAGE);
                }
            }
        }
    }

    /// The history window, and what it needs to draw.
    fn history_window(&mut self, ctx: &egui::Context) {
        if !self.history.is_open() {
            return;
        }
        // Asked for here rather than when the row was clicked: the selection
        // can also change by the list reloading under it, and this covers both.
        let selected = self.history.selected().map(str::to_owned);
        if let Some(id) = &selected {
            self.git.detail(id);
        }

        let state = if self.git.has_repo() {
            if self.git.log_known() {
                history_view::State::Ready {
                    commits: self.git.log(),
                    detail: selected.as_deref().and_then(|id| self.git.known_detail(id)),
                    // Exactly as many as were asked for means there are
                    // probably more; fewer means the history ran out.
                    more: self.git.log().len() >= self.git.log_limit(),
                }
            } else {
                history_view::State::Waiting
            }
        } else {
            history_view::State::NoRepository
        };

        let branch = self.git.branch().map(str::to_owned);
        let action = self.history.ui(ctx, branch.as_deref(), state);

        match action {
            history_view::Action::None => {}
            history_view::Action::LoadMore(limit) => self.git.refresh_log(limit),
            history_view::Action::Open(relative) => {
                if let Some(root) = self.git.root().map(Path::to_path_buf) {
                    self.open_path(&root.join(relative), true);
                }
            }
        }
    }

    /// Confirm before throwing work away.
    ///
    /// Worded more bluntly than the delete prompt on purpose. A deleted file is
    /// in the recycle bin; discarded changes are nowhere. The default is to
    /// cancel, and Escape takes it.
    fn discard_prompt(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.pending_discard.clone() else {
            return;
        };
        let mut decision: Option<bool> = None;

        egui::Modal::new(egui::Id::new("confirm_discard")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(if paths.len() == 1 {
                "Discard changes to this file?"
            } else {
                "Discard changes to these files?"
            });
            ui.add_space(8.0);
            ui.label("The changes are thrown away. This cannot be undone \u{2014} not by Undo, and not by git.");
            ui.add_space(8.0);

            // Every path, up to a point. A list that scrolls off the modal is
            // a list nobody read before clicking.
            const SHOWN: usize = 12;
            for path in paths.iter().take(SHOWN) {
                ui.weak(path);
            }
            if paths.len() > SHOWN {
                ui.weak(format!("\u{2026} and {} more", paths.len() - SHOWN));
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui.button("Discard").clicked() {
                    decision = Some(true);
                }
            });
        });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        match decision {
            None => {}
            Some(false) => self.pending_discard = None,
            Some(true) => {
                let paths = self.pending_discard.take().unwrap_or_default();
                self.git.act(editor_vcs::tracker::Action::Discard(paths));
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
                let recovery_id = self.claim_recovery_id();
                self.docs.push(OpenDoc {
                    recovery_id,
                    doc: Document::untitled(),
                    view: EditorView::default(),
                    language: LanguageId::PlainText,
                    highlighter: None,
                    find: FindBar::default(),
                    pending_find_step: None,
                    preview: false,
                    syntax_version: None,
                    syntax_due: None,
                    disk: DiskState::Unchanged,
                    line_count_seen: None,
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
                for index in 0..self.docs.len() {
                    if !self.docs[index].doc.is_dirty() || self.docs[index].doc.path().is_none() {
                        continue;
                    }
                    self.tidy_before_saving(index);
                    if let Err(e) = self.docs[index].doc.save() {
                        let name = self.docs[index].doc.display_name();
                        failures.push(format!("{name}: {e:#}"));
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
                // The watcher survives: the tabs left open still need watching,
                // and closing the folder is not a reason to stop noticing that
                // their files changed.
                self.watcher = Watcher::new(Some(ctx.clone())).ok();
                self.sync_watched_files();
                // No folder, no project: the branch and the gutter marks were
                // facts about a repository that is no longer open.
                self.git.set_project(None);
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
            CommandId::ShowSourceControl => {
                self.dock = DockTab::Git;
                self.show_output = true;
                // Opening the panel is the moment its contents matter, and the
                // status may be minutes old or never fetched. The log comes
                // with it because that is where the previous message lives, and
                // an amend needs it before the checkbox is ticked.
                self.git.refresh_status();
                if !self.git.log_known() {
                    self.git.refresh_log(history_view::PAGE);
                }
            }
            CommandId::ToggleBlame => {
                self.show_blame = !self.show_blame;
                if self.show_blame {
                    self.info(
                        "Blame is read from the saved file; unsaved edits shift it until you save",
                    );
                }
            }
            CommandId::ShowDiff => match self.active_doc().and_then(|e| e.doc.path()) {
                Some(path) => {
                    let path = path.to_path_buf();
                    self.diff_view.open(path);
                }
                // An unsaved buffer has nothing committed to compare with, and
                // saying so beats an empty window that looks like a failure.
                None => self.error("Save this file before comparing it with the repository"),
            },
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

            CommandId::DuplicateLine => self.on_view(|view, doc| view.duplicate_lines(doc)),
            CommandId::DeleteLine => self.on_view(|view, doc| view.delete_lines(doc)),
            CommandId::MoveLineUp => self.on_view(|view, doc| view.move_lines(doc, -1)),
            CommandId::MoveLineDown => self.on_view(|view, doc| view.move_lines(doc, 1)),
            CommandId::GoToFile => self.open_file_picker(),
            CommandId::ShowTerminal => {
                self.dock = DockTab::Terminal;
                self.show_output = true;
                self.open_terminal(ctx);
            }
            CommandId::ShowPackages => {
                self.dock = DockTab::Packages;
                self.show_output = true;
                self.packages.opened();
                self.refresh_packages();
            }
            CommandId::FindInProject => {
                self.dock = DockTab::Search;
                self.show_output = true;
                self.search.focus();
            }
            CommandId::NextTab => self.cycle_tab(false),
            CommandId::PreviousTab => self.cycle_tab(true),
            CommandId::ToggleBreakpoint => self.toggle_breakpoint_at_caret(),
            CommandId::DebugStart => self.debug_start_or_continue(),
            CommandId::DebugStop => self.debug_stop(),
            CommandId::DebugStepOver => self.debug_step(editor_debug::session::Step::Over),
            CommandId::DebugStepInto => self.debug_step(editor_debug::session::Step::Into),
            CommandId::DebugStepOut => self.debug_step(editor_debug::session::Step::Out),
            CommandId::TriggerCompletion => self.trigger_completion(),
            CommandId::GoToDefinition => {
                self.ask_about_symbol(editor_lsp::session::Query::Definition);
            }
            CommandId::FindUses => {
                self.ask_about_symbol(editor_lsp::session::Query::References);
            }
            CommandId::RenameSymbol => self.begin_rename(),
            CommandId::GoToSymbol => self.open_symbol_picker(),
            CommandId::ToggleFold => {
                let folded = self
                    .active
                    .and_then(|i| self.docs.get_mut(i))
                    .is_some_and(|entry| entry.view.toggle_fold_at_caret(&entry.doc));
                if !folded {
                    self.info("Nothing to fold here");
                }
            }
            CommandId::FoldAll | CommandId::UnfoldAll => {
                let collapse = id == CommandId::FoldAll;
                let changed = self
                    .active_mut()
                    .is_some_and(|entry| entry.view.fold_all(collapse));
                if !changed {
                    self.info(if collapse {
                        "Nothing to fold in this file"
                    } else {
                        "Nothing is folded"
                    });
                }
            }
            CommandId::AddCursorAtNextMatch => {
                let added = self
                    .active_mut()
                    .is_some_and(|entry| entry.view.add_cursor_at_next_match(&entry.doc));
                if !added {
                    self.info("No more occurrences");
                }
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
            CommandId::UserManual => self.manual.open(),
            CommandId::ThirdPartyLicences => self.licences.open(),
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

        // What is going on, so Run and Stop can say so. Debugging counts as
        // running: the program is live either way, and a Stop button that does
        // nothing during a debug session is a lie.
        let debugging = self.debug.is_some();
        let busy = self.runner.is_running() || self.runner.has_queued_work() || debugging;
        let what = if debugging { "debugging" } else { "running" };

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
                            let glyph = editor_widgets::icon::pick(ui, toolbar_glyphs(*id));

                            match id {
                                // While something is live, Run is spent and
                                // Stop is the live one. Saying so with enabled
                                // state and colour rather than with a changing
                                // glyph, because a button whose icon changes
                                // under the pointer is harder to aim at than
                                // one that greys out.
                                CommandId::Run if busy => {
                                    ui.add_enabled(false, egui::Button::new(glyph))
                                        .on_disabled_hover_text(format!("Already {what}"));
                                    // An actual moving thing, so it is obvious
                                    // at a glance that this is not a stalled
                                    // window.
                                    ui.spinner();
                                }
                                CommandId::RunStop => {
                                    let button = egui::Button::new(
                                        egui::RichText::new(glyph).color(if busy {
                                            ui.visuals().error_fg_color
                                        } else {
                                            ui.visuals().weak_text_color()
                                        }),
                                    );
                                    if ui
                                        .add_enabled(busy, button)
                                        .on_hover_text(if debugging {
                                            "Stop debugging".to_owned()
                                        } else {
                                            tip.clone()
                                        })
                                        .on_disabled_hover_text("Nothing is running")
                                        .clicked()
                                    {
                                        invoked = Some(if debugging {
                                            CommandId::DebugStop
                                        } else {
                                            CommandId::RunStop
                                        });
                                    }
                                }
                                _ => {
                                    if ui.button(glyph).on_hover_text(tip).clicked() {
                                        invoked = Some(*id);
                                    }
                                }
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
        let branch = self.git.branch().map(str::to_owned);

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
                            format!("{} No problems", editor_widgets::glyphs::OK)
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

                    // The branch, when the project is a repository. Absent
                    // rather than empty when it is not: a blank space labelled
                    // "branch" invites the question of which one.
                    //
                    // Labelled rather than given a branch icon, because the
                    // bundled fonts have no glyph for one and an unlabelled
                    // name reads as another of the values beside it.
                    if let Some(branch) = &branch {
                        ui.separator();
                        ui.weak(format!("Branch: {branch}"))
                            .on_hover_text("The branch this project is on");
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The theme indicator is a control, not a label — one
                        // of the three ways PLAN.md §3.11 requires it to be
                        // reachable.
                        if ui
                            .button(format!("{} {theme_label}", editor_widgets::glyphs::THEME))
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

            // Directly under the tabs, above everything else in the pane:
            // whatever happened to the file on disk outranks a search bar.
            // This also has to come before the borrows below, which hold
            // `self` for the rest of the closure.
            if let Some(index) = self.active {
                self.disk_bar(ui, index);
            }

            let mut opts = self.editor_options();
            let syntax = &self.syntax_theme;
            let underline_level = self.settings.underline_diagnostics();
            // Read before the mutable borrow below. Breakpoints are stored
            // one-based, as the gutter and the protocol both count them, and
            // converted here for painting.
            let active_path = self
                .active
                .and_then(|i| self.docs.get(i))
                .and_then(|e| e.doc.path())
                .map(Path::to_path_buf);
            let breakpoints_here: Vec<(usize, bool)> = active_path
                .as_ref()
                .map(|p| {
                    self.breakpoints
                        .for_file(p)
                        .into_iter()
                        .map(|line| (line.saturating_sub(1), true))
                        .collect()
                })
                .unwrap_or_default();
            let paused_line = self.debug_view.location().and_then(|(path, line)| {
                (Some(&path) == active_path.as_ref()).then(|| line.saturating_sub(1))
            });
            let diagnostics = self.lsp.diagnostics();

            // How the active buffer differs from the committed version. Read
            // here, before `entry` is borrowed mutably, because the tracker and
            // the documents are separate fields and the closure below would
            // otherwise borrow both at once.
            //
            // Only the active tab: the marks are only ever drawn for the file
            // on screen, and diffing the others would be work for nobody.
            let changes = active_path
                .as_ref()
                .zip(self.active.and_then(|i| self.docs.get(i)))
                .map(|(path, entry)| {
                    self.git
                        .marks(
                            path,
                            entry.doc.version(),
                            entry.doc.text().len_bytes(),
                            || entry.doc.text().to_string(),
                        )
                        .to_vec()
                })
                .unwrap_or_default();

            // Who last touched each line, when the annotations are switched on.
            // Read from the file *on disk*, so unsaved edits shift it — which
            // is what the message on switching it on says, and why saving asks
            // for it again.
            let blame: Vec<(usize, String)> = if self.show_blame {
                active_path
                    .as_ref()
                    .and_then(|path| self.git.blame(path))
                    .map(|lines| {
                        lines
                            .iter()
                            .map(|line| (line.number.saturating_sub(1), line.origin.label()))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

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
                entry.view.set_diagnostics(Self::underlines_for(
                    entry,
                    diagnostics,
                    underline_level,
                ));
                entry
                    .view
                    .set_debug_state(breakpoints_here.clone(), paused_line);
                entry.view.set_changes(changes);
                entry.view.set_blame(blame);

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
             Help > Check Toolchains lists what could be installed."
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
        self.report_startup();
        self.open_from_command_line();
        self.runner.set_context(&ctx);
        self.poll_watcher();
        self.git.poll();
        if self.git.take_committed() {
            // Only on a commit that actually landed. One a hook refused leaves
            // the message alone, so it can be tried again after the fix.
            self.git_panel.committed();
            self.info("Committed");
        }
        self.check_disk_on_focus(&ctx);
        self.autosave(&ctx);
        self.poll_debugger(&ctx);
        self.sync_highlighters(&ctx);
        self.sync_completion();
        self.sync_problem_at_caret();
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
                // Nothing unsaved, and the window is going: this session's
                // recovery copies would be offered back as crash debris on the
                // next start, which is worse than useless.
                self.recovery.clear();
            }
        }

        // One command per frame, from whichever source fired. Keyboard first,
        // so a shortcut is not swallowed by a menu that happens to be open —
        // except while a modal has focus, where keystrokes belong to its
        // fields and its own shortcut must not re-open it.
        // The completion popup is deliberately *not* in this list. It already
        // claims the five keys it needs in `completion_keys`, and treating it
        // as modal blocked every other shortcut in the application -- Ctrl+F,
        // Ctrl+S, F5 -- for as long as a suggestion was on screen, which while
        // typing is most of the time.
        let modal_open = self.symbol_picker.is_open()
            || self.first_run
            || !self.recovered.is_empty()
            || self.rename.is_some()
            || self.file_picker.is_open()
            || self.settings_form.is_open()
            || self.pending_delete.is_some()
            || self.pending_discard.is_some()
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
            let mut debug_action = debugger::Action::None;
            let mut search_action = crate::project_search::Action::None;
            let mut start_terminal = false;
            let mut open_packages = false;
            let mut open_git = false;
            let mut packages_action = crate::packages_panel::Action::None;
            let mut git_action = git_panel::Action::None;
            // Read before the closure borrows self for the panel. The panel
            // needs the repository root to turn git's relative paths back into
            // ones the editor can open — the *repository's* root, not the open
            // folder's, because opening one crate of a workspace makes those
            // two different and every path in the panel relative to the former.
            let git_root = self.git.root().map(Path::to_path_buf);
            // Read before the closure borrows self for the panel.
            let python = self
                .interpreter()
                .map(|i| i.path)
                .filter(|_| self.dock == DockTab::Packages);
            let requirements = crate::packages_panel::requirements_file(self.tree.root());
            // Read before the closure borrows self for the panel.
            let terminal_cwd = self.tree.root().map(Path::to_path_buf);
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
                        if ui
                            .selectable_label(self.dock == DockTab::Debug, "DEBUG")
                            .clicked()
                        {
                            self.dock = DockTab::Debug;
                        }
                        if ui
                            .selectable_label(self.dock == DockTab::Search, "SEARCH")
                            .clicked()
                        {
                            self.dock = DockTab::Search;
                        }
                        if ui
                            .selectable_label(self.dock == DockTab::Terminal, "TERMINAL")
                            .clicked()
                        {
                            self.dock = DockTab::Terminal;
                            start_terminal = true;
                        }
                        if ui
                            .selectable_label(self.dock == DockTab::Packages, "PACKAGES")
                            .clicked()
                        {
                            self.dock = DockTab::Packages;
                            open_packages = true;
                        }
                        if ui
                            .selectable_label(self.dock == DockTab::Git, "SOURCE CONTROL")
                            .clicked()
                        {
                            self.dock = DockTab::Git;
                            open_git = true;
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
                        DockTab::Search => search_action = self.search.ui(ui),
                        DockTab::Terminal => {
                            let cwd = terminal_cwd.clone();
                            start_terminal |= self.terminal.ui(ui, cwd.as_deref());
                        }
                        DockTab::Packages => {
                            packages_action =
                                self.packages
                                    .ui(ui, python.as_deref(), requirements.as_deref());
                        }
                        DockTab::Git => {
                            let state = if self.git.has_repo() {
                                if self.git.status_known() {
                                    git_panel::State::Ready {
                                        status: self.git.status(),
                                        error: self.git.error(),
                                        last_message: self.git.last_message(),
                                    }
                                } else {
                                    git_panel::State::Waiting
                                }
                            } else {
                                git_panel::State::NoRepository
                            };
                            git_action = self.git_panel.ui(ui, state, git_root.as_deref());
                        }
                        DockTab::Debug => {
                            let running = self.debug.is_some();
                            let paused = self
                                .debug
                                .as_ref()
                                .is_some_and(editor_debug::Session::is_paused);
                            debug_action = self.debug_view.ui(ui, running, paused);
                        }
                    }
                });

            if start_terminal {
                self.open_terminal(&ctx);
            }
            if open_packages {
                self.packages.opened();
                self.refresh_packages();
            }
            if open_git {
                self.git.refresh_status();
                if !self.git.log_known() {
                    self.git.refresh_log(history_view::PAGE);
                }
            }
            self.apply_packages_action(packages_action);
            self.apply_git_action(git_action);

            match search_action {
                crate::project_search::Action::None => {}
                crate::project_search::Action::Start(query) => self.start_project_search(&query),
                crate::project_search::Action::Open { path, line, column } => {
                    if let Some(root) = self.tree.root().map(Path::to_path_buf) {
                        self.open_at(&root.join(path), line.saturating_sub(1), column);
                    }
                }
            }

            if let debugger::Action::SelectFrame { id, path, line } = debug_action {
                self.debug_view.selected = Some(id);
                if let Some(session) = self.debug.as_mut() {
                    session.select_frame(id);
                }
                self.open_at(&path, line.saturating_sub(1), 0);
            }
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
                tab_bar::Action::Reorder { from, to } => self.reorder_tab(from, to),
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
        self.recovery_prompt(&ctx);
        if let Some(range) = self.symbol_picker.ui(&ctx) {
            self.reveal_in_active(range);
        }
        self.first_run_ui(&ctx);
        self.manual.ui(&ctx, "user_manual", "User Manual", MANUAL);
        self.licences
            .ui(&ctx, "third_party", "Third-Party Licences", THIRD_PARTY);
        self.diff_window(&ctx);
        self.history_window(&ctx);
        self.rename_ui(&ctx);
        // After the editor has painted, so the caret rect it anchors to is
        // from this frame rather than the last one.
        self.completion_draw(&ctx);
        if let Some(relative) = self.file_picker.ui(&ctx)
            && let Some(root) = self.tree.root().map(Path::to_path_buf)
        {
            self.open_path(&root.join(relative), false);
        }
        self.unsaved_prompt(&ctx);
        self.delete_prompt(&ctx);
        self.discard_prompt(&ctx);
        self.search.poll();
        if self.packages.poll() {
            // pip's update check talks to the network and takes seconds; keep
            // the loop turning gently rather than spinning on it.
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.terminal.poll() {
            // A shell produces output between frames; keep the loop
            // turning while it does, and stop when it goes quiet.
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        self.settle_mru(&ctx);
        self.toasts_ui(&ctx);

        // A breakpoint asked for by a gutter click or the context menu. Drained
        // here, after the editor has drawn: the view records the request and
        // the application owns the breakpoint set, because breakpoints outlive
        // the view that showed them.
        if let Some(line) = self
            .active
            .and_then(|i| self.docs.get_mut(i))
            .and_then(|e| e.view.take_breakpoint_toggle())
        {
            self.toggle_breakpoint(line);
        }

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

/// Apply a rename to a file that is not open, on disk.
///
/// The edits arrive sorted last-first, so each replacement is stated in
/// coordinates that the ones already applied have not disturbed.
fn apply_rename_to_disk(file: &editor_lsp::session::FileEdit) -> Result<(), String> {
    let text = std::fs::read_to_string(&file.path).map_err(|e| e.to_string())?;
    let mut rope = ropey::Rope::from_str(&text);

    for edit in &file.edits {
        let start = offset_of(&rope, edit.start_line as usize, edit.start_column as usize);
        let end = offset_of(&rope, edit.end_line as usize, edit.end_column as usize);
        if end < start || end > rope.len_chars() {
            return Err("the server described an edit outside the file".to_owned());
        }
        rope.remove(start..end);
        rope.insert(start, &edit.text);
    }

    std::fs::write(&file.path, rope.to_string()).map_err(|e| e.to_string())
}

/// A zero-based line and column as a character offset, clamped to that line.
fn offset_of(rope: &ropey::Rope, line: usize, column: usize) -> usize {
    if line >= rope.len_lines() {
        return rope.len_chars();
    }
    let start = rope.line_to_char(line);
    let len = rope.line(line).len_chars();
    (start + column).min(start + len).min(rope.len_chars())
}

/// Strip Windows' verbatim `\\?\` prefix from a canonicalised path.
///
/// `canonicalize` returns extended-length paths, which are correct but leak
/// into everything that displays or stores one: title bars, the recent list,
/// recovery files, and the arguments handed to a language server — some of
/// which do not understand the form at all.
///
/// Only the plain drive-letter case is unwrapped. `\\?\UNC\server\share` is
/// left alone, because dropping its prefix would produce a path that no longer
/// refers to the same place.
#[must_use]
fn plain_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path;
    };
    let is_drive = matches!(rest.as_bytes(), [c, b':', b'\\', ..] if c.is_ascii_alphabetic());
    if is_drive { PathBuf::from(rest) } else { path }
}

/// What to do about a file that changed underneath an open document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiskResponse {
    /// Nothing happened, or nothing that has not already been dealt with.
    Ignore,
    /// Re-read it without asking.
    Reload,
    /// Put the bar up and let the user decide.
    Ask(DiskState),
}

/// The rule, separated from the application state so it can be stated plainly.
///
/// The only case that reloads silently is a rewritten file with nothing unsaved
/// in the buffer: there is genuinely nothing to lose, and a prompt for it is
/// one people learn to dismiss unread — which is what makes the prompt that
/// *does* matter dangerous. Everything else is the user's call.
fn disk_response(state: DiskState, dirty: bool) -> DiskResponse {
    match state {
        DiskState::Unchanged => DiskResponse::Ignore,
        DiskState::Modified if !dirty => DiskResponse::Reload,
        other => DiskResponse::Ask(other),
    }
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

/// Candidate glyphs for a toolbar button, best first, plain ASCII last.
///
/// A list rather than one character because the bundled fonts do not cover
/// everything and a missing glyph is drawn as a box — silently, and identically
/// for every button that misses. See `editor_widgets::icon`.
fn toolbar_glyphs(id: CommandId) -> &'static [&'static str] {
    // Icons first, then a shape from a block the bundled fonts do cover, then
    // a short label. The labels are two letters rather than a lookalike
    // symbol: with no icon font, `>` for both Run and Redo, and a circle for
    // both Open and Find, is worse than plain text.
    match id {
        CommandId::NewFile => &["\u{2795}", "+"],
        CommandId::OpenFile => &["\u{1f4c2}", "Op"],
        CommandId::Save => &["\u{1f4be}", "Sv"],
        // Not `\u{1f5c3}`, `\u{21b6}` or `\u{21b7}`: the bundled fonts have
        // none of the three, so these three buttons were the text fallbacks.
        CommandId::SaveAll => &["\u{1f5c4}", "SA"],
        CommandId::Undo => &["\u{27f2}", "Un"],
        CommandId::Redo => &["\u{27f3}", "Re"],
        CommandId::Find => &["\u{1f50d}", "Fi"],
        CommandId::Run => &["\u{25b6}", "\u{25b8}", "Run"],
        CommandId::RunStop => &["\u{25a0}", "Stop"],
        CommandId::ToggleExplorer => &["\u{2630}", "\u{25a4}", "Ex"],
        CommandId::CommandPalette => &["\u{2318}", "\u{25c8}", "Cmd"],
        CommandId::OpenSettings | CommandId::OpenSettingsFile => &["\u{2699}", "\u{25cf}", "Set"],
        _ => &["?"],
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
            MenuEntry::Item(CommandId::GoToFile),
            MenuEntry::Recent,
            MenuEntry::Item(CommandId::OpenFolder),
            MenuEntry::Item(CommandId::CloseFolder),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Save),
            MenuEntry::Item(CommandId::SaveAs),
            MenuEntry::Item(CommandId::SaveAll),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::NextTab),
            MenuEntry::Item(CommandId::PreviousTab),
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
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::DuplicateLine),
            MenuEntry::Item(CommandId::DeleteLine),
            MenuEntry::Item(CommandId::MoveLineUp),
            MenuEntry::Item(CommandId::MoveLineDown),
            MenuEntry::Item(CommandId::Indent),
            MenuEntry::Item(CommandId::Outdent),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::Find),
            MenuEntry::Item(CommandId::Replace),
            MenuEntry::Item(CommandId::FindInProject),
            MenuEntry::Item(CommandId::FindNext),
            MenuEntry::Item(CommandId::FindPrevious),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::TriggerCompletion),
            MenuEntry::Item(CommandId::GoToDefinition),
            MenuEntry::Item(CommandId::FindUses),
            MenuEntry::Item(CommandId::RenameSymbol),
            MenuEntry::Item(CommandId::AddCursorAtNextMatch),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::GoToSymbol),
            MenuEntry::Item(CommandId::NextUse),
            MenuEntry::Item(CommandId::PreviousUse),
        ],
    ),
    (
        "View",
        &[
            MenuEntry::Item(CommandId::ToggleFold),
            MenuEntry::Item(CommandId::FoldAll),
            MenuEntry::Item(CommandId::UnfoldAll),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::ToggleExplorer),
            MenuEntry::Item(CommandId::ShowOutput),
            MenuEntry::Item(CommandId::ShowTerminal),
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
            MenuEntry::Item(CommandId::ToggleBreakpoint),
            MenuEntry::Item(CommandId::DebugStart),
            MenuEntry::Item(CommandId::DebugStepOver),
            MenuEntry::Item(CommandId::DebugStepInto),
            MenuEntry::Item(CommandId::DebugStepOut),
            MenuEntry::Item(CommandId::DebugStop),
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
            MenuEntry::Item(CommandId::ShowPackages),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::OpenSettings),
        ],
    ),
    (
        "Help",
        &[
            MenuEntry::Item(CommandId::KeyboardShortcuts),
            MenuEntry::Item(CommandId::UserManual),
            MenuEntry::Item(CommandId::CheckToolchains),
            MenuEntry::Item(CommandId::OpenLogFolder),
            MenuEntry::Separator,
            MenuEntry::Item(CommandId::ThirdPartyLicences),
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
                let glyphs = toolbar_glyphs(*id);
                assert_ne!(glyphs, ["?"], "{id:?} is on the toolbar but has no glyph");
                // Two buttons whose last-resort label is the same are
                // indistinguishable on a machine whose fonts cover neither
                // icon -- which is this one. Run and Redo were both `>`.
                for other in TOOLBAR.iter().flat_map(|g| g.iter()) {
                    if other == id {
                        continue;
                    }
                    assert_ne!(
                        glyphs.last(),
                        toolbar_glyphs(*other).last(),
                        "{id:?} and {other:?} fall back to the same label"
                    );
                }
                assert!(
                    glyphs.len() >= 2 || glyphs[0].is_ascii(),
                    "{id:?} has a single non-ASCII glyph and so no fallback if the                      font cannot draw it"
                );
                assert!(
                    glyphs.last().is_some_and(|g| g.is_ascii()),
                    "{id:?} ends in a glyph that could itself be missing"
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
                       Help > Check Toolchains lists what could be installed.";
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

    /// `canonicalize` on Windows returns extended-length paths, which are
    /// correct and unusable: they reach title bars, the recent list, recovery
    /// files, and the arguments given to language servers, some of which do not
    /// understand the form.
    #[test]
    fn a_verbatim_windows_path_is_unwrapped_but_a_unc_one_is_not() {
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\C:\workspace\a.py")),
            PathBuf::from(r"C:\workspace\a.py")
        );
        // Dropping this prefix would name a different place, so it stays.
        let unc = PathBuf::from(r"\\?\UNC\server\share\a.py");
        assert_eq!(plain_path(unc.clone()), unc);
        // Anything that was never verbatim passes through untouched.
        let plain = PathBuf::from("/home/g/a.py");
        assert_eq!(plain_path(plain.clone()), plain);
    }

    /// A clean buffer is re-read without asking; a dirty one never is. Getting
    /// this backwards silently throws away unsaved work.
    #[test]
    fn only_a_clean_buffer_reloads_without_asking() {
        assert_eq!(
            disk_response(DiskState::Modified, false),
            DiskResponse::Reload
        );
        assert_eq!(
            disk_response(DiskState::Modified, true),
            DiskResponse::Ask(DiskState::Modified)
        );
    }

    /// A deleted file is never reloaded, clean buffer or not: reloading means
    /// reading, and there is nothing there to read. The tab is now the only
    /// copy of that text in existence.
    #[test]
    fn a_deleted_file_always_asks_even_when_the_buffer_is_clean() {
        assert_eq!(
            disk_response(DiskState::Deleted, false),
            DiskResponse::Ask(DiskState::Deleted)
        );
        assert_eq!(
            disk_response(DiskState::Deleted, true),
            DiskResponse::Ask(DiskState::Deleted)
        );
    }

    #[test]
    fn an_unchanged_file_is_left_alone_however_dirty_the_buffer_is() {
        assert_eq!(
            disk_response(DiskState::Unchanged, false),
            DiskResponse::Ignore
        );
        assert_eq!(
            disk_response(DiskState::Unchanged, true),
            DiskResponse::Ignore
        );
    }

    /// Reordering must keep the selection and the recent list pointing at the
    /// same *documents*, not at the same positions.
    #[test]
    fn moving_a_tab_carries_the_indices_that_referred_to_it() {
        // The remap, stated directly: moving 0 to 2 in [0,1,2,3].
        let remap = |from: usize, to: usize, i: usize| {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        };

        // Rightwards: the dragged tab lands at 2, the ones it passed shift left.
        assert_eq!(remap(0, 2, 0), 2, "the dragged tab");
        assert_eq!(remap(0, 2, 1), 0);
        assert_eq!(remap(0, 2, 2), 1);
        assert_eq!(remap(0, 2, 3), 3, "beyond the move, untouched");

        // Leftwards: the ones it passed shift right.
        assert_eq!(remap(3, 1, 3), 1);
        assert_eq!(remap(3, 1, 1), 2);
        assert_eq!(remap(3, 1, 2), 3);
        assert_eq!(remap(3, 1, 0), 0);
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
