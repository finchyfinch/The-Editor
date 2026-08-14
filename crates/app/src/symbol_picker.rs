//! Go to Symbol (Ctrl+Shift+O).
//!
//! The same fuzzy matcher as Go to File, over the declarations in the file
//! being edited. Navigating a long file by scrolling is the thing this
//! replaces, and it is what people reach for before they reach for search:
//! searching finds every mention of a name, and what you wanted was the one
//! place it is defined.
//!
//! Built from the parse tree rather than from `documentSymbol`, so it works
//! with no language server running — the same choice made for Go to Definition
//! and Find Uses, and for the same reason.

use editor_syntax::symbols::{Outline, SymbolKind};
use eframe::egui;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// How many rows to draw. The rest are reachable by typing more.
const VISIBLE: usize = 200;

pub(crate) struct SymbolPicker {
    open: bool,
    query: String,
    selected: usize,
    matcher: Matcher,
    symbols: Vec<Outline>,
    results: Vec<usize>,
    just_opened: bool,
}

impl std::fmt::Debug for SymbolPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolPicker")
            .field("open", &self.open)
            .field("symbols", &self.symbols.len())
            .finish()
    }
}

impl Default for SymbolPicker {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            symbols: Vec::new(),
            results: Vec::new(),
            just_opened: false,
        }
    }
}

impl SymbolPicker {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Open over the declarations found in the active file.
    pub(crate) fn open(&mut self, symbols: Vec<Outline>) {
        self.symbols = symbols;
        self.open = true;
        self.just_opened = true;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.symbols = Vec::new();
        self.results = Vec::new();
    }

    fn refresh(&mut self) {
        self.results = rank(&self.query, &self.symbols, &mut self.matcher);
        self.selected = 0;
    }

    /// Draw the picker. Returns the character offset to jump to.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> Option<std::ops::Range<usize>> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }

        let mut chosen = None;

        egui::Modal::new(egui::Id::new("symbol_picker")).show(ctx, |ui| {
            ui.set_width(520.0);

            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Go to symbol\u{2026}")
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

            if self.symbols.is_empty() {
                ui.weak("Nothing is declared in this file");
            } else if self.results.is_empty() {
                ui.weak("No matching symbols");
            }

            egui::ScrollArea::vertical()
                .id_salt("symbol_picker_results")
                .max_height(380.0)
                .show(ui, |ui| {
                    for (row, index) in self.results.iter().take(VISIBLE).enumerate() {
                        let symbol = &self.symbols[*index];
                        let response = ui.selectable_label(row == self.selected, row_text(symbol));
                        if response.clicked() {
                            chosen = Some(symbol.range.clone());
                        }
                    }
                    if self.results.len() > VISIBLE {
                        ui.weak(format!("{} more\u{2026}", self.results.len() - VISIBLE));
                    }
                });

            if enter && let Some(index) = self.results.get(self.selected) {
                chosen = Some(self.symbols[*index].range.clone());
            }
        });

        if chosen.is_some() {
            self.close();
        }
        chosen
    }
}

/// One row: a glyph for the kind, the nesting, and the name.
fn row_text(symbol: &Outline) -> String {
    // Two spaces per level. Enough to read the structure, little enough that a
    // deeply nested name does not run off the row.
    let indent = "  ".repeat(symbol.depth);
    format!("{}{} {}", indent, glyph(symbol.kind), symbol.name)
}

/// A one-character mark for the kind.
///
/// Letters rather than symbols: the bundled font has no guaranteed coverage of
/// the pictographic ranges, and a missing-glyph box conveys nothing at all.
fn glyph(kind: SymbolKind) -> char {
    match kind {
        SymbolKind::Function => 'f',
        SymbolKind::Class => 'C',
        SymbolKind::Module => 'm',
        SymbolKind::Binding | SymbolKind::Unknown => '\u{00b7}',
    }
}

/// Order the symbols by how well they match.
///
/// An empty query keeps the file's own order, which is the outline — and an
/// outline is what the picker is before you type anything.
fn rank(query: &str, symbols: &[Outline], matcher: &mut Matcher) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..symbols.len()).collect();
    }
    let pattern = Pattern::parse(query.trim(), CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = symbols
        .iter()
        .enumerate()
        .filter_map(|(index, symbol)| {
            let mut buf = Vec::new();
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&symbol.name, &mut buf),
                matcher,
            )?;
            Some((score, index))
        })
        .collect();
    // Best first; ties keep document order, so two equally good matches read
    // top to bottom as they do in the file.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str, kind: SymbolKind, depth: usize) -> Outline {
        Outline {
            name: name.to_owned(),
            kind,
            range: 0..name.len(),
            depth,
        }
    }

    #[test]
    fn an_empty_query_keeps_the_files_own_order() {
        let symbols = vec![
            symbol("Widget", SymbolKind::Class, 0),
            symbol("scaled", SymbolKind::Function, 1),
            symbol("main", SymbolKind::Function, 0),
        ];
        let mut matcher = Matcher::new(Config::DEFAULT);
        assert_eq!(rank("", &symbols, &mut matcher), vec![0, 1, 2]);
    }

    #[test]
    fn a_query_filters_and_orders_by_score() {
        let symbols = vec![
            symbol("unrelated", SymbolKind::Function, 0),
            symbol("scaled", SymbolKind::Function, 0),
            symbol("scale_factor", SymbolKind::Function, 0),
        ];
        let mut matcher = Matcher::new(Config::DEFAULT);
        let got = rank("scale", &symbols, &mut matcher);
        assert!(!got.contains(&0), "the non-match is dropped");
        assert_eq!(got.len(), 2);
    }

    /// Fuzzy, not substring: `sf` should still find `scale_factor`.
    #[test]
    fn matching_is_fuzzy() {
        let symbols = vec![symbol("scale_factor", SymbolKind::Function, 0)];
        let mut matcher = Matcher::new(Config::DEFAULT);
        assert_eq!(rank("sf", &symbols, &mut matcher), vec![0]);
    }

    #[test]
    fn a_row_shows_its_kind_and_nesting() {
        assert_eq!(
            row_text(&symbol("Widget", SymbolKind::Class, 0)),
            "C Widget"
        );
        assert_eq!(
            row_text(&symbol("scaled", SymbolKind::Function, 1)),
            "  f scaled"
        );
    }

    /// Every kind needs a mark the bundled font can actually draw.
    #[test]
    fn every_kind_has_a_glyph() {
        for kind in [
            SymbolKind::Function,
            SymbolKind::Class,
            SymbolKind::Module,
            SymbolKind::Binding,
            SymbolKind::Unknown,
        ] {
            assert!(!glyph(kind).is_whitespace());
        }
    }

    #[test]
    fn a_closed_picker_holds_nothing() {
        let mut picker = SymbolPicker::default();
        picker.open(vec![symbol("a", SymbolKind::Function, 0)]);
        assert!(picker.is_open());
        picker.close();
        assert!(!picker.is_open());
        assert!(picker.symbols.is_empty(), "the listing is dropped with it");
    }
}
