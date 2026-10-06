//! User settings, stored as TOML.
//!
//! The backing store is a `toml_edit::DocumentMut` rather than a plain struct
//! deserialised with serde, and that is deliberate. Settings must survive a
//! round trip through a *different version* of The Editor: if a newer build
//! wrote a key this build has never heard of, saving here must not delete it,
//! and the user's comments and key ordering must not be reshuffled. A struct
//! round trip loses all three. See PLAN.md §3.9.
//!
//! Typed accessors read through to the document and fall back to a documented
//! default when a key is absent or the wrong type — a hand-edited settings file
//! with `tab_width = "four"` in it should ignore that line, not fail to start.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Table, Value, value};

use crate::theme::ThemePreference;

/// Written on first run so the file people open is documented rather than
/// empty. Keys match the defaults in the accessors below.
pub const DEFAULT_SETTINGS_TOML: &str = "\
# The Editor - user settings.
#
# Keys not listed here are left at their defaults. Anything this version of
# The Editor does not recognise is preserved untouched when the file is
# rewritten, so it is safe to hand-edit.

[ui]
# \"dark\" (default), \"light\", or \"system\" to follow the operating system.
theme = \"dark\"
# Syntax colours for the code pane. \"follow\" keeps them in step with `theme`.
# Not yet implemented: the code pane always follows `theme` for now.
syntax_theme = \"follow\"
# Size of interface text - menus, tabs, the file tree, the status bar.
# The code pane has its own independent editor.font_size below.
font_size = 14.0
# Zoom, applied on top of the display's own DPI scaling. Leave at 1.0 to use
# whatever the monitor reports; raise it to make everything larger.
ui_scale = 1.0
show_file_tree = true
# Single-clicking a file in the explorer opens it in a reusable preview tab,
# shown in italics, that the next single-clicked file replaces until you edit
# it or double-click it. Off, every file opened gets a tab of its own.
preview_tabs = false
restore_session = true

[editor]
font_size = 13.0
tab_width = 4
insert_spaces = true
# Not yet implemented: long lines always scroll horizontally for now.
word_wrap = false
# Typing an opening bracket or quote also inserts its closer.
auto_close_brackets = true
# Which diagnostics are underlined in the text: \"all\", \"errors\" or \"none\".
# The gutter marks and the Problems panel are unaffected, so nothing is
# hidden -- this only controls how much the editor is written on. A type
# checker that cannot resolve a project's imports reports most of its lines,
# which makes the file unreadable at \"all\".
underline_diagnostics = \"errors\"
# Tidying applied when a file is written. Both land in the undo history, so
# saving and pressing undo gets the whitespace back.
trim_trailing_whitespace = false
insert_final_newline = false
# Honour a project's .editorconfig, which overrides the indent and save
# settings above for files it covers.
use_editorconfig = true
# Keep the def or class you are inside pinned to the top of the editor once
# its own line has scrolled out of sight, up to four levels of nesting. Click a
# pinned row to jump back to it.
sticky_scopes = true
# Stop the caret blinking. Repeating animation is distracting for some people
# and genuinely disabling for a few, and the caret is the one animation that
# is on screen the whole time you are reading.
reduce_motion = false
# Which graphics backend to draw with: \"glow\" (OpenGL) or \"wgpu\" (Direct3D,
# Metal, Vulkan). They look identical; glow starts about sixteen times faster,
# which is why it is the default. Switch to wgpu if the window fails to appear
# or draws incorrectly, which would mean this machine's OpenGL driver is at
# fault. Takes effect at the next start. If The Editor will not start at all,
# set the THE_EDITOR_RENDERER environment variable instead.
renderer = \"glow\"

[python]
# Leave empty to auto-detect: a .venv in the project, else python on PATH,
# else an installation made by the Windows Python Install Manager.
interpreter = \"\"
# Typing \"\"\" on the first line of a def or class body writes a docstring
# skeleton from the signature above it: one entry per parameter, the return
# type, and anything the body raises. \"google\", \"numpy\", \"sphinx\", or
# \"off\" to leave the quotes alone.
docstrings = \"google\"
# How strictly Pyright checks types: \"off\" (default), \"basic\", \"standard\"
# or \"strict\". Off, it still provides hover, completion and Go to Definition,
# and Ruff still reports undefined names, unused imports and the like. The
# stricter modes suit code written with type hints throughout; on code that
# is not, most of what they report is a gap in a library's stubs rather than a
# bug. A project's own pyrightconfig.json or [tool.pyright] takes precedence.
type_checking = \"off\"

