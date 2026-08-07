//! Go to File (Ctrl+P).
//!
//! The same fuzzy matcher as the command palette, over the project's files
//! rather than its commands. Matching runs against the whole relative path, so
//! `mdl/utl` finds `models/utils.py`, but the score is biased towards the file
//! name: someone typing `utils` wants `utils.py`, not the four files that
//! happen to live in a directory called `utils`.
//!
//! The listing is taken once when the picker opens rather than kept in step
//! with the filesystem. A walk of a source tree is a few milliseconds, the
//! result is stale only for as long as the picker is on screen, and the
//! alternative is a second watcher whose only job is to keep a list nobody is
//! looking at up to date.

use std::path::PathBuf;

use eframe::egui;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// How many rows to draw. The rest are reachable by typing more.
const VISIBLE: usize = 200;

/// Weight added to a match on the file name alone.
///
/// Enough to lift a name match above a path match of similar quality, not so
/// much that a good path match is unreachable.
const NAME_BONUS: u32 = 40;

pub(crate) struct FilePicker {
    open: bool,
    query: String,
    selected: usize,
    matcher: Matcher,
    /// Every file under the project root, taken when the picker opened.
    files: Vec<PathBuf>,
    /// True when the walk hit its cap, so the picker can say the list is
    /// incomplete rather than quietly missing what the user is looking for.
    truncated: bool,
    results: Vec<usize>,
    just_opened: bool,
}

impl std::fmt::Debug for FilePicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePicker")
            .field("open", &self.open)
            .field("files", &self.files.len())
            .field("results", &self.results.len())
            .finish()
    }
}

impl Default for FilePicker {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            files: Vec::new(),
            truncated: false,
            results: Vec::new(),
            just_opened: false,
        }
    }
}

impl FilePicker {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Open over a freshly taken listing.
    pub(crate) fn open(&mut self, listing: editor_search::files::Listing) {
        self.files = listing.files;
        self.truncated = listing.truncated;
        self.open = true;
        self.just_opened = true;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        // The listing is dropped: it is only ever used while the picker is on
        // screen, and holding twenty thousand paths for the rest of the session
        // to save one walk is a poor trade.
        self.files = Vec::new();
        self.results = Vec::new();
    }

    fn refresh(&mut self) {
        self.results = rank(&self.query, &self.files, &mut self.matcher);
        self.selected = 0;
    }

    /// Draw the picker. Returns the file the user chose, relative to the root.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }

        let mut chosen = None;
        let mut close_requested = false;

        egui::Modal::new(egui::Id::new("file_picker")).show(ctx, |ui| {
            ui.set_width(560.0);

            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Go to file\u{2026}")
                    .desired_width(f32::INFINITY),
            );
            if self.just_opened {
                edit.request_focus();
                self.just_opened = false;
            }
            if edit.changed() {
                self.refresh();
            }

            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if !self.results.is_empty() {
                if down {
                    self.selected = (self.selected + 1) % self.results.len();
                }
                if up {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(self.results.len() - 1);
                }
            }

            ui.separator();

            if self.files.is_empty() {
                ui.weak("No folder is open");
            } else if self.results.is_empty() {
                ui.weak("No matching files");
            }

            egui::ScrollArea::vertical()
                .id_salt("file_picker_results")
                .max_height(380.0)
                .show(ui, |ui| {
                    for (row, index) in self.results.iter().take(VISIBLE).enumerate() {
                        let path = &self.files[*index];
                        let response = ui.selectable_label(row == self.selected, row_text(path));
                        if response.clicked() {
                            chosen = Some(path.clone());
                        }
                    }
                    if self.results.len() > VISIBLE {
                        ui.weak(format!("{} more\u{2026}", self.results.len() - VISIBLE));
                    }
                });

            if self.truncated {
                ui.separator();
                ui.small("This project has more files than the picker will list.");
            }

            if enter && let Some(index) = self.results.get(self.selected) {
                chosen = Some(self.files[*index].clone());
            }
            if chosen.is_some() {
                close_requested = true;
            }
        });

        if close_requested {
            self.close();
        }
        chosen
    }
}

/// `utils.py   models/` — the name first, then where it lives.
fn row_text(path: &std::path::Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => format!(
            "{name}   {}",
            parent.display().to_string().replace('\\', "/")
        ),
        None => name,
    }
}

