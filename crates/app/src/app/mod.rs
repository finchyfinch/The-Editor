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
use editor_core::document::{DiskState, Document, Encoding, Unrepresentable};
use editor_syntax::LanguageId;
use editor_syntax::highlight::Highlighter;
use editor_syntax::theme::SyntaxTheme;
use editor_widgets::editor_view::{EditorOptions, EditorView, Underline, severity_colour};
use editor_widgets::find_bar::{self, FindBar};
use editor_widgets::{file_tree::FileTree, tab_bar, theme as ui_theme};
use eframe::egui;

use crate::branches_view::{self, BranchesView};
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
use crate::tests_panel::{self, TestsPanel};
use crate::venv_dialog;
use crate::watcher::Watcher;

mod chrome;
mod documents;
mod language;
mod problems;
mod project;
mod running;
mod session;
#[cfg(test)]
mod tests;
mod vcs;

/// Metadata shown in the About dialog.
/// The user manual, compiled in so it is there with no network and no install
/// directory to go looking in.
pub(crate) const MANUAL: &str = include_str!("../../../../docs/manual.md");

/// Generated from the resolved dependency graph by `tools/make_licences.py`.
pub(crate) const THIRD_PARTY: &str = include_str!("../../../../docs/third-party.md");

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

    /// Which folders may run their own tools, and the folder waiting to be
    /// asked about.
    trust: editor_config::trust::Trust,
    trust_prompt: Option<PathBuf>,
    /// Whether the open folder contains anything that would run, and so has
    /// to be trusted before it does. Most folders do not, and are never asked.
    trust_required: bool,
    /// What the project's surroundings say — interpreter, venv, requirements,
    /// `.editorconfig` — remembered between frames rather than asked of the
    /// filesystem on every one.
    environment: crate::environment::Environment,
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
    /// The branches, likewise.
    branches: BranchesView,
    /// A branch awaiting a yes/no before it is deleted with its commits.
    pending_force_delete: Option<String>,
    /// Whether the editor shows who last touched each line.
    show_blame: bool,
    /// What is under the pointer, and what to say about it.
    ///
    /// Held across frames because the answer arrives from a server long after
    /// the question, and cleared the moment the pointer moves to a different
    /// character — a description of somewhere the pointer has left is worse
    /// than none.
    hover: Option<Hover>,
    /// The Tests tab.
    tests_panel: TestsPanel,
    /// The run in progress, or the last one. `None` before anything has been
    /// run, which the panel shows differently from a run that found nothing.
    tests: Option<editor_testing::Session>,
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
    /// Where the window was the last time it was a *normal* window.
    ///
    /// This, and not the size it happens to have right now, is what gets
    /// remembered: a maximized window's size belongs to the screen, and
    /// writing it down as the user's own size is what made restore-down give
    /// back a window the size of the screen. Sampled every frame the window is
    /// not maximized, and seeded from the session so quitting maximized twice
    /// running does not forget the size underneath.
    restored_window: Option<WindowGeometry>,
    /// The geometry asked for on the first frame, held until the window
    /// actually has it. Until then the window still describes itself as it was
    /// *built* — the fallback size in `main` — and filing that away as the
    /// user's chosen size would throw away what the session just restored.
    awaiting_window: Option<WindowGeometry>,
    /// Whether the window is maximized right now, sampled with the geometry.
    window_maximized: bool,

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
    /// A save refused because the file's encoding cannot hold its text: the
    /// tab (by recovery id, which survives tabs moving), what was refused, and
    /// the Save As destination if there was one.
    pending_encoding: Option<(u64, Unrepresentable, Option<PathBuf>)>,
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
}

/// What the pointer is resting on, and what is known about it.
#[derive(Debug, Clone)]
struct Hover {
    /// The character offset asked about, so a reply about somewhere else can be
    /// told from one about here.
    offset: usize,
    /// Where to put the popup.
    at: egui::Pos2,
    /// What to show. Empty while waiting for a server that has been asked.
    text: String,
    /// True when the text came from the parse tree rather than a server, so the
    /// popup can say so — the difference between "this is what it is" and "this
    /// is the line it was declared on" matters.
    from_file: bool,
    /// Whether a server has been asked and has not answered.
    waiting: bool,
    /// The diagnostics at this place, shown above whatever the server says:
    /// "why is this underlined" is the first question a squiggle raises.
    problems: Vec<editor_lsp::diagnostics::Diagnostic>,
    /// Asked from the gutter marker, about the whole line rather than a symbol.
    gutter: bool,
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
    Tests,
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
        let trust = editor_config::trust::Trust::load(&paths.trust_file());

