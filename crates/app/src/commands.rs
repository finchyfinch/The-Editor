//! The command registry.
//!
//! Every user-facing action is registered here exactly once, with its title,
//! category and keyboard shortcut. The menus, the toolbar, the command palette
//! and the Help → Keyboard Shortcuts page are all generated from this list.
//!
//! The point is not tidiness. It is that a menu item and its keyboard shortcut
//! cannot drift apart into doing different things, and that a new command
//! cannot be added to a menu but forgotten in the palette. See PLAN.md §5.

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

/// Every action The Editor can perform.
///
/// Adding a variant without adding it to [`registry`] fails the exhaustiveness
/// test at the bottom of this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CommandId {
    // File
    NewFile,
    NewScratch,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseTab,
    NextTab,
    PreviousTab,
    CloseFolder,
    Exit,
    // Edit
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ToggleComment,
    Indent,
    Outdent,
    Find,
    Replace,
    FindNext,
    FindPrevious,
    FindInProject,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    GoToFile,
    GoToSymbol,
    ToggleFold,
    FoldAll,
    UnfoldAll,
    TriggerCompletion,
    GoToDefinition,
    FindUses,
    RenameSymbol,
    AddCursorAtNextMatch,
    NextUse,
    PreviousUse,
    // Run
    Run,
    RunStop,
    RunRestart,
    RunTests,
    RunTestsInFile,
    RunTestAtCaret,
    RunFailedTests,
    ToggleBreakpoint,
    DebugStart,
    DebugStop,
    DebugStepOver,
    DebugStepInto,
    DebugStepOut,
    ShowOutput,
    ShowTerminal,
    ShowPackages,
    ShowProblems,
    ShowDiff,
    ShowSourceControl,
    ToggleBlame,
    SelectInterpreter,
    CreateVenv,
    // View
    ToggleExplorer,
    ThemeDark,
    ThemeLight,
    ThemeSystem,
    ToggleHiddenFiles,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    // Tools
    CommandPalette,
    OpenSettings,
    OpenSettingsFile,
    // Help
    About,
    KeyboardShortcuts,
    UserManual,
    ThirdPartyLicences,
    CheckToolchains,
    OpenLogFolder,
}

impl CommandId {
    /// Every command, in registry order. Used by the palette and the
    /// exhaustiveness test.
    pub(crate) const ALL: &'static [Self] = &[
        Self::NewFile,
        Self::NewScratch,
        Self::OpenFile,
        Self::OpenFolder,
        Self::Save,
        Self::SaveAs,
        Self::SaveAll,
        Self::CloseTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::CloseFolder,
        Self::Exit,
        Self::Undo,
        Self::Redo,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::SelectAll,
        Self::ToggleComment,
        Self::Indent,
        Self::Outdent,
        Self::Find,
        Self::Replace,
        Self::FindNext,
        Self::FindPrevious,
        Self::FindInProject,
        Self::DuplicateLine,
        Self::DeleteLine,
        Self::MoveLineUp,
        Self::MoveLineDown,
        Self::GoToFile,
        Self::GoToSymbol,
        Self::ToggleFold,
        Self::FoldAll,
        Self::UnfoldAll,
        Self::TriggerCompletion,
        Self::GoToDefinition,
        Self::FindUses,
        Self::RenameSymbol,
        Self::AddCursorAtNextMatch,
        Self::NextUse,
        Self::PreviousUse,
        Self::Run,
        Self::RunStop,
        Self::RunRestart,
        Self::RunTests,
        Self::RunTestsInFile,
        Self::RunTestAtCaret,
        Self::RunFailedTests,
        Self::ToggleBreakpoint,
        Self::DebugStart,
        Self::DebugStop,
        Self::DebugStepOver,
        Self::DebugStepInto,
        Self::DebugStepOut,
        Self::ShowOutput,
        Self::ShowTerminal,
        Self::ShowPackages,
        Self::ShowProblems,
        Self::ShowDiff,
        Self::ShowSourceControl,
        Self::ToggleBlame,
        Self::SelectInterpreter,
        Self::CreateVenv,
        Self::ToggleExplorer,
        Self::ThemeDark,
        Self::ThemeLight,
        Self::ThemeSystem,
        Self::ToggleHiddenFiles,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ZoomReset,
        Self::CommandPalette,
        Self::OpenSettings,
        Self::OpenSettingsFile,
        Self::About,
        Self::KeyboardShortcuts,
        Self::UserManual,
        Self::ThirdPartyLicences,
        Self::CheckToolchains,
        Self::OpenLogFolder,
    ];
}

