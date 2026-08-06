//! The command registry.
//!
//! Every user-facing action is registered here exactly once, with its title,
//! category and keyboard shortcut. The menus, the toolbar, the command palette
//! and the Help â†’ Keyboard Shortcuts page are all generated from this list.
//!
//! The point is not tidiness. It is that a menu item and its keyboard shortcut
//! cannot drift apart into doing different things, and that a new command
//! cannot be added to a menu but forgotten in the palette. See PLAN.md Â§5.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};

/// Every action The Editor can perform.
///
/// Adding a variant without adding it to [`registry`] fails the exhaustiveness
/// test at the bottom of this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CommandId {
    // File
    NewFile,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseTab,
    CloseFolder,
    Exit,
    // Edit
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
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
    OpenSettingsFile,
    // Help
    About,
    KeyboardShortcuts,
    OpenLogFolder,
}

impl CommandId {
    /// Every command, in registry order. Used by the palette and the
    /// exhaustiveness test.
    pub(crate) const ALL: &'static [Self] = &[
        Self::NewFile,
        Self::OpenFile,
        Self::OpenFolder,
        Self::Save,
        Self::SaveAs,
        Self::SaveAll,
        Self::CloseTab,
        Self::CloseFolder,
        Self::Exit,
        Self::Undo,
        Self::Redo,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::SelectAll,
        Self::ToggleExplorer,
        Self::ThemeDark,
        Self::ThemeLight,
        Self::ThemeSystem,
        Self::ToggleHiddenFiles,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ZoomReset,
        Self::CommandPalette,
        Self::OpenSettingsFile,
        Self::About,
        Self::KeyboardShortcuts,
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
    /// `"File: Open Folder"` â€” what the palette matches against and displays.
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

const fn ctrl_shift(key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(
        Modifiers::COMMAND.plus(Modifiers::SHIFT),
        key,
    ))
}

/// The registry.
///
/// `Modifiers::COMMAND` is Ctrl on Windows and Linux and Cmd on macOS, so the
/// platform mapping described in PLAN.md Â§6 comes for free rather than needing
/// a `cfg!` at every call site.
pub(crate) fn registry() -> &'static [Command] {
    &REGISTRY
}

static REGISTRY: [Command; CommandId::ALL.len()] = [
    cmd(CommandId::NewFile, "File", "New File", ctrl(Key::N)),
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
        CommandId::OpenSettingsFile,
        "Tools",
        "Open settings.toml",
        ctrl(Key::Comma),
    ),
    cmd(CommandId::About, "Help", "About The Editor", None),
    cmd(
        CommandId::KeyboardShortcuts,
        "Help",
        "Keyboard Shortcuts",
        None,
    ),
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
    registry().iter().find_map(|cmd| {
        if !cmd.global {
            return None;
        }
        let sc = cmd.shortcut?;
        ctx.input_mut(|i| i.consume_shortcut(&sc)).then_some(cmd.id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
