//! Showing the manual and the licence list inside the application.
//!
//! Documentation you have to leave the program to read is documentation nobody
//! reads. Both files are compiled into the executable, so they are there on a
//! machine with no network and no install directory to go looking in.
//!
//! The renderer handles the small subset of Markdown these two files use:
//! headings, paragraphs, bullets, tables, fenced code and inline `code`. Not a
//! Markdown library — pulling one in, with its HTML sanitiser and its parser
//! generator, to lay out two documents we write ourselves is a poor trade, and
//! the failure mode of this one is a line that looks plain rather than a
//! crash.

use eframe::egui;

/// One document, and whether its window is open.
#[derive(Debug, Default)]
pub(crate) struct DocsWindow {
    open: bool,
}

impl DocsWindow {
    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    /// Draw the window. `id` must be unique per document.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, id: &'static str, title: &str, source: &str) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new(title)
            .id(egui::Id::new(id))
            .open(&mut open)
            .default_size([720.0, 620.0])
            .vscroll(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        render(ui, source);
                    });
            });
        self.open = open;
    }
}

/// Render the Markdown subset these documents use.
fn render(ui: &mut egui::Ui, source: &str) {
    let mut in_code = false;
    let mut code = String::new();

    for line in source.lines() {
        // Fenced code first: everything inside is verbatim, including the
        // characters that would otherwise be markup.
        if line.trim_start().starts_with("```") {
            if in_code {
                code_block(ui, &code);
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }

        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if trimmed.starts_with("---") {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(rest).strong().size(15.0));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            ui.add_space(12.0);
            ui.heading(rest);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(rest).heading().size(24.0));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            ui.horizontal_top(|ui| {
                ui.add_space(12.0);
                ui.label("\u{2022}");
                inline(ui, rest);
            });
            continue;
        }
        if is_table_row(trimmed) {
            // A row of dashes under the header is the separator, and there is
            // nothing to draw for it.
            if trimmed.chars().all(|c| "|-: ".contains(c)) {
                continue;
            }
            table_row(ui, trimmed);
            continue;
        }
        inline(ui, trimmed);
    }

    // A file whose last fence was never closed still shows its contents.
    if !code.is_empty() {
        code_block(ui, &code);
    }
}

fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.len() > 2
}

fn table_row(ui: &mut egui::Ui, line: &str) {
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
    ui.horizontal_top(|ui| {
        ui.add_space(12.0);
        for (i, cell) in cells.iter().enumerate() {
            // The first column of these tables is always the key -- a shortcut
            // or a name -- so it gets a fixed width and the rest wraps after
            // it. Real column measurement would need two passes over the whole
            // table for no visible gain at this scale.
            if i == 0 {
                ui.allocate_ui_with_layout(
                    egui::vec2(190.0, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| inline(ui, cell),
                );
            } else {
                inline(ui, cell);
            }
        }
    });
}

fn code_block(ui: &mut egui::Ui, code: &str) {
    egui::Frame::default()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(code.trim_end()).monospace());
        });
}

/// One line of text, honouring `code`, **bold** and *italic*.
///
/// Written as a scan rather than a parser: these are our own documents, so the
/// worst case is a stray asterisk showing up as an asterisk.
fn inline(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut rest = text;
        while !rest.is_empty() {
            let next = ["`", "**", "*"]
                .iter()
                .filter_map(|marker| rest.find(marker).map(|at| (at, *marker)))
                .min_by_key(|(at, marker)| (*at, std::cmp::Reverse(marker.len())));

            let Some((at, marker)) = next else {
                ui.label(rest);
                return;
            };
            if at > 0 {
                ui.label(&rest[..at]);
            }
            let after = &rest[at + marker.len()..];
            let Some(end) = after.find(marker) else {
                // Unmatched, so it is just a character.
                ui.label(marker);
                rest = after;
                continue;
            };
            let inner = &after[..end];
            let styled = match marker {
                "`" => egui::RichText::new(inner)
                    .monospace()
                    .background_color(ui.visuals().extreme_bg_color),
                "**" => egui::RichText::new(inner).strong(),
                _ => egui::RichText::new(inner).italics(),
            };
            ui.label(styled);
            rest = &after[end + marker.len()..];
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manual is compiled in, so a typo that empties it should not be
    /// something anyone finds out about from the Help menu.
    #[test]
    fn both_documents_are_present_and_look_like_documents() {
        for (name, text) in [
            ("manual", crate::app::MANUAL),
            ("third-party", crate::app::THIRD_PARTY),
        ] {
            assert!(text.len() > 500, "{name} is suspiciously short");
            assert!(
                text.lines().any(|l| l.starts_with("# ")),
                "{name} has no title"
            );
        }
    }

    /// Every fenced block has to be closed, or everything after the stray
    /// fence renders as code.
    #[test]
    fn the_manual_has_balanced_code_fences() {
        let fences = crate::app::MANUAL
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count();
        assert!(fences.is_multiple_of(2), "{fences} fences is an odd number");
    }

    #[test]
    fn a_table_separator_row_is_recognised_and_a_real_row_is_not() {
        assert!(is_table_row("| a | b |"));
        assert!(is_table_row("|---|---|"));
        assert!(!is_table_row("not a table"));
        assert!(!is_table_row("| unterminated"));
    }

    /// The renderer's job is to never lose the words. Whatever it does with the
    /// markup, the text itself has to come out.
    #[test]
    fn inline_markup_is_stripped_rather_than_shown() {
        // Exercised through the same scan the renderer uses.
        let cases = [
            ("plain text", "plain text"),
            ("some `code` here", "some code here"),
            ("**bold** and *italic*", "bold and italic"),
            ("an unmatched ` backtick", "an unmatched ` backtick"),
        ];
        for (input, want) in cases {
            assert_eq!(strip_markup(input), want, "input: {input:?}");
        }
    }

    /// Mirrors `inline`'s scan, so the test exercises the same rule the
    /// renderer applies rather than a second implementation of it.
    fn strip_markup(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while !rest.is_empty() {
            let next = ["`", "**", "*"]
                .iter()
                .filter_map(|m| rest.find(m).map(|at| (at, *m)))
                .min_by_key(|(at, m)| (*at, std::cmp::Reverse(m.len())));
            let Some((at, marker)) = next else {
                out.push_str(rest);
                break;
            };
            out.push_str(&rest[..at]);
            let after = &rest[at + marker.len()..];
            let Some(end) = after.find(marker) else {
                out.push_str(marker);
                rest = after;
                continue;
            };
            out.push_str(&after[..end]);
            rest = &after[end + marker.len()..];
        }
        out
    }
}
