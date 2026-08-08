//! What The Editor remembers between runs.
//!
//! Reopening where you left off is the difference between a tool you return to
//! and one you set up again every morning. Stored as TOML next to the settings,
//! but unlike settings this file is written by the program and not meant to be
//! hand-edited, so a corrupt or stale one is discarded silently rather than
//! reported — losing a session is a mild annoyance; a dialog about it on every
//! launch is worse.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

/// A file that was open, and where the caret was in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub path: PathBuf,
    /// Character offset of the caret.
    pub caret: usize,
}

/// Where the window was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// Smallest window worth restoring. Anything under this is treated as junk
/// left by a crash mid-resize.
const MIN_SIZE: f32 = 320.0;
/// Largest window worth restoring.
const MAX_SIZE: f32 = 32_000.0;
/// Bound on a remembered position. Generous enough for any real multi-monitor
/// arrangement — a monitor to the left of the primary gives genuinely negative
/// coordinates — but not so generous that obvious garbage passes.
const MAX_ORIGIN: f32 = 16_000.0;

impl WindowGeometry {
    /// True if this geometry is not obvious garbage.
    ///
    /// A sanity check only. Whether the window would actually be *visible*
    /// depends on the monitors attached right now, which is not knowable until
    /// the window exists — that is [`Self::is_on_screen`]'s job, applied on the
    /// first frame. Both are needed: this one rejects a corrupt file, that one
    /// rejects a position that was valid until a monitor was unplugged.
    #[must_use]
    pub fn is_plausible(&self) -> bool {
        self.width >= MIN_SIZE
            && self.height >= MIN_SIZE
            && self.width <= MAX_SIZE
            && self.height <= MAX_SIZE
            && self.x.abs() <= MAX_ORIGIN
            && self.y.abs() <= MAX_ORIGIN
            && [self.x, self.y, self.width, self.height]
                .iter()
                .all(|v| v.is_finite())
    }

    /// True if this window would be visible on a desktop of the given size.
    ///
    /// Requires a reasonable strip of the title bar to be reachable, so a
    /// window cannot be restored somewhere it cannot be dragged back from.
    #[must_use]
    pub fn is_on_screen(&self, screen_width: f32, screen_height: f32) -> bool {
        const VISIBLE_MARGIN: f32 = 80.0;
        self.x + self.width > VISIBLE_MARGIN
            && self.y + 30.0 > 0.0
            && self.x < screen_width - VISIBLE_MARGIN
            && self.y < screen_height - 30.0
    }
}

/// Everything restored on the next launch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    /// The folder that was open in the explorer.
    pub folder: Option<PathBuf>,
    pub open_files: Vec<OpenFile>,
    /// Index into `open_files`.
    pub active: Option<usize>,
    pub window: Option<WindowGeometry>,
    pub show_output: bool,
    /// Files opened recently, most recent first, for File -> Open Recent.
    ///
    /// Kept here rather than in `settings.toml` because it is history, not
    /// configuration: it changes constantly, nobody hand-edits it, and losing
    /// it costs nothing.
    pub recent_files: Vec<PathBuf>,
    /// Breakpoints per file, one-based. Kept across runs, because losing them
    /// on restart means placing them all again to resume where you left off.
    pub breakpoints: Vec<(PathBuf, Vec<usize>)>,
}

/// How many entries the recent list keeps.
///
/// Long enough to reach back over a working session, short enough that the
/// menu does not need scrolling.
pub const MAX_RECENT: usize = 15;