/// A registered command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Command {
    pub(crate) id: CommandId,
    /// Shown in the palette, prefixed by the category.
    pub(crate) title: &'static str,
    pub(crate) category: &'static str,
    /// The accelerator, shown in menus and the shortcut reference.
    pub(crate) shortcut: Option<KeyboardShortcut>,
    /// A second binding that also triggers this command, and is deliberately
    /// not shown anywhere: menus and the shortcut reference list one
    /// accelerator per command, because two makes the reference harder to read
    /// for something nobody needs to be told.
    ///
    /// This exists for a key that is one key on some keyboards and two on
    /// others. `+` is Shift+`=` on the usual layouts but has a key to itself on
    /// a numeric keypad, so Zoom In answers to `Ctrl+=` -- the one that is
    /// reachable everywhere, and the one the menu lists -- and to `Ctrl+Plus`
    /// besides, for the keyboards where that is a keystroke of its own.
    /// Browsers accept both and so does this.
    pub(crate) secondary: Option<KeyboardShortcut>,
    /// Whether [`triggered`] should claim this shortcut globally.
    ///
    /// Clipboard shortcuts are `false`: egui synthesises `Event::Copy`,
    /// `Event::Cut` and `Event::Paste` from the platform (including the OS
    /// menu and middle-click paste on X11), and the focused widget handles
    /// them. Claiming Ctrl+C here would consume the keystroke before the
    /// editor ever saw the event, and break copying out of a text field.
    /// The binding is still listed so the menu shows the right accelerator.
    pub(crate) global: bool,
}

impl Command {
    /// `"File: Open Folder"` — what the palette matches against and displays.
    pub(crate) fn palette_label(&self) -> String {
        format!("{}: {}", self.category, self.title)
    }

    /// Human-readable accelerator, e.g. `"Ctrl+Shift+P"`.
    pub(crate) fn shortcut_text(&self, ctx: &eframe::egui::Context) -> Option<String> {
        self.shortcut.map(|s| ctx.format_shortcut(&s))
    }
}

const fn ctrl(key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(Modifiers::COMMAND, key))
}

const fn plain(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::NONE, key)
}

const fn alt(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::ALT, key)
}

const fn alt_shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::ALT.plus(Modifiers::SHIFT), key)
}

const fn shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::SHIFT, key)
}

const fn ctrl_shift(key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(
        Modifiers::COMMAND.plus(Modifiers::SHIFT),
        key,
    ))
}

/// The registry.
///
/// `Modifiers::COMMAND` is Ctrl on Windows and Linux and Cmd on macOS, so the
/// platform mapping described in PLAN.md §6 comes for free rather than needing
/// a `cfg!` at every call site.
pub(crate) fn registry() -> &'static [Command] {
    &REGISTRY
}

