//! The explorer pane.
//!
//! Directories are read on first expansion, never up front — opening a folder
//! containing a `node_modules` or a `target` must not stall the UI while
//! something walks it. Expansion state is kept separately from the cached
//! listings so a refresh can rebuild the listings without collapsing the tree
//! the user has arranged.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use eframe::egui;

/// Directories never shown. M8 makes this a setting; these are the defaults
/// that stop the tree being useless in a real project.
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "__pycache__",
    "target",
    "node_modules",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

/// What the user did, for the app to act on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    None,
    /// Double-click: open in a permanent tab.
    Open(PathBuf),
    /// Single click: open in the reusable preview tab.
    Preview(PathBuf),
    /// Create a file in this directory.
    NewFileIn(PathBuf),
    /// Create a folder in this directory.
    NewFolderIn(PathBuf),
    /// Begin renaming; the app confirms and performs it.
    Rename { from: PathBuf, to: PathBuf },
    /// Move to the operating system's trash, never an unrecoverable delete.
    Delete(PathBuf),
    /// Show in Explorer/Finder/the file manager.
    Reveal(PathBuf),
    /// Copy the absolute path to the clipboard.
    CopyPath(PathBuf),
    /// Copy the path relative to the project root.
    CopyRelativePath(PathBuf),
    /// Re-read the tree.
    Refresh,
}

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// State of the explorer pane.
#[derive(Debug, Default)]
pub struct FileTree {
    root: Option<PathBuf>,
    /// Directory listings, populated lazily on expand.
    listings: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    show_hidden: bool,
    filter: String,
    /// Set when a listing could not be read, so the pane can say so instead of
    /// silently showing an empty directory.
    errors: HashMap<PathBuf, String>,
    /// The entry being renamed in place, and the name typed so far.
    renaming: Option<(PathBuf, String)>,
    /// Set for one frame after rename starts, to focus the field.
    focus_rename: bool,
}

impl FileTree {
    /// Point the tree at a project folder, discarding any previous state.
    pub fn set_root(&mut self, root: PathBuf) {
        self.listings.clear();
        self.errors.clear();
        self.expanded.clear();
        self.selected = None;
        self.expanded.insert(root.clone());
        self.root = Some(root);
    }

    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Drop cached listings, keeping expansion state. Called after a file is
    /// created or deleted; M1's remaining work replaces this with a watcher.
    pub fn refresh(&mut self) {
        self.listings.clear();
        self.errors.clear();
    }

