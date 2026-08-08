//! The debugger's state and its panel.
//!
//! Breakpoints live here rather than in the session, because they outlive it:
//! they are placed before anything is running and are still there when it
//! stops. The session is told about them when it starts and again whenever a
//! file's set changes.
//!
//! Everything below `Breakpoints` is only meaningful while a session exists,
//! and is thrown away with it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use editor_debug::{Frame, Variable};
use eframe::egui;

/// Where breakpoints are kept, per file, in one-based line numbers.
///
/// One-based because that is what the gutter shows, what the protocol uses, and
/// what a user would type. The editor's own line numbers are zero-based, so the
/// conversion happens at the boundary rather than being carried through.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Breakpoints {
    lines: BTreeMap<PathBuf, BTreeSet<usize>>,
}

impl Breakpoints {
    /// Add or remove one. Returns whether it is now set.
    pub(crate) fn toggle(&mut self, path: &Path, line: usize) -> bool {
        let set = self.lines.entry(path.to_path_buf()).or_default();
        let added = set.insert(line);
        if !added {
            set.remove(&line);
        }
        if set.is_empty() {
            self.lines.remove(path);
        }
        added
    }

    #[must_use]
    pub(crate) fn for_file(&self, path: &Path) -> Vec<usize> {
        self.lines
            .get(path)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub(crate) fn files(&self) -> Vec<PathBuf> {
        self.lines.keys().cloned().collect()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Every breakpoint, flattened, for writing to the session file.
    #[must_use]
    pub(crate) fn flatten(&self) -> Vec<(PathBuf, Vec<usize>)> {
        self.lines
            .iter()
            .map(|(path, lines)| (path.clone(), lines.iter().copied().collect()))
            .collect()
    }

    pub(crate) fn set_file(&mut self, path: PathBuf, lines: Vec<usize>) {
        let set: BTreeSet<usize> = lines.into_iter().filter(|l| *l > 0).collect();
        if set.is_empty() {
            self.lines.remove(&path);
        } else {
            self.lines.insert(path, set);
        }
    }

    /// Move breakpoints below an edit, so they stay on the line they were put
    /// on rather than on whatever has since slid into its place.
    pub(crate) fn shift(&mut self, path: &Path, from_line: usize, by: isize) {
        let Some(set) = self.lines.get_mut(path) else {
            return;
        };
        let moved: BTreeSet<usize> = set
            .iter()
            .map(|line| {
                if *line > from_line {
                    line.saturating_add_signed(by).max(1)
                } else {
                    *line
                }
            })
            .collect();
        *set = moved;
    }
}

/// What the panel wants the application to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Action {
    #[default]
    None,
    /// Show this frame's file and line, and read its variables.
    SelectFrame { id: i64, path: PathBuf, line: usize },
}

/// The live half: only meaningful while a session is running.
#[derive(Debug, Default)]
pub(crate) struct DebugView {
    pub(crate) stack: Vec<Frame>,
    pub(crate) variables: Vec<Variable>,
    pub(crate) selected: Option<i64>,
    /// Why it stopped, for the panel's header.
    pub(crate) reason: String,
}

impl DebugView {
    pub(crate) fn clear(&mut self) {
        self.stack.clear();
        self.variables.clear();
        self.selected = None;
        self.reason.clear();
    }

    /// Where execution is stopped: the innermost frame with a file.
    #[must_use]
    pub(crate) fn location(&self) -> Option<(PathBuf, usize)> {
        self.stack
            .iter()
            .find_map(|f| f.path.clone().map(|p| (p, f.line)))
    }

    /// Draw the call stack and the selected frame's variables.
    pub(crate) fn ui(&self, ui: &mut egui::Ui, running: bool, paused: bool) -> Action {
        let mut action = Action::None;

        if !running {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.weak("Not debugging");
                ui.small("F9 sets a breakpoint, Alt+F5 starts");
            });
            return action;
        }
        if !paused {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.spinner();
                ui.weak("Running\u{2026}");
            });
            return action;
        }

        ui.horizontal(|ui| {
            ui.strong("Paused");
            if !self.reason.is_empty() {
                ui.weak(format!("({})", self.reason));
            }
        });
        ui.separator();

        // Stack on the left, variables on the right: the stack is short and
        // the values are wide, so a column each reads better than either above
        // the other.
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_min_width(260.0);
                ui.weak("Call stack");
                egui::ScrollArea::vertical()
                    .id_salt("debug_stack")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for frame in &self.stack {
                            let label = match &frame.path {
                                Some(path) => format!(
                                    "{}  {}:{}",
                                    frame.name,
                                    path.file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                    frame.line
                                ),
                                None => frame.name.clone(),
                            };
                            let chosen = self.selected == Some(frame.id);
                            if ui.selectable_label(chosen, label).clicked()
                                && let Some(path) = frame.path.clone()
                            {
                                action = Action::SelectFrame {
                                    id: frame.id,
                                    path,
                                    line: frame.line,
                                };
                            }
                        }
                    });
            });
            ui.separator();

            ui.vertical(|ui| {
                ui.weak("Variables");
                egui::ScrollArea::vertical()
                    .id_salt("debug_vars")
                    .max_height(220.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.variables.is_empty() {
                            ui.weak("None in this frame");
                        }
                        egui::Grid::new("debug_variables")
                            .num_columns(3)
                            .striped(true)
                            .spacing([16.0, 2.0])
                            .show(ui, |ui| {
                                for v in &self.variables {
                                    ui.monospace(&v.name);
                                    ui.weak(v.kind.clone().unwrap_or_default());
                                    ui.monospace(shorten(&v.value, 90));
                                    ui.end_row();
                                }
                            });
                    });
            });
        });

        action
    }
}