static REGISTRY: [Command; CommandId::ALL.len()] = [
    cmd(CommandId::NewFile, "File", "New File...", ctrl(Key::N)),
    cmd(
        CommandId::NewScratch,
        "File",
        "New Untitled Buffer",
        ctrl_shift(Key::N),
    ),
    cmd(CommandId::OpenFile, "File", "Open File", ctrl(Key::O)),
    cmd(
        CommandId::OpenFolder,
        "File",
        "Open Folder",
        ctrl_shift(Key::O),
    ),
    cmd(CommandId::Save, "File", "Save", ctrl(Key::S)),
    cmd(CommandId::SaveAs, "File", "Save As", ctrl_shift(Key::S)),
    cmd(CommandId::SaveAll, "File", "Save All", None),
    cmd(CommandId::CloseTab, "File", "Close Tab", ctrl(Key::W)),
    // Ctrl+Tab, in most-recently-used order. Registered before Close Tab so
    // the more specific Ctrl+Shift+Tab is tried first; see `triggered`.
    cmd(
        CommandId::PreviousTab,
        "File",
        "Previous Tab",
        Some(KeyboardShortcut::new(
            Modifiers::COMMAND.plus(Modifiers::SHIFT),
            Key::Tab,
        )),
    ),
    cmd(
        CommandId::NextTab,
        "File",
        "Next Tab",
        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Tab)),
    ),
    cmd(CommandId::CloseFolder, "File", "Close Folder", None),
    cmd(CommandId::Exit, "File", "Exit", None),
    cmd(CommandId::Undo, "Edit", "Undo", ctrl(Key::Z)),
    cmd(CommandId::Redo, "Edit", "Redo", ctrl_shift(Key::Z)),
    // Clipboard bindings are listed for display but not claimed globally; see
    // the `global` field on `Command`.
    view_cmd(CommandId::Cut, "Edit", "Cut", ctrl(Key::X)),
    view_cmd(CommandId::Copy, "Edit", "Copy", ctrl(Key::C)),
    view_cmd(CommandId::Paste, "Edit", "Paste", ctrl(Key::V)),
    cmd(CommandId::SelectAll, "Edit", "Select All", ctrl(Key::A)),
    cmd(
        CommandId::ToggleComment,
        "Edit",
        "Toggle Comment",
        ctrl(Key::Slash),
    ),
    // Tab and Shift+Tab are handled by the editor view, which has to decide
    // between indenting a block and inserting a tab character. These are the
    // menu-reachable equivalents.
    cmd(
        CommandId::Indent,
        "Edit",
        "Indent Lines",
        ctrl(Key::CloseBracket),
    ),
    cmd(
        CommandId::Outdent,
        "Edit",
        "Outdent Lines",
        ctrl(Key::OpenBracket),
    ),
    cmd(CommandId::Find, "Edit", "Find", ctrl(Key::F)),
    cmd(CommandId::Replace, "Edit", "Replace", ctrl(Key::H)),
    cmd(
        CommandId::FindNext,
        "Edit",
        "Find Next",
        Some(plain(Key::F3)),
    ),
    cmd(
        CommandId::FindInProject,
        "Edit",
        "Find in Project",
        ctrl_shift(Key::F),
    ),
    cmd(
        CommandId::FindPrevious,
        "Edit",
        "Find Previous",
        Some(shift(Key::F3)),
    ),
    // The bindings every other editor uses for these, so the muscle memory
    // transfers. Alt is free here: the editor claims the plain arrows for the
    // caret, and `matches_exact` keeps the two apart.
    cmd(
        CommandId::DuplicateLine,
        "Edit",
        "Duplicate Line",
        ctrl_shift(Key::D),
    ),
    cmd(
        CommandId::DeleteLine,
        "Edit",
        "Delete Line",
        ctrl_shift(Key::K),
    ),
    cmd(
        CommandId::MoveLineUp,
        "Edit",
        "Move Line Up",
        Some(alt(Key::ArrowUp)),
    ),
    cmd(
        CommandId::MoveLineDown,
        "Edit",
        "Move Line Down",
        Some(alt(Key::ArrowDown)),
    ),
    cmd(CommandId::GoToFile, "File", "Go to File", ctrl(Key::P)),
    // Ctrl+R, as Sublime Text has it. Ctrl+Shift+O, which is VS Code's, is
    // already Open Folder here and that is the more frequently reached for of
    // the two.
    cmd(CommandId::GoToSymbol, "Edit", "Go to Symbol", ctrl(Key::R)),
    // The three VS Code uses, which are also PyCharm's on Windows.
    cmd(
        CommandId::ToggleFold,
        "View",
        "Toggle Fold",
        ctrl_shift(Key::OpenBracket),
    ),
    cmd(
        CommandId::FoldAll,
        "View",
        "Fold All",
        ctrl_shift(Key::Minus),
    ),
    cmd(
        CommandId::UnfoldAll,
        "View",
        "Unfold All",
        ctrl_shift(Key::Plus),
    ),
    cmd(
        CommandId::TriggerCompletion,
        "Edit",
        "Suggest Completions",
        ctrl(Key::Space),
    ),
    // F12 and Shift+F12 are what every IDE uses for these. F8/Shift+F8 walks
    // the results, matching Find Next/Previous one row above.
    cmd(
        CommandId::GoToDefinition,
        "Edit",
        "Go to Definition",
        Some(plain(Key::F12)),
    ),
    cmd(
        CommandId::FindUses,
        "Edit",
        "Find Uses",
        Some(shift(Key::F12)),
    ),
    cmd(
        CommandId::RenameSymbol,
        "Edit",
        "Rename Symbol",
        Some(plain(Key::F2)),
    ),
    cmd(
        CommandId::AddCursorAtNextMatch,
        "Edit",
        "Add Cursor at Next Match",
        ctrl(Key::D),
    ),
    cmd(CommandId::NextUse, "Edit", "Next Use", Some(plain(Key::F8))),
    cmd(
        CommandId::PreviousUse,
        "Edit",
        "Previous Use",
        Some(shift(Key::F8)),
    ),
    cmd(CommandId::Run, "Run", "Run", Some(plain(Key::F5))),
    cmd(CommandId::RunStop, "Run", "Stop", Some(shift(Key::F5))),
    cmd(CommandId::RunRestart, "Run", "Restart", ctrl(Key::F5)),
    cmd(CommandId::RunTests, "Run", "Run Tests", None),
    cmd(
        CommandId::ShowTerminal,
        "Run",
        "Terminal",
        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Backtick)),
    ),
    cmd(CommandId::ShowPackages, "Tools", "Packages", None),
    // The bindings every debugger uses, except for start: F5 is already Run,
    // and quietly changing what F5 does depending on state would be worse than
    // a second key. Alt+F5 both starts and continues, which is one idea.
    cmd(
        CommandId::ToggleBreakpoint,
        "Run",
        "Toggle Breakpoint",
        Some(plain(Key::F9)),
    ),
    cmd(
        CommandId::DebugStart,
        "Run",
        "Start Debugging / Continue",
        Some(alt(Key::F5)),
    ),
    cmd(
        CommandId::DebugStop,
        "Run",
        "Stop Debugging",
        Some(alt_shift(Key::F5)),
    ),
    cmd(
        CommandId::DebugStepOver,
        "Run",
        "Step Over",
        Some(plain(Key::F10)),
    ),
    cmd(
        CommandId::DebugStepInto,
        "Run",
        "Step Into",
        Some(plain(Key::F11)),
    ),
    cmd(
        CommandId::DebugStepOut,
        "Run",
        "Step Out",
        Some(shift(Key::F11)),
    ),
    cmd(
        CommandId::ShowOutput,
        "View",
        "Toggle Output Panel",
        ctrl(Key::J),
    ),
    cmd(
        CommandId::ShowProblems,
        "View",
        "Problems",
        ctrl_shift(Key::M),
    ),
    cmd(
        CommandId::ShowDiff,
        "View",
        "Changes Since Last Commit",
        ctrl_shift(Key::G),
    ),
    cmd(
        CommandId::RunTestsInFile,
        "Run",
        "Run Tests in This File",
        None,
    ),
    cmd(
        CommandId::RunTestAtCaret,
        "Run",
        "Run the Test at the Caret",
        ctrl_shift(Key::T),
    ),
    cmd(
        CommandId::RunFailedTests,
        "Run",
        "Run Failed Tests Again",
        None,
    ),
    cmd(
        CommandId::ShowSourceControl,
        "View",
        "Source Control",
        ctrl(Key::G),
    ),
    cmd(
        CommandId::ToggleBlame,
        "View",
        "Blame Annotations",
        ctrl_shift(Key::B),
    ),
    cmd(
        CommandId::SelectInterpreter,
        "Run",
        "Select Python Interpreter",
        None,
    ),
    cmd(
        CommandId::CreateVenv,
        "Run",
        "Create Virtual Environment...",
        None,
    ),
    cmd(
        CommandId::ToggleExplorer,
        "View",
        "Toggle Explorer",
        ctrl(Key::B),
    ),
    cmd(CommandId::ThemeDark, "View", "Theme: Dark", None),
    cmd(CommandId::ThemeLight, "View", "Theme: Light", None),
    cmd(CommandId::ThemeSystem, "View", "Theme: Follow System", None),
    cmd(
        CommandId::ToggleHiddenFiles,
        "View",
        "Toggle Hidden Files",
        None,
    ),
    // Ctrl+= rather than Ctrl+Plus, and that is not a typo.
    //
    // A `+` is a shifted `=` on the usual layouts, so "Ctrl+Plus" is really
    // Ctrl+Shift+= -- which is Unfold All two entries above, and wins because
    // it demands more modifiers. Advertising Ctrl+Plus therefore advertised a
    // key that unfolds the file. Ctrl+Plus stays as the unlisted second
    // binding for the keyboards that have a `+` of their own, a numeric keypad
    // among them, where it is reachable without shift and collides with
    // nothing.
    cmd_also(
        CommandId::ZoomIn,
        "View",
        "Zoom In",
        ctrl(Key::Equals),
        ctrl(Key::Plus),
    ),
    cmd(CommandId::ZoomOut, "View", "Zoom Out", ctrl(Key::Minus)),
    cmd(CommandId::ZoomReset, "View", "Reset Zoom", ctrl(Key::Num0)),
    cmd(
        CommandId::CommandPalette,
        "Tools",
        "Command Palette",
        ctrl_shift(Key::P),
    ),
    cmd(
        CommandId::OpenSettings,
        "Tools",
        "Settings",
        ctrl(Key::Comma),
    ),
    // The file keeps its place in the menu as the escape hatch, but loses the
    // accelerator to the form: Ctrl+, is what people press expecting a settings
    // window, not a text buffer.
    cmd(
        CommandId::OpenSettingsFile,
        "Tools",
        "Open settings.toml",
        None,
    ),
    cmd(CommandId::About, "Help", "About The Editor", None),
    cmd(
        CommandId::KeyboardShortcuts,
        "Help",
        "Keyboard Shortcuts",
        None,
    ),
    cmd(CommandId::UserManual, "Help", "User Manual", None),
    cmd(
        CommandId::ThirdPartyLicences,
        "Help",
        "Third-Party Licences",
        None,
    ),
    cmd(CommandId::CheckToolchains, "Help", "Check Toolchains", None),
    cmd(CommandId::OpenLogFolder, "Help", "Open Log Folder", None),
];