    /// Start renaming `path` in place, with its current name selected.
    ///
    /// Used after creating a folder, so the placeholder name can be typed over
    /// immediately rather than needing a second right-click.
    pub fn begin_rename(&mut self, path: &Path) {
        self.renaming = Some((
            path.to_path_buf(),
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        ));
        self.focus_rename = true;
        // Make sure the new entry is visible to be renamed.
        if let Some(parent) = path.parent() {
            self.expanded.insert(parent.to_path_buf());
        }
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.refresh();
        }
    }

    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Draw the pane and report what the user did.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Action {
        let Some(root) = self.root.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.weak("No folder open");
                ui.add_space(4.0);
                ui.small("File \u{2192} Open Folder");
            });
            return Action::None;
        };

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.separator();

        let mut action = Action::None;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let label = root.file_name().map_or_else(
                    || root.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.dir_ui(ui, &root, &label, 0, &mut action);
            });
        action
    }

    fn dir_ui(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        label: &str,
        depth: usize,
        action: &mut Action,
    ) {
        if self.renaming.as_ref().is_some_and(|(p, _)| p == path) {
            let renamed = self.rename_row(ui, depth, path);
            if renamed != Action::None {
                *action = renamed;
            }
            return;
        }

        let expanded = self.expanded.contains(path);
        let arrow = if expanded { "\u{25be}" } else { "\u{25b8}" }; // ▾ ▸
        let response = self.row(ui, depth, &format!("{arrow} \u{1f4c1} {label}"), path);

        {
            let path = path.to_path_buf();
            let mut menu_action = Action::None;
            response.context_menu(|ui| self.context_menu(ui, &path, true, &mut menu_action));
            if menu_action != Action::None {
                *action = menu_action;
            }
        }

        if response.clicked() {
            if expanded {
                self.expanded.remove(path);
            } else {
                self.expanded.insert(path.to_path_buf());
                self.ensure_listing(path);
            }
            self.selected = Some(path.to_path_buf());
        }

        if !self.expanded.contains(path) {
            return;
        }
        self.ensure_listing(path);

        if let Some(err) = self.errors.get(path) {
            ui.indent(path, |ui| {
                ui.colored_label(ui.visuals().error_fg_color, format!("\u{26a0} {err}"));
            });
            return;
        }

        let children = self.listings.get(path).cloned().unwrap_or_default();
        for child in children {
            if child.is_dir {
                self.dir_ui(ui, &child.path, &child.name, depth + 1, action);
            } else {
                if !self.matches_filter(&child.name) {
                    continue;
                }
                if self
                    .renaming
                    .as_ref()
                    .is_some_and(|(p, _)| *p == child.path)
                {
                    let renamed = self.rename_row(ui, depth + 1, &child.path);
                    if renamed != Action::None {
                        *action = renamed;
                    }
                    continue;
                }
                let response = self.row(
                    ui,
                    depth + 1,
                    &format!("\u{1f4c4} {}", child.name),
                    &child.path,
                );

                {
                    let path = child.path.clone();
                    let mut menu_action = Action::None;
                    response
                        .context_menu(|ui| self.context_menu(ui, &path, false, &mut menu_action));
                    if menu_action != Action::None {
                        *action = menu_action;
                    }
                }

                if response.double_clicked() {
                    *action = Action::Open(child.path.clone());
                    self.selected = Some(child.path.clone());
                } else if response.clicked() {
                    *action = Action::Preview(child.path.clone());
                    self.selected = Some(child.path.clone());
                }
            }
        }
    }

    fn row(&self, ui: &mut egui::Ui, depth: usize, text: &str, path: &Path) -> egui::Response {
        let indent = 12.0 * depth as f32;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            let selected = self.selected.as_deref() == Some(path);
            ui.selectable_label(selected, text)
        })
        .inner
    }

    /// Draw the in-place rename field for a row. Returns an action once the
    /// user commits or abandons it.
    fn rename_row(&mut self, ui: &mut egui::Ui, depth: usize, path: &Path) -> Action {
        let mut action = Action::None;
        let mut finished = false;

        ui.horizontal(|ui| {
            ui.add_space(12.0 * depth as f32);
            let Some((_, name)) = &mut self.renaming else {
                return;
            };
            let field = ui.add(
                egui::TextEdit::singleline(name)
                    .desired_width(180.0)
                    .hint_text("name"),
            );
            if std::mem::take(&mut self.focus_rename) {
                field.request_focus();
            }

            let commit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let cancel =
                ui.input(|i| i.key_pressed(egui::Key::Escape)) || (field.lost_focus() && !commit);

            if commit {
                let trimmed = name.trim();
                // An unchanged or empty name is a cancel, not a rename: asking
                // the user to confirm a no-op would be noise.
                if !trimmed.is_empty()
                    && path.file_name().is_some_and(|n| n != trimmed)
                    && let Some(parent) = path.parent()
                {
                    action = Action::Rename {
                        from: path.to_path_buf(),
                        to: parent.join(trimmed),
                    };
                }
                finished = true;
            } else if cancel {
                finished = true;
            }
        });

        if finished {
            self.renaming = None;
        }
        action
    }

    /// The context menu shared by files and directories.
    fn context_menu(&mut self, ui: &mut egui::Ui, path: &Path, is_dir: bool, action: &mut Action) {
        // "New" targets the directory itself, or the containing directory for
        // a file — which is what people mean by right-clicking a file and
        // asking for a new one beside it.
        let container = if is_dir {
            path.to_path_buf()
        } else {
            path.parent().map_or_else(PathBuf::new, Path::to_path_buf)
        };

        if ui.button("New File\u{2026}").clicked() {
            *action = Action::NewFileIn(container.clone());
            ui.close();
        }
        if ui.button("New Folder\u{2026}").clicked() {
            *action = Action::NewFolderIn(container);
            ui.close();
        }
        ui.separator();

        if ui.button("Rename").clicked() {
            self.renaming = Some((
                path.to_path_buf(),
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            ));
            self.focus_rename = true;
            ui.close();
        }
        if ui
            .button("Delete")
            .on_hover_text("Moves to the recycle bin, not a permanent delete")
            .clicked()
        {
            *action = Action::Delete(path.to_path_buf());
            ui.close();
        }
        ui.separator();

        if ui.button("Copy Path").clicked() {
            *action = Action::CopyPath(path.to_path_buf());
            ui.close();
        }
        if ui.button("Copy Relative Path").clicked() {
            *action = Action::CopyRelativePath(path.to_path_buf());
            ui.close();
        }
        if ui.button(reveal_label()).clicked() {
            *action = Action::Reveal(path.to_path_buf());
            ui.close();
        }
        ui.separator();
        if ui.button("Refresh").clicked() {
            *action = Action::Refresh;
            ui.close();
        }
    }

    fn matches_filter(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(&self.filter.to_lowercase())
    }

    fn ensure_listing(&mut self, dir: &Path) {
        if self.listings.contains_key(dir) || self.errors.contains_key(dir) {
            return;
        }
        match read_dir_sorted(dir, self.show_hidden) {
            Ok(entries) => {
                self.listings.insert(dir.to_path_buf(), entries);
            }
            Err(e) => {
                self.errors.insert(dir.to_path_buf(), e);
            }
        }
    }
}