[lsp]
# Language servers to leave alone, by id: \"ruff\", \"pyright\", \"pylsp\",
# \"rust-analyzer\", \"taplo\". A type checker whose findings you do not trust is
# worse than none, and this turns one off without uninstalling it.
disabled = []
";

/// Defaults, in one place so the accessors and the documentation above cannot
/// drift apart.
mod defaults {
    pub(super) const SYNTAX_THEME: &str = "follow";
    pub(super) const UI_FONT_SIZE: f32 = 14.0;
    pub(super) const UI_SCALE: f32 = 1.0;
    pub(super) const SHOW_FILE_TREE: bool = true;
    /// Off: a file that vanishes when the next one is clicked looks like a
    /// file that failed to stay open, to anyone who has not met the idea.
    pub(super) const PREVIEW_TABS: bool = false;
    pub(super) const RESTORE_SESSION: bool = true;
    pub(super) const FONT_SIZE: f32 = 13.0;
    pub(super) const TAB_WIDTH: usize = 4;
    pub(super) const INSERT_SPACES: bool = true;
    pub(super) const WORD_WRAP: bool = false;
    pub(super) const AUTO_CLOSE_BRACKETS: bool = true;
    pub(super) const REDUCE_MOTION: bool = false;
    pub(super) const STICKY_SCOPES: bool = true;

    /// Guard rails for hand-edited values. A `ui_scale = 40.0` should clamp to
    /// something usable rather than render an unrecoverable window.
    pub(super) const UI_SCALE_RANGE: (f32, f32) = (0.5, 3.0);
    pub(super) const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
    pub(super) const UI_FONT_SIZE_RANGE: (f32, f32) = (9.0, 32.0);
    pub(super) const TAB_WIDTH_RANGE: (usize, usize) = (1, 16);
}

/// How much of a diagnostic shows up in the text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineDiagnostics {
    /// Everything a server reports.
    All,
    /// Errors only. The default, because a type checker that cannot resolve a
    /// project's imports reports most of its lines, and a file underlined from
    /// end to end cannot be read let alone edited. Warnings stay in the gutter
    /// and the Problems panel.
    #[default]
    Errors,
    /// Nothing in the text.
    None,
}

impl UnderlineDiagnostics {
    pub const ALL: [Self; 3] = [Self::All, Self::Errors, Self::None];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Errors => "errors",
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Errors and warnings",
            Self::Errors => "Errors only",
            Self::None => "Nothing",
        }
    }
}

/// How a generated docstring is laid out. Defined where the generating is.
/// How a generated docstring is laid out.
///
/// Three are offered because there is no winner: Google's is the most
/// readable, NumPy's is the convention across the scientific stack, and
/// Sphinx's reST is what older codebases and `autodoc` expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocstringStyle {
    /// `Args:` / `Returns:`, indented under each heading.
    #[default]
    Google,
    /// `Parameters` / `Returns` over a row of dashes.
    Numpy,
    /// `:param x:` / `:rtype:`, which is what Sphinx `autodoc` reads.
    Sphinx,
}

impl DocstringStyle {
    pub const ALL: [Self; 3] = [Self::Google, Self::Numpy, Self::Sphinx];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Numpy => "numpy",
            Self::Sphinx => "sphinx",
        }
    }

    /// Parse the name a settings file uses.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "google" => Some(Self::Google),
            "numpy" => Some(Self::Numpy),
            "sphinx" | "rest" => Some(Self::Sphinx),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Numpy => "NumPy",
            Self::Sphinx => "Sphinx (reST)",
        }
    }
}

/// How strictly Pyright checks types, as its `typeCheckingMode`.
///
/// Off by default, as in Pylance. Pyright's findings are statements about
/// types, and in code written without type hints they are mostly about the
/// stubs: a wxPython project of 28,000 lines drew around 480 errors at both
/// "basic" and "standard", none of them a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeChecking {
    #[default]
    Off,
    Basic,
    Standard,
    Strict,
}

impl TypeChecking {
    pub const ALL: [Self; 4] = [Self::Off, Self::Basic, Self::Standard, Self::Strict];

    /// The name pyright and the settings file both use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Basic => "basic",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == name)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Basic => "Basic",
            Self::Standard => "Standard",
            Self::Strict => "Strict",
        }
    }
}