impl Session {
    /// Read a session file. A missing, unreadable or malformed file yields an
    /// empty session — see the module note on why this is silent.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            tracing::warn!(path = %path.display(), "discarding an unreadable session file");
            return Self::default();
        };
        Self::from_doc(&doc)
    }

    fn from_doc(doc: &DocumentMut) -> Self {
        let folder = doc
            .get("folder")
            .and_then(Item::as_str)
            .map(PathBuf::from)
            // A project folder that has since been deleted or unmounted must
            // not leave the explorer pointing at nothing.
            .filter(|p| p.is_dir());

        let open_files: Vec<OpenFile> = doc
            .get("files")
            .and_then(Item::as_array_of_tables)
            .map(|tables| {
                tables
                    .iter()
                    .filter_map(|t| {
                        let path = PathBuf::from(t.get("path")?.as_str()?);
                        // Skip files that have since been deleted or renamed,
                        // rather than opening a tab full of an error.
                        path.is_file().then(|| OpenFile {
                            caret: t
                                .get("caret")
                                .and_then(Item::as_integer)
                                .and_then(|n| usize::try_from(n).ok())
                                .unwrap_or(0),
                            path,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let active = doc
            .get("active")
            .and_then(Item::as_integer)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|i| *i < open_files.len());

        let window = doc.get("window").and_then(Item::as_table).and_then(|t| {
            let number = |key: &str| t.get(key).and_then(Item::as_float).map(|f| f as f32);
            let geometry = WindowGeometry {
                x: number("x")?,
                y: number("y")?,
                width: number("width")?,
                height: number("height")?,
                maximized: t.get("maximized").and_then(Item::as_bool).unwrap_or(false),
            };
            geometry.is_plausible().then_some(geometry)
        });

        Self {
            folder,
            open_files,
            active,
            window,
            show_output: doc
                .get("show_output")
                .and_then(Item::as_bool)
                .unwrap_or(false),
            // Entries whose file has since gone are dropped on load rather than
            // shown and then failing to open.
            breakpoints: doc
                .get("breakpoints")
                .and_then(Item::as_array_of_tables)
                .map(|tables| {
                    tables
                        .iter()
                        .filter_map(|t| {
                            let path = PathBuf::from(t.get("path")?.as_str()?);
                            // A file that has gone takes its breakpoints with
                            // it, as its open tab already does.
                            if !path.is_file() {
                                return None;
                            }
                            let lines: Vec<usize> = t
                                .get("lines")?
                                .as_array()?
                                .iter()
                                .filter_map(|v| usize::try_from(v.as_integer()?).ok())
                                .filter(|l| *l > 0)
                                .collect();
                            (!lines.is_empty()).then_some((path, lines))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            recent_files: doc
                .get("recent_files")
                .and_then(Item::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(PathBuf::from))
                        .filter(|p| p.is_file())
                        .take(MAX_RECENT)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Record that a file was opened.
    ///
    /// Moves it to the front if it is already listed rather than adding a
    /// second entry — a recent list with the same file three times in it is a
    /// log, not a shortcut.
    pub fn push_recent(recent: &mut Vec<PathBuf>, path: &Path) {
        recent.retain(|p| p != path);
        recent.insert(0, path.to_path_buf());
        recent.truncate(MAX_RECENT);
    }

    /// Write the session out.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_toml())
    }

    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut doc = DocumentMut::new();
        doc.decor_mut()
            .set_prefix("# Written by The Editor. Safe to delete; it will be recreated.\n\n");

        if let Some(folder) = &self.folder {
            doc["folder"] = value(folder.display().to_string());
        }
        if let Some(active) = self.active {
            doc["active"] = value(active as i64);
        }
        doc["show_output"] = value(self.show_output);

        if let Some(w) = self.window {
            let mut table = Table::new();
            table["x"] = value(f64::from(w.x));
            table["y"] = value(f64::from(w.y));
            table["width"] = value(f64::from(w.width));
            table["height"] = value(f64::from(w.height));
            table["maximized"] = value(w.maximized);
            doc["window"] = Item::Table(table);
        }

        let mut files = toml_edit::ArrayOfTables::new();
        for file in &self.open_files {
            let mut table = Table::new();
            table["path"] = value(file.path.display().to_string());
            table["caret"] = value(file.caret as i64);
            files.push(table);
        }
        if !files.is_empty() {
            doc["files"] = Item::ArrayOfTables(files);
        }

        let mut breakpoints = toml_edit::ArrayOfTables::new();
        for (path, lines) in &self.breakpoints {
            if lines.is_empty() {
                continue;
            }
            let mut table = Table::new();
            table["path"] = value(path.display().to_string());
            let mut array = toml_edit::Array::new();
            for line in lines {
                array.push(*line as i64);
            }
            table["lines"] = value(array);
            breakpoints.push(table);
        }
        if !breakpoints.is_empty() {
            doc["breakpoints"] = Item::ArrayOfTables(breakpoints);
        }

        if !self.recent_files.is_empty() {
            let mut recent = toml_edit::Array::new();
            for path in self.recent_files.iter().take(MAX_RECENT) {
                recent.push(path.display().to_string());
            }
            doc["recent_files"] = value(recent);
        }

        doc.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> WindowGeometry {
        WindowGeometry {
            x: 100.0,
            y: 200.0,
            width: 1280.0,
            height: 800.0,
            maximized: false,
        }
    }

    /// Round-trip through TOML, which is what `save`/`load` do.
    fn round_trip(session: &Session) -> Session {
        let text = session.to_toml();
        let doc = text.parse::<DocumentMut>().expect("valid TOML");
        Session::from_doc(&doc)
    }

    #[test]
    fn a_window_position_round_trips() {
        let session = Session {
            window: Some(geometry()),
            ..Session::default()
        };
        assert_eq!(round_trip(&session).window, Some(geometry()));
    }

    #[test]
    fn the_output_panel_state_round_trips() {
        let session = Session {
            show_output: true,
            ..Session::default()
        };
        assert!(round_trip(&session).show_output);
    }

    #[test]
    fn open_files_round_trip_with_their_caret_positions() {
        // Uses real files, because loading deliberately drops ones that have
        // since been deleted.
        let dir = std::env::temp_dir().join("the-editor-session-files");
        std::fs::create_dir_all(&dir).expect("create dir");
        let a = dir.join("a.py");
        let b = dir.join("b.rs");
        std::fs::write(&a, b"x").expect("write a");
        std::fs::write(&b, b"y").expect("write b");

        let session = Session {
            open_files: vec![
                OpenFile {
                    path: a.clone(),
                    caret: 42,
                },
                OpenFile {
                    path: b.clone(),
                    caret: 0,
                },
            ],
            active: Some(1),
            ..Session::default()
        };

        let restored = round_trip(&session);
        assert_eq!(restored.open_files.len(), 2);
        assert_eq!(restored.open_files[0].caret, 42);
        assert_eq!(restored.active, Some(1));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_that_no_longer_exist_are_dropped() {
        let dir = std::env::temp_dir().join("the-editor-session-missing");
        std::fs::create_dir_all(&dir).expect("create dir");
        let real = dir.join("real.py");
        std::fs::write(&real, b"x").expect("write");

        let session = Session {
            open_files: vec![
                OpenFile {
                    path: real.clone(),
                    caret: 0,
                },
                OpenFile {
                    path: dir.join("deleted.py"),
                    caret: 0,
                },
            ],
            active: Some(1),
            ..Session::default()
        };

        let restored = round_trip(&session);
        assert_eq!(
            restored.open_files.len(),
            1,
            "a deleted file must not reopen as an error tab"
        );
        assert_eq!(
            restored.active, None,
            "an active index past the end must be discarded, not restored out of range"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_that_no_longer_exists_is_dropped() {
        let session = Session {
            folder: Some(PathBuf::from("/nonexistent/project")),
            ..Session::default()
        };
        assert_eq!(
            round_trip(&session).folder,
            None,
            "an unmounted or deleted project must not leave the explorer broken"
        );
    }

    #[test]
    fn an_absent_session_file_yields_an_empty_session() {
        let loaded = Session::load(Path::new("/nonexistent/session.toml"));
        assert_eq!(loaded, Session::default());
    }

    #[test]
    fn a_corrupt_session_file_is_discarded_rather_than_failing() {
        let dir = std::env::temp_dir().join("the-editor-session-corrupt");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("session.toml");
        std::fs::write(&path, b"this is not [ valid toml").expect("write");

        assert_eq!(
            Session::load(&path),
            Session::default(),
            "a corrupt session must not stop the editor starting"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_session_survives_a_real_save_and_load() {
        let dir = std::env::temp_dir().join("the-editor-session-io");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("session.toml");

        let session = Session {
            folder: Some(dir.clone()),
            window: Some(geometry()),
            show_output: true,
            ..Session::default()
        };
        session.save(&path).expect("save");

        let loaded = Session::load(&path);
        assert_eq!(loaded.folder, Some(dir.clone()));
        assert_eq!(loaded.window, Some(geometry()));
        assert!(loaded.show_output);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn implausible_window_geometry_is_rejected() {
        let absurd = WindowGeometry {
            x: -30_000.0,
            ..geometry()
        };
        assert!(!absurd.is_plausible());

        // ...but a monitor genuinely to the left of the primary gives negative
        // coordinates, and those must survive.
        let second_monitor = WindowGeometry {
            x: -1920.0,
            ..geometry()
        };
        assert!(
            second_monitor.is_plausible(),
            "a window on a left-hand monitor must be restorable"
        );

        let too_small = WindowGeometry {
            width: 4.0,
            height: 4.0,
            ..geometry()
        };
        assert!(!too_small.is_plausible());

        let nonsense = WindowGeometry {
            x: f32::NAN,
            ..geometry()
        };
        assert!(!nonsense.is_plausible());

        assert!(geometry().is_plausible());
    }

    #[test]
    fn implausible_geometry_is_dropped_on_load() {
        let session = Session {
            window: Some(WindowGeometry {
                x: -30_000.0,
                ..geometry()
            }),
            ..Session::default()
        };
        assert_eq!(
            round_trip(&session).window,
            None,
            "restoring this would open the window where it cannot be seen"
        );
    }

    #[test]
    fn a_window_is_recognised_as_on_or_off_the_desktop() {
        let screen = (1920.0, 1080.0);

        assert!(geometry().is_on_screen(screen.0, screen.1));

        // Just off the right-hand edge, with only a sliver showing.
        let mostly_off = WindowGeometry {
            x: 1900.0,
            ..geometry()
        };
        assert!(!mostly_off.is_on_screen(screen.0, screen.1));

        // Above the top of the desktop, so the title bar cannot be grabbed.
        let above = WindowGeometry {
            y: -200.0,
            ..geometry()
        };
        assert!(!above.is_on_screen(screen.0, screen.1));

        // Partly off the left is fine, as long as enough remains to grab.
        let partly_left = WindowGeometry {
            x: -200.0,
            ..geometry()
        };
        assert!(partly_left.is_on_screen(screen.0, screen.1));
    }

    #[test]
    fn opening_a_file_again_moves_it_to_the_front_rather_than_duplicating_it() {
        let mut recent = Vec::new();
        Session::push_recent(&mut recent, Path::new("/a.py"));
        Session::push_recent(&mut recent, Path::new("/b.py"));
        Session::push_recent(&mut recent, Path::new("/a.py"));
        assert_eq!(
            recent,
            [PathBuf::from("/a.py"), PathBuf::from("/b.py")],
            "the same file must not appear twice"
        );
    }

    #[test]
    fn the_recent_list_is_capped() {
        let mut recent = Vec::new();
        for i in 0..(MAX_RECENT + 10) {
            Session::push_recent(&mut recent, &PathBuf::from(format!("/f{i}.py")));
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(
            recent[0],
            PathBuf::from(format!("/f{}.py", MAX_RECENT + 9)),
            "the newest is first"
        );
    }

    #[test]
    fn recent_files_survive_a_round_trip_through_toml() {
        // Asserting only that the output parses is not enough. A bare key
        // written after a `[window]` header parses perfectly well and belongs
        // to that table, so the list would be silently lost on the way back in.
        // This reads the value back at the top level, where it has to be.
        // The test binary itself: a path that certainly exists and is
        // absolute, so the `is_file` filter on load does not drop it the way it
        // would drop `file!()`, which is relative to the workspace root.
        let this_file = std::env::current_exe().expect("current exe");
        let session = Session {
            recent_files: vec![this_file.clone()],
            window: Some(geometry()),
            ..Session::default()
        };

        let text = session.to_toml();
        let doc: DocumentMut = text.parse().expect("valid TOML");
        assert!(
            doc.get("recent_files").and_then(Item::as_array).is_some(),
            "recent_files is not a top-level array:
{text}"
        );

        let back = Session::from_doc(&doc);
        assert_eq!(
            back.recent_files,
            vec![this_file],
            "the list did not survive the round trip"
        );
        assert!(back.window.is_some(), "the window table survived too");
    }

    #[test]
    fn recent_entries_whose_file_has_gone_are_dropped_on_load() {
        // A menu that offers a file which then fails to open is worse than a
        // shorter menu. The existing `files` list already works this way.
        let doc: DocumentMut = "recent_files = [\"/definitely/not/here.py\"]"
            .parse()
            .expect("parse");
        let session = Session::from_doc(&doc);
        assert!(session.recent_files.is_empty());
    }

    #[test]
    fn an_empty_session_writes_valid_toml() {
        let text = Session::default().to_toml();
        assert!(text.parse::<DocumentMut>().is_ok(), "got {text:?}");
        assert!(text.contains("Written by The Editor"));
    }
}