        // Before this session claims a directory of its own, so its own empty
        // one is not among the candidates.
        let backups = paths.backup_dir();
        let recovered = recovery::collect(&backups, None);
        let paths_for_recovery = backups;

        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
        });

        // Zoom has one owner, and it is this application.
        //
        // egui runs its own Cmd+Plus/Minus/0 handler at the end of every pass,
        // which moves `Context::zoom_factor` directly and tells nobody. The
        // View menu's zoom commands move `ui.ui_scale` in the settings, which
        // is persisted and pushed into the same zoom factor by
        // `sync_appearance`. Two owners of one number, each unaware of the
        // other, and they come apart in the ordinary course of use: egui also
        // binds Cmd+= (a `+` is a shifted `=`, so Ctrl+= never reached the
        // command table), so every press of the obvious zoom-in key moved the
        // display without moving the setting. Once the setting had drifted to
        // the bottom of its range, zooming out stopped doing anything at all
        // -- the value clamped, `sync_appearance` saw nothing change, and
        // nothing was pushed to the context -- while zoom in still worked,
        // because that was egui's handler all along.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);

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
            trust,
            trust_prompt: None,
            trust_required: false,
            environment: crate::environment::Environment::default(),
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
            branches: BranchesView::default(),
            pending_force_delete: None,
            show_blame: false,
            hover: None,
            tests_panel: TestsPanel::default(),
            tests: None,
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
            restored_window: None,
            awaiting_window: None,
            window_maximized: false,
            recent: Vec::new(),
            pending_recent: None,
            pending_delete: None,
            pending_discard: None,
            pending_encoding: None,
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

    fn active_doc(&self) -> Option<&OpenDoc> {
        self.active.and_then(|i| self.docs.get(i))
    }

    fn active_mut(&mut self) -> Option<&mut OpenDoc> {
        self.active.and_then(|i| self.docs.get_mut(i))
    }

    /// Editor options from settings, with the language left at its default —
    /// callers that have a document fill that in.
    fn editor_options(&mut self) -> EditorOptions {
        let mut opts = EditorOptions {
            font_size: self.settings.font_size(),
            tab_width: self.settings.tab_width(),
            insert_spaces: self.settings.insert_spaces(),
            show_line_numbers: true,
            language: LanguageId::PlainText,
            auto_close_brackets: self.settings.auto_close_brackets(),
            docstrings: self.settings.docstrings(),
            reduce_motion: self.settings.reduce_motion(),
            sticky_scopes: self.settings.sticky_scopes(),
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
            let style = self.environment.style_for(path);
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
            CommandId::RunTests => self.run_tests(editor_testing::Scope::All),
            CommandId::RunTestsInFile => match self.active_doc().and_then(|e| e.doc.path()) {
                Some(path) => {
                    let path = path.to_path_buf();
                    self.run_tests(editor_testing::Scope::File(path));
                }
                None => self.error("Save the file before running its tests"),
            },
            CommandId::RunTestAtCaret => self.run_test_at_caret(),
            CommandId::RunFailedTests => {
                let failures = self
                    .tests
                    .as_ref()
                    .map(|s| s.report.failures())
                    .unwrap_or_default();
                if failures.is_empty() {
                    self.info("No failing tests to run again");
                } else {
                    self.run_tests(editor_testing::Scope::These(failures));
                }
            }
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
                self.git.refresh_branches();
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
            CommandId::FolderTrust => match self.tree.root() {
                Some(root) => self.trust_prompt = Some(root.to_path_buf()),
                None => self.info("Open a folder first"),
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
        self.track_window(&ctx);
        self.report_startup();
        self.open_from_command_line();
        self.runner.set_context(&ctx);
        self.poll_watcher();
        self.poll_tests();
        self.git.poll();
        // Git refusing to delete an unmerged branch is not a dead end: it is the
        // moment to ask whether to force it. Tried the safe way first and only
        // offering the dangerous one when the safe one is impossible is the
        // whole shape of this interaction.
        if let Some(editor_vcs::tracker::Action::DeleteBranch { name, force: false }) =
            self.git.take_failed()
            && self
                .git
                .error()
                .is_some_and(|e| e.contains("not fully merged"))
        {
            self.git.clear_error();
            self.pending_force_delete = Some(name);
        }
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
        self.sync_hover(&ctx);
        // Before the menu bar, toolbar and editor read this frame's events:
        // whoever looks first gets the key.
        self.completion_keys(&ctx);
        self.sync_language_servers();
        if self.lsp.is_starting() || self.environment.is_asking() {
            // A server being looked for or shaking hands answers on another
            // thread, and nothing else wakes an idle window to notice.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // Never let the window close with unsaved work. This must run before
        // anything else in the frame, and `quit_confirmed` stops the second
        // close request being intercepted again.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            // Written before the unsaved prompt, so the session survives even
            // if the user then cancels the quit and closes some other way.
            self.save_session();
            if self.docs.iter().any(|d| d.doc.is_dirty()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            } else {
                // Nothing unsaved, and the window is going: this session's
                // recovery copies would be offered back as crash debris on the
                // next start, which is worse than useless.
                self.confirm_quit();
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
            || self.pending_encoding.is_some()
            || self.trust_prompt.is_some()
            || self.pending_force_delete.is_some()
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
            // Whatever arrived after the last `poll_tests`, which is where the
            // summary lines usually land. Taken *here* and not before the
            // check: draining the tap on every frame would throw away the
            // output of a run still going, which is every line of it.
            let last_output = self.runner.take_output();

            // A test run's own report is the outcome, so the generic "Exited
            // with code 1" banner is noise — a failing test is *supposed* to
            // exit non-zero.
            let was_tests = self.tests.is_some();
            let outcome = self.tests.as_mut().map(|session| {
                // Anything left in the pipe, then close the parsers: a test
                // that started and never reported becomes a failure rather
                // than staying "running" for ever.
                session.feed(&last_output);
                session.finish();
                (
                    session.report.summary(),
                    session
                        .report
                        .count(editor_testing::report::Outcome::Failed),
                )
            });
            if let Some((summary, failed)) = outcome {
                self.runner.watch_output(false);
                if failed > 0 {
                    self.error(summary);
                } else {
                    self.info(summary);
                }
            }

            // A virtual environment being created takes precedence over the
            // generic banner: the user asked for an environment, not for a
            // process to exit.
            if let Some(completion) = self.pending_venv.take() {
                self.finish_venv(&completion, code);
            } else if was_tests {
                // Already reported, in the terms that matter.
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
            let mut tests_action = tests_panel::Action::None;
            // Read before the closure borrows self for the panel.
            let test_label = self
                .tests
                .as_ref()
                .map(|s| format!("{} \u{2014} {}", s.framework.label(), s.scope.label()))
                .unwrap_or_default();
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
            let requirements = self
                .environment
                .requirements(&self.settings.python_interpreter(), self.tree.root());
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
                        if ui
                            .selectable_label(self.dock == DockTab::Tests, "TESTS")
                            .clicked()
                        {
                            self.dock = DockTab::Tests;
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
                                        branch: self.git.current_branch(),
                                        remotes: self.git.remotes(),
                                        busy: self.git.busy(),
                                        remote_said: self.git.remote_said(),
                                    }
                                } else {
                                    git_panel::State::Waiting
                                }
                            } else {
                                git_panel::State::NoRepository
                            };
                            git_action = self.git_panel.ui(ui, state, git_root.as_deref());
                        }
                        DockTab::Tests => {
                            let state = match &self.tests {
                                None => tests_panel::State::Idle,
                                Some(session) if session.report.finished => {
                                    tests_panel::State::Finished {
                                        report: &session.report,
                                        label: &test_label,
                                    }
                                }
                                Some(session) => tests_panel::State::Running {
                                    report: &session.report,
                                    label: &test_label,
                                },
                            };
                            tests_action = self.tests_panel.ui(ui, state);
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
                self.git.refresh_branches();
            }
            self.apply_packages_action(packages_action);
            self.apply_git_action(git_action);
            self.apply_tests_action(tests_action);

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
        self.branches_window(&ctx);
        self.rename_ui(&ctx);
        // After the editor has painted, so the caret rect it anchors to is
        // from this frame rather than the last one.
        self.completion_draw(&ctx);
        self.hover_draw(&ctx);
        if let Some(relative) = self.file_picker.ui(&ctx)
            && let Some(root) = self.tree.root().map(Path::to_path_buf)
        {
            self.open_path(&root.join(relative), false);
        }
        self.unsaved_prompt(&ctx);
        self.delete_prompt(&ctx);
        self.discard_prompt(&ctx);
        self.encoding_prompt(&ctx);
        self.trust_prompt_ui(&ctx);
        self.force_delete_prompt(&ctx);
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
                ContextAction::ShowProblem => {
                    self.run_command(CommandId::ShowProblems, &ctx);
                    // Scroll to it even if it was already the highlighted row:
                    // the list may have been scrolled away from it since.
                    self.problem_revealed = None;
                }
                ContextAction::CopyProblem => self.copy_problem_at_caret(&ctx),
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

/// The status bar's word on what runs the active file, and what clicking it
/// does.
struct RuntimeStatus {
    text: String,
    hover: String,
    command: CommandId,
}

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
/// Through a [`Document`], so the file is read and written exactly as an open
/// one would be: its encoding, byte-order mark and line endings kept, and the
/// write atomic. The edits all go in one transaction, in the coordinates of the
/// file as it was — which is what a `WorkspaceEdit` states them in — rather
/// than one after another, where each depended on the ones before it having
/// been applied in the right order.
fn apply_rename_to_disk(file: &editor_lsp::session::FileEdit) -> Result<(), String> {
    let mut doc = Document::open(&file.path).map_err(|e| format!("{e:#}"))?;
    let mut edits = Vec::with_capacity(file.edits.len());
    for edit in &file.edits {
        let start = offset_of(
            doc.text(),
            edit.start_line as usize,
            edit.start_column as usize,
        );
        let end = offset_of(doc.text(), edit.end_line as usize, edit.end_column as usize);
        if end < start {
            return Err("the server described an edit that ends before it starts".to_owned());
        }
        edits.push(editor_core::edit::Edit::replace(
            start..end,
            edit.text.clone(),
        ));
    }
    let unchanged = editor_core::selection::Selection::at(0);
    doc.apply(
        &editor_core::edit::Transaction::new(edits),
        unchanged,
        unchanged,
    );
    doc.save().map_err(|e| format!("{e:#}"))
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

/// One step of the zoom, rounded back onto the step.
///
/// A tenth is not an `f32`, so adding 0.1 over and over walks 1.1 to 1.1000001
/// to 1.2000002 and on. The drift is invisible in the rendering and loud
/// everywhere a number is compared: the value that should read 1.0 no longer
/// equals the 1.0 that the reset command sets, that the settings slider shows,
/// and that is written to the settings file.
fn stepped_scale(scale: f32, delta: f32) -> f32 {
    ((scale + delta) * 10.0).round() / 10.0
}

/// The tab already showing `path`, if one is.
///
/// The rule, separated from the application state so it can be stated plainly:
/// a buffer belongs to its file, and a file gets one tab. A buffer with no path
/// matches nothing -- two untitled buffers are two different pieces of work,
/// however alike they look.
fn tab_showing(docs: &[OpenDoc], path: Option<&Path>) -> Option<usize> {
    let path = path?;
    docs.iter().position(|d| d.doc.path() == Some(path))
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
            MenuEntry::Item(CommandId::RunTestsInFile),
            MenuEntry::Item(CommandId::RunTestAtCaret),
            MenuEntry::Item(CommandId::RunFailedTests),
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
            MenuEntry::Item(CommandId::FolderTrust),
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