/// The graphics backend to draw with.
///
/// PLAN.md D1 chose wgpu with glow as the fallback. Measurement reversed that:
/// on Windows, wgpu spends about 1.3 seconds creating its device before the
/// first frame, against 80 ms for glow, and the two are pixel-identical. That
/// is 2.6x the startup budget spent on nothing the user can see, every launch.
/// wgpu stays available because OpenGL drivers are the weaker link on some
/// machines, and a renderer that will not start needs an alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Renderer {
    #[default]
    Glow,
    Wgpu,
}

impl Renderer {
    #[must_use]
    pub fn parse(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "wgpu" => Self::Wgpu,
            // Anything unrecognised gets the default rather than an error: a
            // typo in a settings file must not stop the editor starting.
            _ => Self::Glow,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Glow => "glow",
            Self::Wgpu => "wgpu",
        }
    }
}

/// Loaded settings plus the file they came from.
#[derive(Debug, Clone)]
pub struct Settings {
    doc: DocumentMut,
    path: Option<PathBuf>,
    /// Set by every mutator, cleared by [`Settings::save`]. Lets the app save
    /// on a timer without rewriting an unchanged file every tick.
    dirty: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            doc: DEFAULT_SETTINGS_TOML
                .parse::<DocumentMut>()
                .unwrap_or_default(),
            path: None,
            dirty: false,
        }
    }
}