/// A command whose shortcut the application claims globally.
const fn cmd(
    id: CommandId,
    category: &'static str,
    title: &'static str,
    shortcut: Option<KeyboardShortcut>,
) -> Command {
    Command {
        id,
        title,
        category,
        shortcut,
        secondary: None,
        global: true,
    }
}

/// A command with a second, unlisted binding. See [`Command::secondary`].
const fn cmd_also(
    id: CommandId,
    category: &'static str,
    title: &'static str,
    shortcut: Option<KeyboardShortcut>,
    secondary: Option<KeyboardShortcut>,
) -> Command {
    Command {
        secondary,
        ..cmd(id, category, title, shortcut)
    }
}

/// A command the focused view handles itself. The binding is recorded so menus
/// show the accelerator, but it is not consumed at the application level.
const fn view_cmd(
    id: CommandId,
    category: &'static str,
    title: &'static str,
    shortcut: Option<KeyboardShortcut>,
) -> Command {
    Command {
        global: false,
        ..cmd(id, category, title, shortcut)
    }
}
/// Look up a registered command.
pub(crate) fn get(id: CommandId) -> &'static Command {
    registry()
        .iter()
        .find(|c| c.id == id)
        .expect("every CommandId is in the registry; the exhaustiveness test enforces it")
}