/// A long repr is a wall of text in a grid cell; the first line of it is what
/// anyone reads anyway.
fn shorten(value: &str, max: usize) -> String {
    let first = value.lines().next().unwrap_or(value);
    if first.chars().count() <= max {
        return first.to_owned();
    }
    let head: String = first.chars().take(max).collect();
    format!("{head}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("/p/main.py")
    }

    #[test]
    fn toggling_adds_then_removes() {
        let mut b = Breakpoints::default();
        assert!(b.toggle(&path(), 10), "first press sets it");
        assert_eq!(b.for_file(&path()), [10]);
        assert!(!b.toggle(&path(), 10), "second press clears it");
        assert!(b.for_file(&path()).is_empty());
        assert!(b.is_empty(), "an empty file is not kept in the map");
    }

    #[test]
    fn breakpoints_come_back_in_line_order() {
        // The protocol takes a set per file and the panel shows them in order;
        // insertion order would put them wherever they were clicked.
        let mut b = Breakpoints::default();
        for line in [30, 10, 20] {
            b.toggle(&path(), line);
        }
        assert_eq!(b.for_file(&path()), [10, 20, 30]);
    }

    #[test]
    fn a_file_with_no_breakpoints_is_not_listed() {
        let mut b = Breakpoints::default();
        b.toggle(&path(), 5);
        b.toggle(&path(), 5);
        assert!(b.files().is_empty());
    }

    #[test]
    fn inserting_lines_carries_the_breakpoints_below_it_down() {
        // Otherwise adding an import at the top silently moves every breakpoint
        // in the file onto the wrong statement.
        let mut b = Breakpoints::default();
        for line in [5, 10] {
            b.toggle(&path(), line);
        }
        b.shift(&path(), 2, 3);
        assert_eq!(b.for_file(&path()), [8, 13]);
    }

    #[test]
    fn deleting_lines_carries_them_back_up_and_never_past_the_first_line() {
        let mut b = Breakpoints::default();
        for line in [4, 20] {
            b.toggle(&path(), line);
        }
        b.shift(&path(), 1, -30);
        assert_eq!(b.for_file(&path()), [1], "clamped, and merged into one");
    }

    #[test]
    fn a_breakpoint_above_the_edit_does_not_move() {
        let mut b = Breakpoints::default();
        b.toggle(&path(), 3);
        b.shift(&path(), 10, 5);
        assert_eq!(b.for_file(&path()), [3]);
    }

    #[test]
    fn restoring_from_a_session_drops_impossible_lines() {
        // Line zero cannot exist; a hand-edited session file should not be able
        // to produce a breakpoint that can never bind.
        let mut b = Breakpoints::default();
        b.set_file(path(), vec![0, 4, 9]);
        assert_eq!(b.for_file(&path()), [4, 9]);
    }

    #[test]
    fn flattening_round_trips_through_set_file() {
        let mut b = Breakpoints::default();
        b.toggle(&path(), 7);
        b.toggle(Path::new("/p/other.py"), 2);

        let mut restored = Breakpoints::default();
        for (path, lines) in b.flatten() {
            restored.set_file(path, lines);
        }
        assert_eq!(restored, b);
    }

    #[test]
    fn the_stopped_location_is_the_innermost_frame_with_a_file() {
        // The innermost frame is sometimes inside the interpreter and has no
        // file; jumping to it is impossible, so the next one down is used.
        let view = DebugView {
            stack: vec![
                Frame {
                    id: 1,
                    name: "<lambda>".into(),
                    path: None,
                    line: 0,
                },
                Frame {
                    id: 2,
                    name: "total".into(),
                    path: Some(path()),
                    line: 6,
                },
            ],
            ..DebugView::default()
        };
        assert_eq!(view.location(), Some((path(), 6)));
    }

    #[test]
    fn a_stack_with_no_files_at_all_has_no_location() {
        let view = DebugView {
            stack: vec![Frame {
                id: 1,
                name: "x".into(),
                path: None,
                line: 0,
            }],
            ..DebugView::default()
        };
        assert_eq!(view.location(), None);
    }

    #[test]
    fn a_long_value_is_cut_to_its_first_line() {
        let value = format!("{}\nsecond line", "x".repeat(400));
        let short = shorten(&value, 40);
        assert!(short.chars().count() <= 41, "got {}", short.chars().count());
        assert!(!short.contains('\n'));
    }
}
