//! The command registry.
//!
//! Every user-facing action is registered here exactly once, with its title,
//! category and keyboard shortcut. The menus, the toolbar, the command palette
//! and the Help → Keyboard Shortcuts page are all generated from this list.
//!
//! The point is not tidiness. It is that a menu item and its keyboard shortcut
//! cannot drift apart into doing different things, and that a new command
//! cannot be added to a menu but forgotten in the palette. See PLAN.md §5.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};

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
    ToggleBreakpoint,
    DebugStart,
    DebugStop,
    DebugStepOver,
    DebugStepInto,
    DebugStepOut,
    ShowOutput,
    ShowTerminal,
    ShowProblems,
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
        Self::ToggleBreakpoint,
        Self::DebugStart,
        Self::DebugStop,
        Self::DebugStepOver,
        Self::DebugStepInto,
        Self::DebugStepOut,
        Self::ShowOutput,
        Self::ShowTerminal,
        Self::ShowProblems,
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
    cmd(CommandId::ZoomIn, "View", "Zoom In", ctrl(Key::Plus)),
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
        global: true,
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
    let mut candidates: Vec<&Command> = registry()
        .iter()
        .filter(|cmd| cmd.global && cmd.shortcut.is_some())
        .collect();
    candidates.sort_by_key(|cmd| std::cmp::Reverse(specificity(cmd)));

    candidates.into_iter().find_map(|cmd| {
        let sc = cmd.shortcut?;
        ctx.input_mut(|i| i.consume_shortcut(&sc)).then_some(cmd.id)
    })
}

/// How many modifiers a binding demands. More is more specific.
fn specificity(cmd: &Command) -> u32 {
    let Some(sc) = cmd.shortcut else { return 0 };
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

    /// The bug this ordering exists for: Redo ran Undo.
    ///
    /// egui matches shortcuts logically, so an extra Shift is ignored and
    /// Ctrl+Shift+Z satisfies Ctrl+Z. Whichever is checked first wins, and in
    /// registry order that was Undo.
    #[test]
    fn a_shortcut_with_more_modifiers_is_checked_first() {
        let mut candidates: Vec<&Command> = registry()
            .iter()
            .filter(|cmd| cmd.global && cmd.shortcut.is_some())
            .collect();
        candidates.sort_by_key(|cmd| std::cmp::Reverse(specificity(cmd)));

        // For every pair sharing a key, the one demanding more modifiers must
        // come first in the order `triggered` walks.
        for (i, a) in candidates.iter().enumerate() {
            for b in &candidates[i + 1..] {
                let (Some(sa), Some(sb)) = (a.shortcut, b.shortcut) else {
                    continue;
                };
                if sa.logical_key != sb.logical_key {
                    continue;
                }
                assert!(
                    specificity(a) >= specificity(b),
                    "{:?} ({} modifiers) is checked after {:?} ({}), so it can never fire",
                    b.id,
                    specificity(b),
                    a.id,
                    specificity(a),
                );
            }
        }
    }

    #[test]
    fn the_pairs_that_actually_collide_are_ordered_correctly() {
        // Named explicitly, because these are the ones a user notices.
        let order: Vec<CommandId> = {
            let mut c: Vec<&Command> = registry()
                .iter()
                .filter(|cmd| cmd.global && cmd.shortcut.is_some())
                .collect();
            c.sort_by_key(|cmd| std::cmp::Reverse(specificity(cmd)));
            c.into_iter().map(|cmd| cmd.id).collect()
        };
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
            let Some(sc) = cmd.shortcut else { continue };
            if let Some((_, other)) = seen.iter().find(|(s, _)| *s == sc) {
                panic!("{:?} and {other:?} both bind the same shortcut", cmd.id);
            }
            seen.push((sc, cmd.id));
        }
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