/// Every binding the application claims globally, in the order [`triggered`]
/// checks them: most modifiers first.
///
/// Both of a command's bindings are candidates in their own right and are
/// sorted together -- a secondary binding is no less specific than a primary
/// one, and the same rule has to cover it.
///
/// A function rather than something written out at the one call site, because
/// the tests below check this order and a copy of it in the tests is a copy
/// that can agree with itself while disagreeing with what runs.
fn claimed_bindings() -> Vec<(KeyboardShortcut, CommandId)> {
    let mut candidates: Vec<(KeyboardShortcut, CommandId)> = registry()
        .iter()
        .filter(|cmd| cmd.global)
        .flat_map(|cmd| {
            [cmd.shortcut, cmd.secondary]
                .into_iter()
                .flatten()
                .map(|sc| (sc, cmd.id))
        })
        .collect();
    candidates.sort_by_key(|(sc, _)| std::cmp::Reverse(specificity(*sc)));
    candidates
}

/// The command whose shortcut was just pressed, if any.
///
/// Consuming the shortcut prevents it also reaching a focused text field.
pub(crate) fn triggered(ctx: &eframe::egui::Context) -> Option<CommandId> {
    // Most modifiers first. egui's `consume_shortcut` matches *logically*, not
    // exactly: an extra Shift or Alt is ignored, so Ctrl+Shift+Z satisfies a
    // Ctrl+Z binding. Its own documentation says to check the more specific
    // shortcut first, and in registry order Undo (Ctrl+Z) comes before Redo
    // (Ctrl+Shift+Z) -- so Redo ran Undo, Save As ran Save, and Shift+F12 ran
    // Go to Definition.
    //
    // Sorting here rather than reordering the registry, because the registry's
    // order is what the menus and the palette read, and a future binding added
    // in the obvious place would silently reintroduce this.
    let candidates = claimed_bindings();

    // A focused text field owns the keys that mean something inside text.
    // Asked once, before the loop, because it does not change mid-frame.
    let in_text_field = ctx.text_edit_focused();

    candidates.into_iter().find_map(|(sc, id)| {
        if in_text_field && edits_text(sc) {
            // Left in the queue rather than consumed, so the field itself gets
            // it later in the frame.
            return None;
        }
        ctx.input_mut(|i| consume(i, sc)).then_some(id)
    })
}

