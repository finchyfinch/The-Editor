//! **Throwaway spike.** Validates the single riskiest assumption in PLAN.md:
//! that a custom egui widget can render a large file from a `ropey::Rope` at
//! 60 fps by painting only the visible rows, and that editing stays sub-
//! millisecond.
//!
//! This is not product code and will be deleted after M2. It cuts every corner
//! that does not bear on the question: no syntax highlighting, no selections,
//! no undo, no soft wrap, ASCII-width cursor mapping, no IME.
//!
//! Run it with `cargo spike`. Controls:
//!   * click / arrows / PageUp / PageDown / Home / End — move the caret
//!   * type, Enter, Backspace — edit the rope
//!   * the checkbox disables virtualisation, to measure the contrast
//!
//! What to look for: the "paint" figure in the overlay must stay flat as you
//! scroll and as the line count grows. If it tracks total lines instead of
//! visible rows, virtualisation is broken.

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui;
use ropey::Rope;

const DEFAULT_LINES: usize = 50_000;
const GUTTER_PAD: f32 = 12.0;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("The Editor — rendering spike")
            .with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native(
        "spike",
        options,
        Box::new(|_cc| Ok(Box::<Spike>::default())),
    )
}

struct Spike {
    rope: Rope,
    caret: Caret,
    font_size: f32,
    virtualise: bool,
    /// Wall-clock time spent painting rows, last N frames.
    paint_us: VecDeque<f32>,
    /// Wall-clock time of the last edit applied to the rope.
    last_edit_us: f32,
    /// Rows actually painted last frame.
    painted_rows: usize,
    caret_moved_at: Instant,
}

#[derive(Clone, Copy, Debug, Default)]
struct Caret {
    line: usize,
    /// Column in characters, not bytes. The spike assumes one column per
    /// char, which is wrong for CJK and combining marks — M2 uses real galley
    /// cursor mapping instead.
    col: usize,
}

impl std::fmt::Debug for Spike {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spike")
            .field("lines", &self.rope.len_lines())
            .field("caret", &self.caret)
            .finish()
    }
}

impl Default for Spike {
    fn default() -> Self {
        Self {
            rope: synthetic_source(DEFAULT_LINES),
            caret: Caret::default(),
            font_size: 13.0,
            virtualise: true,
            paint_us: VecDeque::with_capacity(120),
            last_edit_us: 0.0,
            painted_rows: 0,
            caret_moved_at: Instant::now(),
        }
    }
}

/// Something that looks like real source: varied line lengths, indentation,
/// and the occasional very long line, so the layout cost is representative.
fn synthetic_source(lines: usize) -> Rope {
    let mut s = String::with_capacity(lines * 44);
    for i in 0..lines {
        match i % 17 {
            0 => s.push_str(&format!("# ---- section {} ----\n", i / 17)),
            1 => s.push_str(&format!("def function_number_{i}(alpha, beta=None):\n")),
            2 => s.push_str("    \"\"\"Do something moderately interesting.\"\"\"\n"),
            3 | 7 | 11 => s.push_str(&format!("    total = alpha * {i} + len(str(beta))\n")),
            5 => s.push_str(&format!(
                "    payload = {{'index': {i}, 'label': 'a fairly long string literal used to widen this line considerably', 'ok': True}}\n"
            )),
            9 => s.push_str("        if total > 0 and beta is not None:\n"),
            10 => s.push_str("            yield total\n"),
            13 => s.push_str("    return total\n"),
            15 => s.push('\n'),
            _ => s.push_str(&format!("    value_{i} = compute(alpha, beta, {i})\n")),
        }
    }
    Rope::from_str(&s)
}

impl Spike {
    fn line_text(&self, idx: usize) -> String {
        // trim_end handles the trailing newline; a real implementation would
        // slice without allocating, which is one of the things M2 must do.
        self.rope
            .line(idx)
            .as_str()
            .map_or_else(|| self.rope.line(idx).to_string(), ToOwned::to_owned)
            .trim_end_matches(['\n', '\r'])
            .to_owned()
    }

    fn line_len(&self, idx: usize) -> usize {
        self.line_text(idx).chars().count()
    }

    /// Char offset of the caret within the whole rope.
    fn caret_offset(&self) -> usize {
        let line_start = self.rope.line_to_char(self.caret.line);
        line_start + self.caret.col.min(self.line_len(self.caret.line))
    }