impl Settings {
    /// Load from `path`.
    ///
    /// A missing file is not an error — it yields the documented defaults,
    /// which [`Settings::save`] will then write out. A *malformed* file is also
    /// not fatal: the defaults are used and the parse error is returned
    /// alongside them so the caller can show it, because refusing to start
    /// because of a stray bracket in a config file is unacceptable behaviour
    /// for an editor.
    #[must_use]
    pub fn load(path: &Path) -> (Self, Option<anyhow::Error>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (
                    Self {
                        path: Some(path.to_path_buf()),
                        dirty: true,
                        ..Self::default()
                    },
                    None,
                );
            }
            Err(e) => {
                let err = anyhow::Error::new(e).context(format!("reading {}", path.display()));
                return (
                    Self {
                        path: Some(path.to_path_buf()),
                        ..Self::default()
                    },
                    Some(err),
                );
            }
        };

        match text.parse::<DocumentMut>() {
            Ok(doc) => (
                Self {
                    doc,
                    path: Some(path.to_path_buf()),
                    dirty: false,
                },
                None,
            ),
            Err(e) => (
                Self {
                    path: Some(path.to_path_buf()),
                    ..Self::default()
                },
                Some(anyhow::Error::new(e).context(format!("parsing {}", path.display()))),
            ),
        }
    }

    /// Write back to the file it was loaded from, if anything changed.
    ///
    /// The write is atomic: a temporary file in the same directory, then a
    /// rename over the original. A power cut mid-save leaves the old settings
    /// intact rather than a half-written file.
    ///
    /// # Errors
    /// If the file cannot be written or replaced.
    pub fn save(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;

        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.doc.to_string())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;

        self.dirty = false;
        Ok(())
    }

    /// True if there are unsaved changes.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The file these settings live in, for "Open settings.toml".
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The whole document as text, for the settings editor's escape hatch.
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.doc.to_string()
    }

    // ---- typed accessors -------------------------------------------------

    /// UI theme preference. Unrecognised values fall back to the default
    /// rather than refusing to start.
    #[must_use]
    pub fn theme(&self) -> ThemePreference {
        self.str_at("ui", "theme")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default()
    }

    pub fn set_theme(&mut self, theme: ThemePreference) {
        self.set("ui", "theme", value(theme.to_string()));
    }

    /// Name of the syntax theme, or `"follow"` to track the UI theme.
    #[must_use]
    pub fn syntax_theme(&self) -> String {
        self.str_at("ui", "syntax_theme")
            .unwrap_or(defaults::SYNTAX_THEME)
            .to_owned()
    }

    pub fn set_syntax_theme(&mut self, name: &str) {
        self.set("ui", "syntax_theme", value(name));
    }

    /// Size of interface text — menus, tabs, the file tree, the status bar.
    /// Independent of [`Self::font_size`], which is the code pane.
    #[must_use]
    pub fn ui_font_size(&self) -> f32 {
        clamp_f32(
            self.f32_at("ui", "font_size")
                .unwrap_or(defaults::UI_FONT_SIZE),
            defaults::UI_FONT_SIZE_RANGE,
        )
    }

    pub fn set_ui_font_size(&mut self, size: f32) {
        let size = clamp_f32(size, defaults::UI_FONT_SIZE_RANGE);
        self.set("ui", "font_size", value(f64::from(size)));
    }

    /// Zoom applied on top of the display's own DPI scaling. 1.0 means "use
    /// whatever the monitor reports".
    #[must_use]
    pub fn ui_scale(&self) -> f32 {
        clamp_f32(
            self.f32_at("ui", "ui_scale").unwrap_or(defaults::UI_SCALE),
            defaults::UI_SCALE_RANGE,
        )
    }

    pub fn set_ui_scale(&mut self, scale: f32) {
        let scale = clamp_f32(scale, defaults::UI_SCALE_RANGE);
        self.set("ui", "ui_scale", value(f64::from(scale)));
    }

    #[must_use]
    pub fn show_file_tree(&self) -> bool {
        self.bool_at("ui", "show_file_tree")
            .unwrap_or(defaults::SHOW_FILE_TREE)
    }

    pub fn set_show_file_tree(&mut self, show: bool) {
        self.set("ui", "show_file_tree", value(show));
    }

    /// Whether a single click in the explorer opens a reusable preview tab
    /// rather than a tab of its own.
    #[must_use]
    pub fn preview_tabs(&self) -> bool {
        self.bool_at("ui", "preview_tabs")
            .unwrap_or(defaults::PREVIEW_TABS)
    }

    pub fn set_preview_tabs(&mut self, on: bool) {
        self.set("ui", "preview_tabs", value(on));
    }

    #[must_use]
    pub fn restore_session(&self) -> bool {
        self.bool_at("ui", "restore_session")
            .unwrap_or(defaults::RESTORE_SESSION)
    }

    pub fn set_restore_session(&mut self, restore: bool) {
        self.set("ui", "restore_session", value(restore));
    }

    #[must_use]
    pub fn font_size(&self) -> f32 {
        clamp_f32(
            self.f32_at("editor", "font_size")
                .unwrap_or(defaults::FONT_SIZE),
            defaults::FONT_SIZE_RANGE,
        )
    }

    pub fn set_font_size(&mut self, size: f32) {
        let size = clamp_f32(size, defaults::FONT_SIZE_RANGE);
        self.set("editor", "font_size", value(f64::from(size)));
    }

    #[must_use]
    pub fn tab_width(&self) -> usize {
        let (lo, hi) = defaults::TAB_WIDTH_RANGE;
        self.int_at("editor", "tab_width")
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(defaults::TAB_WIDTH)
            .clamp(lo, hi)
    }

    /// Clamped on write as well as on read, so the settings form cannot store a
    /// value the accessor would then quietly ignore.
    pub fn set_tab_width(&mut self, width: usize) {
        let (lo, hi) = defaults::TAB_WIDTH_RANGE;
        let width = width.clamp(lo, hi);
        self.set("editor", "tab_width", value(width as i64));
    }

    #[must_use]
    pub fn insert_spaces(&self) -> bool {
        self.bool_at("editor", "insert_spaces")
            .unwrap_or(defaults::INSERT_SPACES)
    }

    pub fn set_insert_spaces(&mut self, spaces: bool) {
        self.set("editor", "insert_spaces", value(spaces));
    }

    #[must_use]
    pub fn word_wrap(&self) -> bool {
        self.bool_at("editor", "word_wrap")
            .unwrap_or(defaults::WORD_WRAP)
    }

    /// Path to the Python interpreter, or empty to auto-detect.
    #[must_use]
    pub fn python_interpreter(&self) -> String {
        self.str_at("python", "interpreter")
            .unwrap_or("")
            .to_owned()
    }

    pub fn set_python_interpreter(&mut self, path: &str) {
        self.set("python", "interpreter", value(path));
    }

    /// Whether typing an opening bracket or quote inserts its closer.
    #[must_use]
    pub fn auto_close_brackets(&self) -> bool {
        self.bool_at("editor", "auto_close_brackets")
            .unwrap_or(defaults::AUTO_CLOSE_BRACKETS)
    }

    /// Which graphics backend to draw with.
    ///
    /// Read before the window exists, so this is a plain string rather than an
    /// enum the rest of the application knows about.
    #[must_use]
    pub fn renderer(&self) -> Renderer {
        // The environment variable wins, because the reason to change this is
        // usually that the window did not appear -- and you cannot reach the
        // settings form through a window that did not appear.
        if let Ok(name) = std::env::var("THE_EDITOR_RENDERER") {
            return Renderer::parse(&name);
        }
        Renderer::parse(self.str_at("editor", "renderer").unwrap_or(""))
    }

    pub fn set_renderer(&mut self, renderer: Renderer) {
        self.set("editor", "renderer", value(renderer.as_str()));
    }

    /// Whether to pin the enclosing declarations to the top of the editor.
    #[must_use]
    pub fn sticky_scopes(&self) -> bool {
        self.bool_at("editor", "sticky_scopes")
            .unwrap_or(defaults::STICKY_SCOPES)
    }

    pub fn set_sticky_scopes(&mut self, on: bool) {
        self.set("editor", "sticky_scopes", value(on));
    }

    /// Whether to suppress repeating animation, chiefly the caret blink.
    #[must_use]
    pub fn reduce_motion(&self) -> bool {
        self.bool_at("editor", "reduce_motion")
            .unwrap_or(defaults::REDUCE_MOTION)
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        self.set("editor", "reduce_motion", value(on));
    }

    /// Which diagnostics to underline in the text.
    ///
    /// Unrecognised values fall back to the default rather than showing
    /// nothing, which would look like the language server having stopped.
    #[must_use]
    pub fn underline_diagnostics(&self) -> UnderlineDiagnostics {
        match self.str_at("editor", "underline_diagnostics") {
            Some("all") => UnderlineDiagnostics::All,
            Some("none") => UnderlineDiagnostics::None,
            _ => UnderlineDiagnostics::Errors,
        }
    }

    pub fn set_underline_diagnostics(&mut self, level: UnderlineDiagnostics) {
        self.set("editor", "underline_diagnostics", value(level.as_str()));
    }

    /// How to lay out a generated docstring, or `None` to generate none.
    ///
    /// An unrecognised name reads as the default rather than as "off": a typo
    /// in a config file should not silently switch a feature off.
    #[must_use]
    pub fn docstrings(&self) -> Option<DocstringStyle> {
        match self.str_at("python", "docstrings") {
            Some("off" | "none") => None,
            Some(name) => Some(DocstringStyle::parse(name).unwrap_or_default()),
            None => Some(DocstringStyle::default()),
        }
    }

    pub fn set_docstrings(&mut self, style: Option<DocstringStyle>) {
        let name = style.map_or("off", DocstringStyle::as_str);
        self.set("python", "docstrings", value(name));
    }

    /// How strictly Pyright checks types. An unrecognised name reads as the
    /// default.
    #[must_use]
    pub fn type_checking(&self) -> TypeChecking {
        self.str_at("python", "type_checking")
            .and_then(TypeChecking::parse)
            .unwrap_or_default()
    }

    pub fn set_type_checking(&mut self, mode: TypeChecking) {
        self.set("python", "type_checking", value(mode.as_str()));
    }

    /// Server ids the user has switched off.
    #[must_use]
    pub fn disabled_servers(&self) -> Vec<String> {
        self.item_at("lsp", "disabled")
            .and_then(Item::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn set_server_enabled(&mut self, id: &str, enabled: bool) {
        let mut disabled = self.disabled_servers();
        disabled.retain(|d| d != id);
        if !enabled {
            disabled.push(id.to_owned());
        }
        disabled.sort();
        let mut array = toml_edit::Array::new();
        for id in disabled {
            array.push(id);
        }
        self.set("lsp", "disabled", value(array));
    }

    #[must_use]
    pub fn trim_trailing_whitespace(&self) -> bool {
        self.bool_at("editor", "trim_trailing_whitespace")
            .unwrap_or(false)
    }

    pub fn set_trim_trailing_whitespace(&mut self, trim: bool) {
        self.set("editor", "trim_trailing_whitespace", value(trim));
    }

    #[must_use]
    pub fn insert_final_newline(&self) -> bool {
        self.bool_at("editor", "insert_final_newline")
            .unwrap_or(false)
    }

    pub fn set_insert_final_newline(&mut self, insert: bool) {
        self.set("editor", "insert_final_newline", value(insert));
    }

    /// Whether a project's `.editorconfig` overrides these settings.
    ///
    /// On by default: a file in the project is a statement about that code,
    /// and someone who put one there meant it.
    #[must_use]
    pub fn use_editorconfig(&self) -> bool {
        self.bool_at("editor", "use_editorconfig").unwrap_or(true)
    }

    pub fn set_use_editorconfig(&mut self, use_it: bool) {
        self.set("editor", "use_editorconfig", value(use_it));
    }

    pub fn set_auto_close_brackets(&mut self, close: bool) {
        self.set("editor", "auto_close_brackets", value(close));
    }

    pub fn set_word_wrap(&mut self, wrap: bool) {
        self.set("editor", "word_wrap", value(wrap));
    }

    // ---- document plumbing -----------------------------------------------

    fn item_at(&self, section: &str, key: &str) -> Option<&Item> {
        self.doc.get(section)?.as_table_like()?.get(key)
    }

    fn str_at(&self, section: &str, key: &str) -> Option<&str> {
        self.item_at(section, key)?.as_str()
    }

    fn f32_at(&self, section: &str, key: &str) -> Option<f32> {
        let item = self.item_at(section, key)?;
        // Accept `ui_scale = 1` as well as `1.0`; TOML distinguishes them and
        // users do not.
        item.as_float()
            .map(|f| f as f32)
            .or_else(|| item.as_integer().map(|i| i as f32))
    }

    fn int_at(&self, section: &str, key: &str) -> Option<i64> {
        self.item_at(section, key)?.as_integer()
    }

    fn bool_at(&self, section: &str, key: &str) -> Option<bool> {
        self.item_at(section, key)?.as_bool()
    }

    fn set(&mut self, section: &str, key: &str, item: Item) {
        let entry = self
            .doc
            .entry(section)
            .or_insert_with(|| Item::Table(Table::new()));
        if let Some(table) = entry.as_table_like_mut() {
            table.insert(key, item);
            self.dirty = true;
        } else {
            // The section exists but is not a table (e.g. `ui = 3` in a
            // hand-edited file). Replace it rather than silently doing nothing.
            let mut table = Table::new();
            table.insert(key, item);
            *entry = Item::Table(table);
            self.dirty = true;
        }
    }
}

fn clamp_f32(v: f32, (lo, hi): (f32, f32)) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { lo }
}

