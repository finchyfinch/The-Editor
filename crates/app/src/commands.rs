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
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseTab,
    CloseFolder,
    Exit,
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
    pub(crate) shortcut: Option<KeyboardShortcut>,
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
    Command {
        id: CommandId::NewFile,
        title: "New File",
        category: "File",
        shortcut: ctrl(Key::N),
    },
    Command {
        id: CommandId::OpenFile,
        title: "Open File",
        category: "File",
        shortcut: ctrl(Key::O),
    },
    Command {
        id: CommandId::OpenFolder,
        title: "Open Folder",
        category: "File",
        shortcut: ctrl_shift(Key::O),
    },
    Command {
        id: CommandId::Save,
        title: "Save",
        category: "File",
        shortcut: ctrl(Key::S),
    },
    Command {
        id: CommandId::SaveAs,
        title: "Save As",
        category: "File",
        shortcut: ctrl_shift(Key::S),
    },
    Command {
        id: CommandId::SaveAll,
        title: "Save All",
        category: "File",
        shortcut: None,
    },
    Command {
        id: CommandId::CloseTab,
        title: "Close Tab",
        category: "File",
        shortcut: ctrl(Key::W),
    },
    Command {
        id: CommandId::CloseFolder,
        title: "Close Folder",
        category: "File",
        shortcut: None,
    },
    Command {
        id: CommandId::Exit,
        title: "Exit",
        category: "File",
        shortcut: None,
    },
    Command {
        id: CommandId::ToggleExplorer,
        title: "Toggle Explorer",
        category: "View",
        shortcut: ctrl(Key::B),
    },
    Command {
        id: CommandId::ThemeDark,
        title: "Theme: Dark",
        category: "View",
        shortcut: None,
    },
    Command {
        id: CommandId::ThemeLight,
        title: "Theme: Light",
        category: "View",
        shortcut: None,
    },
    Command {
        id: CommandId::ThemeSystem,
        title: "Theme: Follow System",
        category: "View",
        shortcut: None,
    },
    Command {
        id: CommandId::ToggleHiddenFiles,
        title: "Toggle Hidden Files",
        category: "View",
        shortcut: None,
    },
    Command {
        id: CommandId::ZoomIn,
        title: "Zoom In",
        category: "View",
        shortcut: ctrl(Key::Plus),
    },
    Command {
        id: CommandId::ZoomOut,
        title: "Zoom Out",
        category: "View",
        shortcut: ctrl(Key::Minus),
    },
    Command {
        id: CommandId::ZoomReset,
        title: "Reset Zoom",
        category: "View",
        shortcut: ctrl(Key::Num0),
    },
    Command {
        id: CommandId::CommandPalette,
        title: "Command Palette",
        category: "Tools",
        shortcut: ctrl_shift(Key::P),
    },
    Command {
        id: CommandId::OpenSettingsFile,
        title: "Open settings.toml",
        category: "Tools",
        shortcut: ctrl(Key::Comma),
    },
    Command {
        id: CommandId::About,
        title: "About The Editor",
        category: "Help",
        shortcut: None,
    },
    Command {
        id: CommandId::KeyboardShortcuts,
        title: "Keyboard Shortcuts",
        category: "Help",
        shortcut: None,
    },
    Command {
        id: CommandId::OpenLogFolder,
        title: "Open Log Folder",
        category: "Help",
        shortcut: None,
    },
];

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