/// Directories first, then files, each alphabetically and case-insensitively —
/// the ordering every file manager uses, because it is the one people can scan.
fn read_dir_sorted(dir: &Path, show_hidden: bool) -> Result<Vec<Entry>, String> {
    let iter = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut entries: Vec<Entry> = iter
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);

            if !show_hidden && is_hidden(&name) {
                return None;
            }
            if is_dir && DEFAULT_EXCLUDES.contains(&name.as_str()) {
                return None;
            }
            Some(Entry { path, name, is_dir })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// The platform's name for "show this in the file manager".
fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "Reveal in Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Open Containing Folder"
    }
}

/// Move a path to the operating system's trash.
///
/// Never an unrecoverable delete: a mis-clicked Delete in a file tree should
/// cost a trip to the recycle bin, not a lost afternoon.
///
/// # Errors
/// If the platform refuses — a file on a volume with no trash, or one that is
/// in use.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            is_dir,
        }
    }

    #[test]
    fn sorting_puts_directories_first_then_case_insensitive_alphabetical() {
        let mut v = [
            entry("zebra.py", false),
            entry("Apple.py", false),
            entry("src", true),
            entry("Docs", true),
            entry("beta.py", false),
        ];
        v.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Docs", "src", "Apple.py", "beta.py", "zebra.py"]);
    }

    #[test]
    fn noise_directories_are_excluded_by_default() {
        for name in ["target", "node_modules", "__pycache__", ".venv"] {
            assert!(
                DEFAULT_EXCLUDES.contains(&name),
                "{name} should be hidden by default"
            );
        }
        assert!(!DEFAULT_EXCLUDES.contains(&"src"));
    }

    #[test]
    fn setting_a_root_expands_it_and_clears_previous_state() {
        let mut tree = FileTree::default();
        tree.set_root(PathBuf::from("/a"));
        tree.expanded.insert(PathBuf::from("/a/sub"));
        tree.set_root(PathBuf::from("/b"));

        assert_eq!(tree.root(), Some(Path::new("/b")));
        assert!(tree.expanded.contains(Path::new("/b")));
        assert!(
            !tree.expanded.contains(Path::new("/a/sub")),
            "state from the previous project must not leak into the new one"
        );
    }

    #[test]
    fn refresh_drops_listings_but_keeps_the_tree_arrangement() {
        let mut tree = FileTree::default();
        tree.set_root(PathBuf::from("/a"));
        tree.expanded.insert(PathBuf::from("/a/sub"));
        tree.listings.insert(PathBuf::from("/a"), vec![]);

        tree.refresh();

        assert!(tree.listings.is_empty(), "listings are re-read");
        assert!(
            tree.expanded.contains(Path::new("/a/sub")),
            "the user's expansion state must survive a refresh"
        );
    }

    #[test]
    fn hidden_files_are_recognised() {
        assert!(is_hidden(".gitignore"));
        assert!(!is_hidden("main.py"));
    }
}