/// Rank files against a query, returning indices into `files`.
fn rank(query: &str, files: &[PathBuf], matcher: &mut Matcher) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..files.len()).collect();
    }
    let pattern = Pattern::parse(query.trim(), CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, usize)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            // Forward slashes whatever the platform, so a query with `/` in it
            // works the same on Windows as anywhere else.
            let full = path.display().to_string().replace('\\', "/");
            let mut buf = Vec::new();
            let path_score = pattern.score(nucleo_matcher::Utf32Str::new(&full, &mut buf), matcher);

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut name_buf = Vec::new();
            let name_score = pattern
                .score(nucleo_matcher::Utf32Str::new(&name, &mut name_buf), matcher)
                .map(|s| s + NAME_BONUS);

            // Either kind of match will do; the better of the two decides the
            // rank, so `mdl/utl` still finds `models/utils.py` even though the
            // name alone does not match it.
            match (path_score, name_score) {
                (None, None) => None,
                (a, b) => Some((a.unwrap_or(0).max(b.unwrap_or(0)), i)),
            }
        })
        .collect();

    // Best first, ties broken by the listing's own order, which is sorted — so
    // the rows do not reshuffle unpredictably as the query grows.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn ranked(query: &str, paths: &[&str]) -> Vec<String> {
        let files = files(paths);
        let mut matcher = Matcher::new(Config::DEFAULT);
        rank(query, &files, &mut matcher)
            .into_iter()
            .map(|i| files[i].display().to_string())
            .collect()
    }

    #[test]
    fn an_empty_query_lists_everything_in_order() {
        assert_eq!(
            ranked("", &["a.py", "b.py"]),
            ["a.py", "b.py"],
            "the listing's own order is already sorted"
        );
    }

    #[test]
    fn a_name_match_beats_a_directory_match() {
        // Typing `utils` means the file, not the four files inside a directory
        // that happens to be called `utils`.
        let out = ranked("utils", &["utils/helpers.py", "models/utils.py"]);
        assert_eq!(out.first().map(String::as_str), Some("models/utils.py"));
    }

    #[test]
    fn a_subsequence_across_the_path_still_matches() {
        // The reason the whole path is matched as well as the name.
        let out = ranked("mdlutl", &["models/utils.py", "other.py"]);
        assert_eq!(out.first().map(String::as_str), Some("models/utils.py"));
    }

    #[test]
    fn a_query_with_a_slash_works_on_windows_paths_too() {
        // Backslashes are normalised, or a `src/` query matches nothing on
        // Windows and everything elsewhere.
        let out = ranked("src/main", &["src\\main.rs", "docs\\readme.md"]);
        assert_eq!(out.first().map(String::as_str), Some("src\\main.rs"));
    }

    #[test]
    fn nothing_matching_returns_nothing() {
        assert!(ranked("zzzz", &["a.py", "b.py"]).is_empty());
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(ranked("readme", &["README.md"]).len(), 1);
    }

    #[test]
    fn the_order_is_stable_for_equally_good_matches() {
        // Ties fall back to the listing order, so rows do not jump about while
        // the query is being typed.
        let out = ranked("py", &["a.py", "b.py", "c.py"]);
        assert_eq!(out, ["a.py", "b.py", "c.py"]);
    }

    #[test]
    fn a_row_shows_the_name_first_and_then_its_folder() {
        // The name is what is being looked for; the folder only disambiguates.
        assert_eq!(
            row_text(std::path::Path::new("models/utils.py")),
            "utils.py   models"
        );
        assert_eq!(row_text(std::path::Path::new("main.py")), "main.py");
    }

    #[test]
    fn closing_drops_the_listing() {
        // Twenty thousand paths held for the rest of the session, to save one
        // walk that takes milliseconds, is a poor trade.
        let mut picker = FilePicker::default();
        picker.open(editor_search::files::Listing {
            files: files(&["a.py", "b.py"]),
            truncated: false,
        });
        assert!(picker.is_open());
        assert_eq!(picker.files.len(), 2);
        picker.close();
        assert!(!picker.is_open());
        assert!(picker.files.is_empty());
    }

    #[test]
    fn opening_starts_from_a_clean_query() {
        // Reopening with the last query still in it means the first keystroke
        // appends to something invisible.
        let mut picker = FilePicker::default();
        picker.open(editor_search::files::Listing {
            files: files(&["a.py"]),
            truncated: false,
        });
        picker.query = "stale".to_owned();
        picker.close();
        picker.open(editor_search::files::Listing {
            files: files(&["a.py"]),
            truncated: false,
        });
        assert!(picker.query.is_empty());
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn a_truncated_listing_is_remembered_so_it_can_be_admitted() {
        let mut picker = FilePicker::default();
        picker.open(editor_search::files::Listing {
            files: files(&["a.py"]),
            truncated: true,
        });
        assert!(picker.truncated);
    }
}