    fn insert(&mut self, text: &str) {
        let t = Instant::now();
        let at = self.caret_offset();
        self.rope.insert(at, text);
        if text == "\n" {
            self.caret.line += 1;
            self.caret.col = 0;
        } else {
            self.caret.col += text.chars().count();
        }
        self.last_edit_us = t.elapsed().as_secs_f32() * 1e6;
        self.caret_moved_at = Instant::now();
    }

    fn backspace(&mut self) {
        let at = self.caret_offset();
        if at == 0 {
            return;
        }
        let t = Instant::now();
        self.rope.remove(at - 1..at);
        if self.caret.col == 0 {
            self.caret.line = self.caret.line.saturating_sub(1);
            self.caret.col = self.line_len(self.caret.line);
        } else {
            self.caret.col -= 1;
        }
        self.last_edit_us = t.elapsed().as_secs_f32() * 1e6;
        self.caret_moved_at = Instant::now();
    }

    fn handle_keys(&mut self, ui: &egui::Ui, rows_per_page: usize) {
        let events = ui.input(|i| i.events.clone());
        let last_line = self.rope.len_lines().saturating_sub(1);

        for event in events {
            match event {
                egui::Event::Text(t) => self.insert(&t),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    self.caret_moved_at = Instant::now();
                    match key {
                        egui::Key::Enter => self.insert("\n"),
                        egui::Key::Backspace => self.backspace(),
                        egui::Key::Tab => self.insert("    "),
                        egui::Key::ArrowDown => {
                            self.caret.line = (self.caret.line + 1).min(last_line);
                        }
                        egui::Key::ArrowUp => {
                            self.caret.line = self.caret.line.saturating_sub(1);
                        }
                        egui::Key::ArrowLeft => {
                            if self.caret.col > 0 {
                                self.caret.col -= 1;
                            } else if self.caret.line > 0 {
                                self.caret.line -= 1;
                                self.caret.col = self.line_len(self.caret.line);
                            }
                        }
                        egui::Key::ArrowRight => {
                            if self.caret.col < self.line_len(self.caret.line) {
                                self.caret.col += 1;
                            } else if self.caret.line < last_line {
                                self.caret.line += 1;
                                self.caret.col = 0;
                            }
                        }
                        egui::Key::PageDown => {
                            self.caret.line = (self.caret.line + rows_per_page).min(last_line);
                        }
                        egui::Key::PageUp => {
                            self.caret.line = self.caret.line.saturating_sub(rows_per_page);
                        }
                        egui::Key::Home if modifiers.ctrl => {
                            self.caret.line = 0;
                            self.caret.col = 0;
                        }
                        egui::Key::End if modifiers.ctrl => {
                            self.caret.line = last_line;
                            self.caret.col = 0;
                        }
                        egui::Key::Home => self.caret.col = 0,
                        egui::Key::End => self.caret.col = self.line_len(self.caret.line),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        self.caret.col = self.caret.col.min(self.line_len(self.caret.line));
    }

    fn draw_text_area(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(self.font_size);
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
        let n_lines = self.rope.len_lines();

        let gutter_digits = n_lines.to_string().len();
        let gutter_w = char_w * gutter_digits as f32 + GUTTER_PAD * 2.0;

        // The widget claims the full document size; the ScrollArea around it
        // decides which part of that is on screen.
        let content_w = gutter_w + char_w * 160.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(content_w, row_h * n_lines as f32),
            egui::Sense::click_and_drag(),
        );

        let visible = ui.clip_rect().intersect(rect);
        let rows_per_page = (visible.height() / row_h).floor().max(1.0) as usize;

        if (response.clicked() || response.dragged())
            && let Some(p) = response.interact_pointer_pos()
        {
            let line = (((p.y - rect.top()) / row_h).floor().max(0.0) as usize)
                .min(n_lines.saturating_sub(1));
            let col = (((p.x - rect.left() - gutter_w) / char_w).round().max(0.0)) as usize;
            self.caret = Caret {
                line,
                col: col.min(self.line_len(line)),
            };
            self.caret_moved_at = Instant::now();
            response.request_focus();
        }
        if response.has_focus() || response.clicked() {
            self.handle_keys(ui, rows_per_page);
        }

        // ---- the actual measurement -------------------------------------
        let (first, last) = if self.virtualise {
            // Paint the visible band plus a small margin so a fast scroll
            // never shows a blank row before the next frame lands.
            let first = (((visible.top() - rect.top()) / row_h).floor().max(0.0) as usize)
                .saturating_sub(4);
            let last =
                ((((visible.bottom() - rect.top()) / row_h).ceil() as usize) + 4).min(n_lines);
            (first, last)
        } else {
            (0, n_lines)
        };

        let painter = ui.painter_at(ui.clip_rect());
        let visuals = ui.visuals();
        let text_col = visuals.text_color();
        let dim_col = visuals.weak_text_color();

        let started = Instant::now();

        // Current-line highlight, painted under the text.
        let caret_y = rect.top() + self.caret.line as f32 * row_h;
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), caret_y),
                egui::vec2(rect.width(), row_h),
            ),
            0.0,
            visuals.faint_bg_color,
        );

        for line in first..last {
            let y = rect.top() + line as f32 * row_h;
            let is_caret_line = line == self.caret.line;

            // Gutter: line numbers, right-aligned, dimmed except the caret row.
            painter.text(
                egui::pos2(rect.left() + gutter_w - GUTTER_PAD, y),
                egui::Align2::RIGHT_TOP,
                line + 1,
                font.clone(),
                if is_caret_line { text_col } else { dim_col },
            );

            let text = self.line_text(line);
            if !text.is_empty() {
                let galley = painter.layout_no_wrap(text, font.clone(), text_col);
                painter.galley(egui::pos2(rect.left() + gutter_w, y), galley, text_col);
            }
        }

        let elapsed_us = started.elapsed().as_secs_f32() * 1e6;
        self.painted_rows = last - first;
        if self.paint_us.len() == 120 {
            self.paint_us.pop_front();
        }
        self.paint_us.push_back(elapsed_us);

        // Caret, blinking at ~530 ms, solid for a moment after any movement so
        // it is never invisible right when you look for it.
        let since = self.caret_moved_at.elapsed().as_secs_f32();
        let blink_on = since < 0.5 || ((since * 1000.0 / 530.0) as u32).is_multiple_of(2);
        if blink_on && response.has_focus() {
            let x = rect.left() + gutter_w + self.caret.col as f32 * char_w;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, caret_y), egui::vec2(1.5, row_h)),
                0.0,
                visuals.strong_text_color(),
            );
        }
        // Keep the blink animating without spinning the CPU at full rate.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(120));
    }

    fn overlay(&self, ui: &mut egui::Ui) {
        let n = self.paint_us.len().max(1) as f32;
        let avg = self.paint_us.iter().sum::<f32>() / n;
        let max = self.paint_us.iter().copied().fold(0.0_f32, f32::max);

        egui::Grid::new("stats").num_columns(2).show(ui, |ui| {
            ui.label("Lines");
            ui.label(format!("{}", self.rope.len_lines()));
            ui.end_row();
            ui.label("Rows painted");
            ui.label(format!("{}", self.painted_rows));
            ui.end_row();
            ui.label("Paint (avg)");
            ui.colored_label(
                if avg < 4000.0 {
                    egui::Color32::from_rgb(0x4c, 0xaf, 0x50)
                } else {
                    egui::Color32::from_rgb(0xe5, 0x39, 0x35)
                },
                format!("{:.0} µs", avg),
            );
            ui.end_row();
            ui.label("Paint (worst)");
            ui.label(format!("{max:.0} µs"));
            ui.end_row();
            ui.label("Last edit");
            ui.label(format!("{:.0} µs", self.last_edit_us));
            ui.end_row();
            ui.label("Caret");
            ui.label(format!(
                "Ln {}, Col {}",
                self.caret.line + 1,
                self.caret.col + 1
            ));
            ui.end_row();
        });
    }
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.virtualise, "Virtualise")
                    .on_hover_text("Off = paint every line every frame. Expect it to crawl.");
                ui.separator();
                ui.add(egui::Slider::new(&mut self.font_size, 8.0..=28.0).text("size"));
                ui.separator();
                if ui.button("50k lines").clicked() {
                    self.rope = synthetic_source(50_000);
                    self.caret = Caret::default();
                }
                if ui.button("200k lines").clicked() {
                    self.rope = synthetic_source(200_000);
                    self.caret = Caret::default();
                }
                if ui.button("1M lines").clicked() {
                    self.rope = synthetic_source(1_000_000);
                    self.caret = Caret::default();
                }
            });
        });

        egui::Panel::right("stats_panel")
            .default_size(200.0)
            .show(ui, |ui| {
                ui.heading("Spike");
                ui.separator();
                self.overlay(ui);
                ui.separator();
                ui.small("Budget: paint < 4000 µs, edit < 1000 µs.");
                ui.small("Click in the text to focus, then type.");
            });

        let bg = ui.visuals().extreme_bg_color;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg))
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.draw_text_area(ui));
            });
    }
}