/// Take `shortcut`'s key press out of this frame's input, if it happened.
///
/// egui's own `consume_shortcut` ignores any extra Shift or Alt, so a
/// combination nothing is bound to ran whatever its letter was bound to:
/// Ctrl+Shift+W closed the tab. And on a keyboard with an AltGr key, which
/// arrives as Ctrl+Alt, typing a letter such as Polish `ą` (AltGr+A) ran
/// Select All. So the modifiers must match exactly — except on keys where
/// Shift is part of typing the character on some layout. `/` is Shift+7 on a
/// German keyboard and digits need Shift on a French one, so Ctrl+/ really
/// arrives as Ctrl+Shift+/ there; demanding an exact match would make Toggle
/// Comment unreachable. Those keep egui's leniency, and the ordering in
/// `claimed_bindings` still decides between them.
fn consume(input: &mut egui::InputState, shortcut: KeyboardShortcut) -> bool {
    if shift_can_be_part_of(shortcut.logical_key) {
        return input.consume_shortcut(&shortcut);
    }
    let mut found = false;
    input.events.retain(|event| {
        let hit = matches!(
            event,
            egui::Event::Key { key, modifiers, pressed: true, .. }
                if *key == shortcut.logical_key && modifiers.matches_exact(shortcut.modifiers)
        );
        found |= hit;
        !hit
    });
    found
}

/// Whether some keyboard layout needs Shift, or AltGr, to type this key.
fn shift_can_be_part_of(key: Key) -> bool {
    matches!(
        key,
        Key::Colon
            | Key::Comma
            | Key::Backslash
            | Key::Slash
            | Key::Pipe
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
            | Key::Backtick
            | Key::Minus
            | Key::Period
            | Key::Plus
            | Key::Equals
            | Key::Semicolon
            | Key::Quote
            | Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9
    )
}

/// Whether this shortcut means something inside a text field.
///
/// Ctrl+A in the Find box selects the query; it does not select the whole
/// document. Ctrl+Z there undoes what was typed into the box, not the last
/// edit to the file -- which was the more dangerous half of the same bug,
/// because the document changed while you were looking at a text field.
///
/// Derived from the shortcut rather than flagged per command on purpose. A
/// flag has to be remembered, and the way this class of bug returns is somebody
/// binding a new command to one of these and not thinking about text fields.
/// Deriving it means the binding cannot be added without inheriting the rule.
///
/// The clipboard trio is absent because those bindings are already non-global
/// -- egui synthesises Cut/Copy/Paste events from the platform and the focused
/// widget handles them, which is the same principle arrived at earlier.
fn edits_text(shortcut: KeyboardShortcut) -> bool {
    let command_only = shortcut.modifiers == Modifiers::COMMAND;
    let command_shift = shortcut.modifiers == Modifiers::COMMAND.plus(Modifiers::SHIFT);
    match shortcut.logical_key {
        // Select all, undo.
        Key::A | Key::Z if command_only => true,
        // Redo, in both of its usual spellings.
        Key::Z if command_shift => true,
        Key::Y if command_only => true,
        _ => false,
    }
}