/// Convenience so callers can write `value(x)` for the common scalar types.
#[allow(dead_code)]
fn as_value(v: impl Into<Value>) -> Item {
    Item::Value(v.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_toml(s: &str) -> Settings {
        Settings {
            doc: s.parse().expect("test input must be valid TOML"),
            path: None,
            dirty: false,
        }
    }

    #[test]
    fn shipped_defaults_parse_and_match_the_accessors() {
        let s = from_toml(DEFAULT_SETTINGS_TOML);
        assert_eq!(s.theme(), ThemePreference::Dark);
        assert_eq!(s.syntax_theme(), "follow");
        assert!((s.ui_font_size() - 14.0).abs() < f32::EPSILON);
        assert!((s.ui_scale() - 1.0).abs() < f32::EPSILON);
        assert!(s.show_file_tree());
        assert!(s.restore_session());
        assert!((s.font_size() - 13.0).abs() < f32::EPSILON);
        assert_eq!(s.tab_width(), 4);
        assert!(s.insert_spaces());
        assert!(!s.word_wrap());
        assert!(s.sticky_scopes());
    }

    #[test]
    fn an_empty_file_yields_every_default() {
        let s = from_toml("");
        assert_eq!(s.theme(), ThemePreference::Dark);
        assert_eq!(s.tab_width(), 4);
        assert!(!s.word_wrap());
    }

    #[test]
    fn unknown_keys_and_comments_survive_a_write() {
        let original = "\
# my notes
[ui]
theme = \"light\"
something_from_a_newer_version = 42

[experimental]
whatever = true
";
        let mut s = from_toml(original);
        s.set_theme(ThemePreference::Dark);
        let out = s.to_toml();

        assert!(out.contains("# my notes"), "comments must survive");
        assert!(
            out.contains("something_from_a_newer_version = 42"),
            "unknown keys must survive: {out}"
        );
        assert!(out.contains("[experimental]"), "unknown sections too");
        assert!(out.contains("theme = \"dark\""), "the change must apply");
    }

    #[test]
    fn wrong_types_fall_back_instead_of_failing() {
        let s = from_toml("[editor]\ntab_width = \"four\"\nfont_size = true\n");
        assert_eq!(s.tab_width(), 4);
        assert!((s.font_size() - 13.0).abs() < f32::EPSILON);
    }

    #[test]
    fn out_of_range_values_are_clamped_to_something_usable() {
        let s = from_toml("[ui]\nui_scale = 40.0\nfont_size = 0.1\n\n[editor]\ntab_width = 900\n");
        assert!(
            (s.ui_scale() - 3.0).abs() < f32::EPSILON,
            "a hand-edited ui_scale of 40 must not render an unusable window"
        );
        assert!((s.ui_font_size() - 9.0).abs() < f32::EPSILON);
        assert_eq!(s.tab_width(), 16);
    }

    #[test]
    fn interface_and_editor_font_sizes_are_independent() {
        let s = from_toml("[ui]\nfont_size = 18.0\n\n[editor]\nfont_size = 11.0\n");
        assert!((s.ui_font_size() - 18.0).abs() < f32::EPSILON);
        assert!((s.font_size() - 11.0).abs() < f32::EPSILON);
    }

    #[test]
    fn integers_are_accepted_where_floats_are_expected() {
        let s = from_toml("[ui]\nui_scale = 2\n");
        assert!((s.ui_scale() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_section_that_is_not_a_table_gets_replaced_rather_than_ignored() {
        let mut s = from_toml("ui = 3\n");
        s.set_theme(ThemePreference::Light);
        assert_eq!(s.theme(), ThemePreference::Light);
    }

    #[test]
    fn preview_tabs_are_off_by_default_and_can_be_turned_on() {
        let mut settings = Settings::default();
        assert!(!settings.preview_tabs());
        settings.set_preview_tabs(true);
        assert!(settings.preview_tabs());
        assert!(
            from_toml(&settings.to_toml()).preview_tabs(),
            "survives a write"
        );
    }

    #[test]
    fn sticky_scopes_is_on_by_default_and_can_be_turned_off() {
        let mut settings = Settings::default();
        assert!(settings.sticky_scopes());
        settings.set_sticky_scopes(false);
        assert!(!settings.sticky_scopes());
        assert!(
            !from_toml(&settings.to_toml()).sticky_scopes(),
            "survives a write"
        );
    }

    #[test]
    fn mutating_marks_dirty_and_reading_does_not() {
        let mut s = from_toml(DEFAULT_SETTINGS_TOML);
        assert!(!s.is_dirty());
        let _ = s.theme();
        assert!(!s.is_dirty());
        s.set_theme(ThemePreference::Light);
        assert!(s.is_dirty());
    }

    #[test]
    fn theme_round_trips_through_the_file() {
        let mut s = from_toml("");
        for pref in ThemePreference::ALL {
            s.set_theme(pref);
            assert_eq!(from_toml(&s.to_toml()).theme(), pref);
        }
    }

    #[test]
    fn docstrings_default_to_google_and_can_be_turned_off() {
        let mut settings = Settings::default();
        assert_eq!(
            settings.docstrings(),
            Some(DocstringStyle::Google),
            "the documented default"
        );

        settings.set_docstrings(None);
        assert_eq!(settings.docstrings(), None);
        assert!(
            settings.to_toml().contains("docstrings = \"off\""),
            "off is written out as a word, not by deleting the key"
        );

        settings.set_docstrings(Some(DocstringStyle::Sphinx));
        assert_eq!(settings.docstrings(), Some(DocstringStyle::Sphinx));
    }

    /// A typo in a hand-edited config should not silently switch a feature
    /// off — that is indistinguishable from a bug in the editor.
    #[test]
    fn an_unrecognised_docstring_style_falls_back_rather_than_off() {
        let settings = from_toml("[python]\ndocstrings = \"gooogle\"\n");
        assert_eq!(settings.docstrings(), Some(DocstringStyle::Google));
    }

    #[test]
    fn type_checking_defaults_to_off_and_round_trips() {
        let mut settings = Settings::default();
        assert_eq!(settings.type_checking(), TypeChecking::Off);
        for mode in TypeChecking::ALL {
            settings.set_type_checking(mode);
            assert_eq!(settings.type_checking(), mode);
        }
        settings.set("python", "type_checking", value("pedantic"));
        assert_eq!(settings.type_checking(), TypeChecking::Off);
    }

    #[test]
    fn every_docstring_style_survives_being_written_and_read_back() {
        for style in DocstringStyle::ALL {
            let mut settings = Settings::default();
            settings.set_docstrings(Some(style));
            assert_eq!(settings.docstrings(), Some(style), "{style:?}");
        }
    }
}
