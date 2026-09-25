//! The gutter: where each of its columns is, and what a pointer over it means.

use super::*;

/// Space to the left of the line numbers, and either side of a fold chevron.
pub(super) const GUTTER_GAP: f32 = 4.0;

/// Where each column of the gutter is, as offsets from its left edge.
///
/// Worked out once per frame and read by the painting and the hit-testing
/// alike. They used to work it out separately and disagreed: a whole
/// row-height was reserved for the fold chevrons, but the chevron was drawn in
/// the twelve pixels of padding to the right of the numbers, so the space
/// reserved for it turned up as a blank band to the *left* of them — and the
/// fold's click zone overlapped the last digit of every line number.
///
/// Left to right: blame (when shown), the change bar, one glyph column shared
/// by breakpoints and the diagnostic marker, the numbers, the fold chevrons.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Gutter {
    pub(super) blame: f32,
    pub(super) glyphs: f32,
    /// The digits and the gap before them; zero with line numbers off.
    pub(super) numbers: f32,
    pub(super) folds: f32,
}

impl Gutter {
    pub(super) fn new(
        blame: f32,
        row_height: f32,
        digit_width: f32,
        line_count: usize,
        show_numbers: bool,
    ) -> Self {
        let digits = line_count.max(1).to_string().len() as f32;
        Self {
            blame,
            glyphs: row_height,
            numbers: if show_numbers {
                GUTTER_GAP + digits * digit_width
            } else {
                0.0
            },
            // One character, with a gap either side so the chevron touches
            // neither the digits nor the code.
            folds: digit_width + 2.0 * GUTTER_GAP,
        }
    }

    pub(super) fn glyphs_left(&self) -> f32 {
        self.blame + CHANGE_COLUMN
    }

    pub(super) fn glyphs_centre(&self) -> f32 {
        self.glyphs_left() + self.glyphs / 2.0
    }

    /// The digits are right-aligned to this.
    pub(super) fn numbers_right(&self) -> f32 {
        self.glyphs_left() + self.glyphs + self.numbers
    }

    pub(super) fn folds_centre(&self) -> f32 {
        self.numbers_right() + self.folds / 2.0
    }

    /// Where the text begins.
    pub(super) fn width(&self) -> f32 {
        self.numbers_right() + self.folds
    }

    pub(super) fn zone(&self, x: f32) -> Zone {
        if x < self.glyphs_left() {
            Zone::Annotation
        } else if x < self.glyphs_left() + self.glyphs {
            Zone::Breakpoints
        } else if x < self.numbers_right() {
            Zone::Numbers
        } else if x < self.width() {
            Zone::Folds
        } else {
            Zone::Text
        }
    }
}

impl EditorView {
    /// Which column `x` falls in.
    ///
    /// The order of these tests is the order the gutter is laid out in, so a
    /// column that has been given no width simply never matches.
    pub(super) fn zone_at(&self, x: f32, rect: egui::Rect) -> Zone {
        self.gutter.zone(x - rect.left())
    }

    /// The document line drawn at height `y`.
    pub(super) fn line_at_pos(&self, y: f32, rect: egui::Rect, row_height: f32) -> usize {
        let row = ((y - rect.top()) / row_height).floor().max(0.0) as usize;
        self.fold_map.line_at(row)
    }

    /// The pointer to show at `pos`.
    ///
    /// An I-beam over the code, because the code is text. A hand over the two
    /// things in the gutter that act on a click — and only where they actually
    /// would: the fold column is mostly empty, and a hand beside a line with no
    /// chevron on it promises something that does not happen. Everywhere else
    /// the ordinary arrow, which is what a column you can only read deserves.
    pub(super) fn cursor_icon(
        &self,
        doc: &Document,
        pos: egui::Pos2,
        rect: egui::Rect,
        row_height: f32,
    ) -> egui::CursorIcon {
        // Before the zones, which describe the text underneath: a pinned row
        // covers the gutter as well as the code, and all of it is one target.
        if self.sticky_at(pos).is_some() {
            return egui::CursorIcon::PointingHand;
        }
        let line = self.line_at_pos(pos.y, rect, row_height);
        match self.zone_at(pos.x, rect) {
            Zone::Text => egui::CursorIcon::Text,
            Zone::Breakpoints if line < doc.line_count() => egui::CursorIcon::PointingHand,
            Zone::Folds if self.folds.iter().any(|f| f.first == line) => {
                egui::CursorIcon::PointingHand
            }
            _ => egui::CursorIcon::Default,
        }
    }
}