/// How many modifiers a binding demands. More is more specific.
fn specificity(sc: KeyboardShortcut) -> u32 {
    let m = sc.modifiers;
    u32::from(m.shift)
        + u32::from(m.alt)
        + u32::from(m.ctrl)
        + u32::from(m.mac_cmd)
        + u32::from(m.command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Run `triggered` against one key press, as the frame loop would.
    fn press(key: Key, modifiers: Modifiers) -> Option<CommandId> {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        });
        let fired = triggered(&ctx);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        fired
    }

    /// Ctrl+Shift+W is bound to nothing, and used to close the tab because
    /// egui ignored the Shift.
    #[test]
    fn an_extra_modifier_on_a_letter_is_a_different_shortcut() {
        let (close, _) = claimed_bindings()
            .into_iter()
            .find(|(_, id)| *id == CommandId::CloseTab)
            .expect("Close Tab has a shortcut");
        assert_eq!(
            press(close.logical_key, close.modifiers),
            Some(CommandId::CloseTab)
        );

        let with_shift = close.modifiers.plus(Modifiers::SHIFT);
        let claimed = claimed_bindings()
            .into_iter()
            .any(|(sc, _)| sc.logical_key == close.logical_key && sc.modifiers == with_shift);
        if !claimed {
            assert_eq!(press(close.logical_key, with_shift), None);
        }
    }

    /// AltGr arrives as Ctrl+Alt. With a letter it types a character — Polish
    /// `ą` is AltGr+A — and must not run that letter's Ctrl shortcut.
    #[test]
    fn altgr_with_a_letter_runs_nothing() {
        // As egui reports it on Windows and Linux: `command` rides with Ctrl.
        let altgr = Modifiers::COMMAND.plus(Modifiers::ALT);
        let bindings = claimed_bindings();
        let (letter, _) = bindings
            .iter()
            .find(|(sc, _)| {
                sc.modifiers == Modifiers::COMMAND
                    && !shift_can_be_part_of(sc.logical_key)
                    && !bindings.iter().any(|(other, _)| {
                        other.logical_key == sc.logical_key && other.modifiers == altgr
                    })
            })
            .expect("some Ctrl+letter shortcut has no Ctrl+Alt twin");
        assert_eq!(
            press(letter.logical_key, altgr),
            None,
            "{:?}",
            letter.logical_key
        );
    }

    /// On a German keyboard `/` is Shift+7, so Toggle Comment arrives with a
    /// Shift it did not ask for and must still work.
    #[test]
    fn shift_on_a_punctuation_key_is_forgiven() {
        let (comment, _) = claimed_bindings()
            .into_iter()
            .find(|(_, id)| *id == CommandId::ToggleComment)
            .expect("Toggle Comment has a shortcut");
        assert!(shift_can_be_part_of(comment.logical_key));
        assert_eq!(
            press(
                comment.logical_key,
                comment.modifiers.plus(Modifiers::SHIFT)
            ),
            Some(CommandId::ToggleComment)
        );
    }

    /// The bug this ordering exists for: Redo ran Undo.
    ///
    /// egui matches shortcuts logically, so an extra Shift is ignored and
    /// Ctrl+Shift+Z satisfies Ctrl+Z. Whichever is checked first wins, and in
    /// registry order that was Undo.
    #[test]
    fn a_shortcut_with_more_modifiers_is_checked_first() {
        let candidates = claimed_bindings();

        // For every pair sharing a key, the one demanding more modifiers must
        // come first in the order `triggered` walks.
        for (i, (sa, ida)) in candidates.iter().enumerate() {
            for (sb, idb) in &candidates[i + 1..] {
                if sa.logical_key != sb.logical_key {
                    continue;
                }
                assert!(
                    specificity(*sa) >= specificity(*sb),
                    "{idb:?} ({} modifiers) is checked after {ida:?} ({}), so it can never fire",
                    specificity(*sb),
                    specificity(*sa),
                );
            }
        }
    }

    #[test]
    fn the_pairs_that_actually_collide_are_ordered_correctly() {
        // Named explicitly, because these are the ones a user notices.
        let order: Vec<CommandId> = claimed_bindings().into_iter().map(|(_, id)| id).collect();
        let before = |a: CommandId, b: CommandId| {
            let ia = order.iter().position(|id| *id == a);
            let ib = order.iter().position(|id| *id == b);
            match (ia, ib) {
                (Some(ia), Some(ib)) => ia < ib,
                _ => true,
            }
        };
        assert!(before(CommandId::Redo, CommandId::Undo), "Ctrl+Shift+Z");
        assert!(before(CommandId::SaveAs, CommandId::Save), "Ctrl+Shift+S");
        assert!(
            before(CommandId::OpenFolder, CommandId::OpenFile),
            "Ctrl+Shift+O"
        );
        assert!(
            before(CommandId::NewScratch, CommandId::NewFile),
            "Ctrl+Shift+N"
        );
        assert!(
            before(CommandId::FindPrevious, CommandId::FindNext),
            "Shift+F3"
        );
        assert!(before(CommandId::RunStop, CommandId::Run), "Shift+F5");
        assert!(
            before(CommandId::FindUses, CommandId::GoToDefinition),
            "Shift+F12"
        );
        assert!(
            before(CommandId::PreviousUse, CommandId::NextUse),
            "Shift+F8"
        );
    }

    #[test]
    fn every_command_id_is_registered() {
        let registered: HashSet<CommandId> = registry().iter().map(|c| c.id).collect();
        for id in CommandId::ALL {
            assert!(
                registered.contains(id),
                "{id:?} has no registry entry, so it can never appear in a menu or the palette"
            );
        }
        assert_eq!(
            registered.len(),
            CommandId::ALL.len(),
            "CommandId::ALL and the registry have drifted apart"
        );
    }

    #[test]
    fn no_command_is_registered_twice() {
        let mut seen = HashSet::new();
        for cmd in registry() {
            assert!(seen.insert(cmd.id), "{:?} is registered twice", cmd.id);
        }
    }

    #[test]
    fn no_two_commands_share_a_shortcut() {
        let mut seen: Vec<(KeyboardShortcut, CommandId)> = Vec::new();
        for cmd in registry() {
            // Secondary bindings counted too: they are consumed exactly like
            // primary ones, so a clash between a secondary and somebody else's
            // primary is the same bug and just as invisible.
            for sc in [cmd.shortcut, cmd.secondary].into_iter().flatten() {
                if let Some((_, other)) = seen.iter().find(|(s, _)| *s == sc) {
                    panic!("{:?} and {other:?} both bind the same shortcut", cmd.id);
                }
                seen.push((sc, cmd.id));
            }
        }
    }

    /// The key Zoom In is advertised under has to be one that reaches it.
    ///
    /// `+` is Shift+`=` on the usual layouts, so a Ctrl+Plus binding is really
    /// Ctrl+Shift+= -- and Unfold All binds exactly that and wins on
    /// specificity. Zoom In was listed in the View menu under a key that
    /// unfolds the file, and the Ctrl+= people press instead reached nothing
    /// here at all: egui's own zoom handler took it and moved a zoom factor
    /// this application believes it owns, which is how the two came apart.
    #[test]
    fn zooming_in_is_advertised_under_a_key_that_actually_zooms() {
        let zoom_in = get(CommandId::ZoomIn);
        assert_eq!(
            zoom_in.shortcut,
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Equals)),
            "the listed accelerator must not be the shifted spelling"
        );

        // And the shifted spelling still belongs to Unfold All, which is what
        // makes the above necessary rather than merely tidy.
        let unfold = get(CommandId::UnfoldAll);
        assert_eq!(
            unfold.shortcut,
            Some(KeyboardShortcut::new(
                Modifiers::COMMAND.plus(Modifiers::SHIFT),
                Key::Plus
            )),
        );
        let order: Vec<CommandId> = claimed_bindings()
            .into_iter()
            .filter(|(sc, _)| sc.logical_key == Key::Plus)
            .map(|(_, id)| id)
            .collect();
        assert_eq!(
            order,
            vec![CommandId::UnfoldAll, CommandId::ZoomIn],
            "Ctrl+Shift+Plus must be checked before Ctrl+Plus, or Unfold All can never fire"
        );
    }

    #[test]
    fn palette_labels_are_unique_so_fuzzy_matches_are_unambiguous() {
        let mut seen = HashSet::new();
        for cmd in registry() {
            let label = cmd.palette_label();
            assert!(
                seen.insert(label.clone()),
                "duplicate palette entry {label:?}"
            );
        }
    }

    #[test]
    fn get_returns_the_matching_command() {
        assert_eq!(get(CommandId::Save).title, "Save");
        assert_eq!(get(CommandId::About).category, "Help");
    }
}
