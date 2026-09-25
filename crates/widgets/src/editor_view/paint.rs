//! Drawing: the rows on screen and nothing else, the gutter beside them, the
//! sticky band over them, and what a screen reader is told about them.

use super::*;

impl EditorView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        highlighter: Option<&mut Highlighter>,
        syntax: &SyntaxTheme,
        opts: EditorOptions,
        font: &egui::FontId,
        rect: egui::Rect,
        text_left: f32,
        gutter_width: f32,
        row_height: f32,
        visible: egui::Rect,
        response: &egui::Response,
    ) {
        // The bracket around the caret, from the tree the highlighter already
        // maintains. Recomputed per paint rather than cached: the caret moves
        // constantly and the walk is a handful of node lookups.
        self.bracket_pair = highlighter
            .as_ref()
            .and_then(|h| h.tree())
            .and_then(|tree| {
                editor_syntax::brackets::match_at(tree, doc.text(), self.selection.head)
            });

        let line_count = doc.text().len_lines();
        let first = (((visible.top() - rect.top()) / row_height).floor().max(0.0) as usize)
            .saturating_sub(OVERSCAN_ROWS);
        // Capped by the document rather than by the row map. A keystroke is
        // handled earlier in this same frame and applies to `doc` at once,
        // while the map is rebuilt at the top of the *next* one: backspacing a
        // selection that spans lines leaves rows here with no line behind
        // them, and painting one asked the rope for a line past its end.
        let last = ((((visible.bottom() - rect.top()) / row_height).ceil() as usize)
            + OVERSCAN_ROWS)
            .min(self.fold_map.rows_within(line_count));

        // The rows the sticky header will pin, worked out before anything is
        // painted because both the highlighting below and the scroll-to-caret
        // at the end of this function need to know how many there are.
        //
        // From the first row genuinely on screen, not from `first` -- that has
        // the overscan subtracted, and a row above the viewport has not
        // scrolled off it.
        let top_row = ((visible.top() - rect.top()) / row_height).ceil().max(0.0) as usize;
        let sticky = if opts.sticky_scopes {
            self.sticky_lines(self.fold_map.line_at(top_row), line_count)
        } else {
            Vec::new()
        };

        // Highlight exactly the rows about to be painted, and nothing else.
        // This is where "cost tracks the viewport, not the file" is enforced.
        //
        // The byte range runs from the first visible line to the last. With a
        // fold in between that also covers the hidden lines, which costs a
        // little work and keeps the range contiguous -- asking for several
        // disjoint ranges would cost more than the lines are worth.
        let mut highlighter = highlighter;
        let spans = match highlighter.as_mut() {
            Some(h) => {
                let from = doc
                    .text()
                    .line_to_byte(self.fold_map.line_at(first).min(line_count));
                let to = doc
                    .text()
                    .line_to_byte(self.fold_map.line_at(last).min(line_count));
                h.spans(doc.text(), from..to.max(from), syntax)
            }
            None => Vec::new(),
        };

        // One range per pinned row, each a single line, rather than one range
        // running from the outermost declaration down to the viewport. A class
        // can be two thousand lines long, and highlighting all of it to draw
        // four rows is precisely the cost virtualised rendering exists to
        // avoid.
        let sticky_spans: Vec<Vec<editor_syntax::highlight::Span>> = sticky
            .iter()
            .map(|line| match highlighter.as_mut() {
                Some(h) => {
                    let from = doc.text().line_to_byte((*line).min(line_count));
                    let to = doc.text().line_to_byte((line + 1).min(line_count));
                    h.spans(doc.text(), from..to.max(from), syntax)
                }
                None => Vec::new(),
            })
            .collect();

        let painter = ui.painter_at(ui.clip_rect());
        let visuals = ui.visuals().clone();
        let caret_line = doc.line_of(self.selection.head);
        // Every caret's span, and every caret's head, worked out once rather
        // than per painted row. Ordinarily this is a one-element vector.
        let (all_cursors, _) = self.cursors();
        let sel_ranges: Vec<std::ops::Range<usize>> = all_cursors
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.range())
            .collect();
        let caret_heads: Vec<usize> = all_cursors.iter().map(|s| s.head).collect();

        // Current-line stripe, under everything else.
        if self.selection.is_empty() {
            let y = rect.top() + self.fold_map.row_at(caret_line) as f32 * row_height;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(text_left, y),
                    egui::vec2(rect.width() - gutter_width, row_height),
                ),
                0.0,
                visuals.faint_bg_color,
            );
        }

        // The line the debugger is stopped on, above the current-line stripe so
        // it is unmistakable which is which.
        if let Some(line) = self.paused_line
            && line < doc.line_count()
            && !self.fold_map.is_hidden(line)
        {
            let y = rect.top() + self.fold_map.row_at(line) as f32 * row_height;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(text_left, y),
                    egui::vec2(rect.width() - gutter_width, row_height),
                ),
                0.0,
                // A wash rather than a solid fill: the code underneath is the
                // point, and a highlight that hides it defeats itself.
                visuals.selection.bg_fill.gamma_multiply(0.45),
            );
        }

        // Change bars, in the leftmost column. Drawn from the marks the
        // application supplies; nothing here knows about git.
        //
        // Walked rather than indexed because the marks are per *line* and the
        // gutter is per *row*, and with a fold in the way those disagree. A
        // fold hiding changed lines still shows a bar, on the line that is
        // standing in for them — otherwise collapsing a function would quietly
        // hide the fact that you had changed it.
        for (line, status) in &self.changes {
            let row = self.fold_map.row_at(*line);
            if row < first || row >= last {
                continue;
            }
            let y = rect.top() + row as f32 * row_height;
            let x = rect.left() + self.blame_width;
            let colour = change_colour(&visuals, *status);
            let bar = match status {
                // A deletion has no lines of its own, so it gets a short mark
                // at the join rather than a full-height bar. Full height would
                // claim the line below was deleted, which is the opposite of
                // what happened to it.
                LineStatus::DeletedAbove => egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(CHANGE_BAR_WIDTH, (row_height * 0.3).max(2.0)),
                ),
                _ => egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(CHANGE_BAR_WIDTH, row_height),
                ),
            };
            painter.rect_filled(bar, 0.0, colour);
        }

        // Blame, in the leftmost column when it is switched on.
        //
        // Painted per visible row rather than per entry: the annotations cover
        // every line of the file, and walking all of them to draw forty would
        // make scrolling a long file cost more the further down it went.
        if self.blame_width > 0.0 {
            let index: std::collections::HashMap<usize, &String> = self
                .blame
                .iter()
                .map(|(line, text)| (*line, text))
                .collect();
            for row in first..last {
                let line = self.fold_map.line_at(row);
                let Some(text) = index.get(&line) else {
                    continue;
                };
                let y = rect.top() + row as f32 * row_height;
                // Truncated by characters, not bytes: a name with an accent in
                // it must not be cut in half.
                let shown: String = text.chars().take(BLAME_COLUMNS).collect();
                painter.text(
                    egui::pos2(rect.left() + 4.0, y),
                    egui::Align2::LEFT_TOP,
                    shown,
                    font.clone(),
                    visuals.weak_text_color(),
                );
            }
            // A hairline between the annotations and everything else, so the
            // eye has somewhere to stop.
            let x = rect.left() + self.blame_width - 3.0;
            painter.line_segment(
                [
                    egui::pos2(x, visible.top()),
                    egui::pos2(x, visible.bottom()),
                ],
                egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.3)),
            );
        }

        // Breakpoints, in the glyph column beside the change bars.
        for (line, verified) in &self.breakpoints {
            if self.fold_map.is_hidden(*line) {
                continue;
            }
            // By row, not by line: below a fold the two differ, and comparing
            // a line number with the visible rows skipped breakpoints that were
            // on screen.
            let row = self.fold_map.row_at(*line);
            if row < first || row >= last {
                continue;
            }
            let y = rect.top() + row as f32 * row_height + row_height / 2.0;
            let centre = egui::pos2(rect.left() + self.gutter.glyphs_centre(), y);
            let radius = row_height * 0.26;
            let colour = egui::Color32::from_rgb(0xd0, 0x45, 0x45);
            if *verified {
                painter.circle_filled(centre, radius, colour);
            } else {
                // Hollow: placed, but the debugger could not bind it — usually
                // a blank line or a comment. It will never be hit, and must not
                // look like one that will.
                painter.circle_stroke(centre, radius, egui::Stroke::new(1.5, colour));
            }
        }

        let mut caret_rect = None;
        let mut extra_carets: Vec<egui::Rect> = Vec::new();
        let widest_before = self.widest_text;

        for row in first..last {
            let line = self.fold_map.line_at(row);
            let y = rect.top() + row as f32 * row_height;
            let line_start = doc.line_start(line);
            let text = doc.line_text(line);

            let galley = if spans.is_empty() {
                painter.layout_no_wrap(text.clone(), font.clone(), visuals.text_color())
            } else {
                let job = highlighted_line(
                    &text,
                    doc.text().line_to_byte(line),
                    &spans,
                    syntax,
                    font,
                    visuals.text_color(),
                );
                ui.fonts_mut(|f| f.layout_job(job))
            };

            // Search results, painted under the selection so a selected match
            // still reads as selected.
            if !self.search_matches.is_empty() {
                let line_end = line_start + text.chars().count();
                for found in &self.search_matches {
                    if found.end < line_start || found.start > line_end {
                        continue;
                    }
                    let from = found.start.clamp(line_start, line_end) - line_start;
                    let to = found.end.clamp(line_start, line_end) - line_start;
                    if from >= to {
                        continue;
                    }
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    let x1 = text_left + galley.pos_from_cursor(ccursor(to)).left();
                    let is_current = self.current_match.as_ref() == Some(found);
                    let colour = if is_current {
                        visuals.selection.bg_fill
                    } else {
                        visuals.widgets.hovered.bg_fill
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y),
                            egui::pos2(x1.max(x0 + 2.0), y + row_height),
                        ),
                        2,
                        colour,
                    );
                    if is_current {
                        // An outline as well as a fill, so the current match is
                        // findable even where the fill sits under a selection.
                        painter.rect_stroke(
                            egui::Rect::from_min_max(
                                egui::pos2(x0, y),
                                egui::pos2(x1.max(x0 + 2.0), y + row_height),
                            ),
                            2,
                            egui::Stroke::new(1.0, visuals.strong_text_color()),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }

            // Selection highlight for the part of this line each caret covers.
            let line_end = line_start + text.chars().count();
            for sel_range in &sel_ranges {
                if sel_range.end < line_start || sel_range.start > line_end {
                    continue;
                }
                let from = sel_range.start.clamp(line_start, line_end) - line_start;
                let to = sel_range.end.clamp(line_start, line_end) - line_start;
                if from < to || (sel_range.start <= line_end && sel_range.end > line_end) {
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    // A selection spanning the line break highlights to the
                    // end of the row, so the newline is visibly included.
                    let x1 = if sel_range.end > line_end {
                        rect.right()
                    } else {
                        text_left + galley.pos_from_cursor(ccursor(to)).left()
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, y),
                            egui::pos2(x1.max(x0 + 2.0), y + row_height),
                        ),
                        0.0,
                        visuals.selection.bg_fill,
                    );
                }
            }

            // Diagnostic underlines, over the text so they are not hidden by
            // the selection.
            if !self.diagnostics.is_empty() {
                let line_end = line_start + text.chars().count();
                for diagnostic in &self.diagnostics {
                    if diagnostic.range.end < line_start || diagnostic.range.start > line_end {
                        continue;
                    }
                    let from = diagnostic.range.start.clamp(line_start, line_end) - line_start;
                    let to = diagnostic.range.end.clamp(line_start, line_end) - line_start;
                    // A zero-width diagnostic — an error at the end of a line —
                    // still needs something visible, so give it a minimum width.
                    let x0 = text_left + galley.pos_from_cursor(ccursor(from)).left();
                    let x1 = (text_left + galley.pos_from_cursor(ccursor(to)).left()).max(x0 + 6.0);
                    paint_squiggle(
                        &painter,
                        x0,
                        x1,
                        y + row_height - 2.0,
                        severity_colour(&visuals, diagnostic.severity),
                    );
                }
            }

            // A chevron for a line that opens a fold: pointing down when the
            // fold is open, right when it is closed, which is the direction
            // every file manager and outline view has used for decades.
            if self.folds.iter().any(|f| f.first == line) {
                let closed = self.collapsed.contains(&line);
                painter.text(
                    egui::pos2(
                        rect.left() + self.gutter.folds_centre(),
                        y + row_height / 2.0,
                    ),
                    egui::Align2::CENTER_CENTER,
                    if closed {
                        crate::glyphs::FOLD_CLOSED
                    } else {
                        crate::glyphs::FOLD_OPEN
                    },
                    font.clone(),
                    if closed {
                        visuals.strong_text_color()
                    } else {
                        visuals.weak_text_color()
                    },
                );
            }

            // A glyph for the worst diagnostic on this line, so severity is not
            // conveyed by the squiggle's colour alone. It shares a column with
            // breakpoints and gives way to one: the squiggle still marks the
            // problem, and a breakpoint hidden behind a warning sign is one
            // nobody can see they set.
            let has_breakpoint = self.breakpoints.iter().any(|(l, _)| *l == line);
            if !has_breakpoint
                && let Some(worst) = self
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        let line_end = line_start + text.chars().count();
                        d.range.start <= line_end && d.range.end >= line_start
                    })
                    .min_by_key(|d| d.severity)
            {
                painter.text(
                    egui::pos2(
                        rect.left() + self.gutter.glyphs_centre(),
                        y + row_height / 2.0,
                    ),
                    egui::Align2::CENTER_CENTER,
                    worst.severity.glyph(),
                    font.clone(),
                    severity_colour(&visuals, worst.severity),
                );
            }

            if opts.show_line_numbers {
                let is_caret_line = line == caret_line;
                painter.text(
                    egui::pos2(rect.left() + self.gutter.numbers_right(), y),
                    egui::Align2::RIGHT_TOP,
                    line + 1,
                    font.clone(),
                    if is_caret_line {
                        visuals.text_color()
                    } else {
                        visuals.weak_text_color()
                    },
                );
            }

            self.widest_text = self.widest_text.max(galley.size().x);
            if !text.is_empty() {
                painter.galley(
                    egui::pos2(text_left, y),
                    galley.clone(),
                    visuals.text_color(),
                );
            }

            // Bracket match: a faint box round each half of the pair, which
            // reads as "these two go together" without competing with the
            // selection or the caret.
            if let Some(pair) = &self.bracket_pair {
                for half in [&pair.open, &pair.close] {
                    let line_end = line_start + text.chars().count();
                    if half.start < line_start || half.start >= line_end.max(line_start) {
                        continue;
                    }
                    let column = half.start - line_start;
                    let x = text_left + galley.pos_from_cursor(ccursor(column)).left();
                    let width = galley.pos_from_cursor(ccursor(column + 1)).left()
                        - galley.pos_from_cursor(ccursor(column)).left();
                    painter.rect_stroke(
                        egui::Rect::from_min_size(
                            egui::pos2(x, y),
                            egui::vec2(width.max(1.0), row_height),
                        ),
                        2.0,
                        egui::Stroke::new(1.0, visuals.weak_text_color()),
                        egui::StrokeKind::Inside,
                    );
                }
            }

            let line_end = line_start + text.chars().count();
            for head in &caret_heads {
                if *head < line_start || *head > line_end {
                    continue;
                }
                let x = text_left + galley.pos_from_cursor(ccursor(head - line_start)).left();
                let here = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(1.5, row_height));
                if *head == self.selection.head && line == caret_line {
                    // The primary's rectangle is the one the completion popup
                    // anchors itself to, so it is the one worth remembering.
                    caret_rect = Some(here);
                } else {
                    extra_carets.push(here);
                }
            }
        }

        self.caret_screen_rect = caret_rect;
        // Extra carets do not blink. A dozen of them flashing in unison is
        // distracting, and a steady one is easier to count.
        if response.has_focus() {
            for caret in &extra_carets {
                painter.rect_filled(*caret, 0.0, visuals.strong_text_color());
            }
        }
        if let Some(caret) = caret_rect
            && response.has_focus()
            && self.blink_on()
        {
            painter.rect_filled(caret, 0.0, visuals.strong_text_color());
        }

        let scrolling = std::mem::take(&mut self.scroll_to_caret);
        if scrolling {
            // Derived from the line number rather than taken from `caret_rect`,
            // which only exists when the caret was painted -- that is, only
            // when it is *already* on screen. Keying the scroll off it meant
            // the one case that needs scrolling was the one case that could not
            // ask for it, so jumping to a search match or a definition outside
            // the visible range moved the caret and left the view behind.
            let y = rect.top() + self.fold_map.row_at(caret_line) as f32 * row_height;
            // A caret that was not painted is on a line off screen, and its
            // column matters as much as its line: laid out here, one line, so a
            // jump to the end of a long line does not scroll to its start.
            let x = match caret_rect {
                Some(r) => r.left(),
                None => {
                    let galley = painter.layout_no_wrap(
                        doc.line_text(caret_line),
                        font.clone(),
                        visuals.text_color(),
                    );
                    self.widest_text = self.widest_text.max(galley.size().x);
                    let column = self.selection.head - doc.line_start(caret_line);
                    text_left + galley.pos_from_cursor(ccursor(column)).left()
                }
            };
            // A couple of rows of context either side, so the target does not
            // land flush against the top or bottom edge -- and above it, room
            // for the sticky header as well, or scrolling up to a caret parks
            // it behind the band and the file appears not to have moved.
            let above = row_height * (sticky.len() as f32 + 2.0);
            let target = egui::Rect::from_min_max(
                egui::pos2(x, y - above),
                egui::pos2(x + 1.5, y + row_height * 3.0),
            );
            ui.scroll_to_rect(target, None);
        }

        // A wider line than any before it: the scroll area was sized before it
        // was measured, so ask for the frame that sizes it properly -- and if
        // this frame was scrolling to the caret, scroll again then, when the
        // area is wide enough to reach it.
        if self.widest_text > widest_before {
            if scrolling {
                self.scroll_to_caret = true;
            }
            ui.ctx().request_repaint();
        }

        // Last, so it covers the text, the selection and any caret that ran
        // underneath it. A pinned row that a caret shone through would read as
        // a caret on the wrong line.
        self.paint_sticky(
            ui,
            doc,
            &painter,
            &visuals,
            syntax,
            opts,
            font,
            &sticky,
            &sticky_spans,
            visible,
            text_left,
            row_height,
        );

        if response.has_focus() {
            // Keep the blink animating without spinning at the full frame rate.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    /// Draw the declarations enclosing the top of the viewport, pinned to it.
    ///
    /// The rows are the real lines of the file, highlighted the way they are
    /// highlighted in place, rather than a rendering of the declaration's name.
    /// A header made of names has to invent a notation for signatures and
    /// decorators and gets it subtly wrong; the source line is already the
    /// notation the reader knows, and it is the line they would have scrolled
    /// back to look at.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_sticky(
        &mut self,
        ui: &egui::Ui,
        doc: &Document,
        painter: &egui::Painter,
        visuals: &egui::Visuals,
        syntax: &SyntaxTheme,
        opts: EditorOptions,
        font: &egui::FontId,
        lines: &[usize],
        spans: &[Vec<editor_syntax::highlight::Span>],
        visible: egui::Rect,
        text_left: f32,
        row_height: f32,
    ) {
        // Cleared unconditionally, so switching the header off or scrolling
        // back to the top of a file cannot leave a click target behind on a
        // band that is no longer drawn.
        self.sticky_hits.clear();
        if lines.is_empty() {
            return;
        }

        let band = egui::Rect::from_min_max(
            egui::pos2(visible.left(), visible.top()),
            egui::pos2(
                visible.right(),
                visible.top() + row_height * lines.len() as f32,
            ),
        );
        // Opaque first: the rows this covers are still painted underneath, and
        // a translucent band would show them through the pinned text. The panel
        // fill is the editor's own background. The wash over it is what says
        // these rows are pinned rather than simply the next rows of the file.
        painter.rect_filled(band, 0.0, visuals.panel_fill);
        painter.rect_filled(band, 0.0, visuals.faint_bg_color);

        for (row, line) in lines.iter().enumerate() {
            let y = band.top() + row as f32 * row_height;
            let text = doc.line_text(*line);
            let galley = match spans.get(row) {
                Some(spans) if !spans.is_empty() => {
                    let job = highlighted_line(
                        &text,
                        doc.text().line_to_byte((*line).min(doc.line_count())),
                        spans,
                        syntax,
                        font,
                        visuals.text_color(),
                    );
                    ui.fonts_mut(|f| f.layout_job(job))
                }
                _ => painter.layout_no_wrap(text.clone(), font.clone(), visuals.text_color()),
            };

            // The real line number, not a row index. Half of what makes the
            // band trustworthy is that it says where in the file the line is,
            // so the reader can tell it apart from the code below it.
            if opts.show_line_numbers {
                painter.text(
                    egui::pos2(
                        text_left - self.gutter.width() + self.gutter.numbers_right(),
                        y,
                    ),
                    egui::Align2::RIGHT_TOP,
                    line + 1,
                    font.clone(),
                    visuals.weak_text_color(),
                );
            }
            painter.galley(egui::pos2(text_left, y), galley, visuals.text_color());

            self.sticky_hits.push((
                egui::Rect::from_min_max(
                    egui::pos2(band.left(), y),
                    egui::pos2(band.right(), y + row_height),
                ),
                *line,
            ));
        }

        // A rule under the band, so it reads as a header rather than as code
        // that has refused to scroll.
        painter.hline(
            band.x_range(),
            band.bottom(),
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        );
    }

    /// Solid for a moment after any interaction, so the caret is never invisible
    /// exactly when the user looks for it.
    /// Tell the accessibility tree what this widget is and where the caret is.
    ///
    /// Without this the editor is an unlabelled rectangle: a screen reader
    /// announces the menus, the buttons and the file tree, and then nothing at
    /// all for the one part of the window that matters. egui reports its own
    /// widgets automatically, but a custom-painted one has to say so itself.
    ///
    /// The value reported is the **current line**, not the document. A screen
    /// reader announces text as the caret moves through it, so the line is the
    /// unit it actually wants; handing it a five-megabyte string every time the
    /// caret moves would be both useless and ruinous.
    pub(super) fn describe_for_screen_readers(
        &self,
        ui: &egui::Ui,
        doc: &Document,
        response: &egui::Response,
    ) {
        let (line, column) = doc.line_col(self.selection.head);
        let line_index = line - 1;
        let text = doc.line_text(line_index);
        let line_start = doc.line_start(line_index);
        let selection = self.selection.range();
        // Clamped to this line, in characters: the selection may run off both
        // ends of it, and a range outside the reported value is nonsense. The
        // line's *byte* length would be the wrong bound — a range in character
        // offsets bounded by a byte count is only right for ASCII.
        let line_chars = doc.line_len(line_index);
        let from = selection.start.saturating_sub(line_start).min(line_chars);
        let to = selection.end.saturating_sub(line_start).min(line_chars);
        let (from, to) = (egui::text::CharIndex(from), egui::text::CharIndex(to));

        let editable = doc.is_editable();
        let label = format!(
            "Code editor, line {line} of {}, column {column}",
            doc.line_count()
        );

        response.widget_info(|| egui::WidgetInfo {
            typ: egui::WidgetType::TextEdit,
            enabled: editable,
            label: Some(label.clone()),
            current_text_value: Some(text.clone()),
            text_selection: Some(from..to),
            ..egui::WidgetInfo::new(egui::WidgetType::TextEdit)
        });

        // The node itself, so the editor has a role and a name in the tree even
        // when nothing has happened to raise an event.
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(accesskit::Role::MultilineTextInput);
            node.set_label(label.clone());
            node.set_value(text.clone());
            if !editable {
                node.set_read_only();
            }
        });
    }

    pub(super) fn blink_on(&self) -> bool {
        // A blinking caret is the animation accessibility guidance names first,
        // and the one that is on screen the whole time you are reading.
        if self.reduce_motion {
            return true;
        }
        let Some(since) = self.last_interaction.map(|t| t.elapsed().as_millis()) else {
            return true;
        };
        since < 500 || (since / BLINK_MS).is_multiple_of(2)
    }
}

/// Build a layout job for one line, splitting it into the runs the highlighter
/// produced.
///
/// `spans` covers the whole visible window and is sorted; only the part
/// overlapping this line is used. Bytes with no span get the theme's default
/// colour, so the output always covers the line exactly once with no gaps.
pub(super) fn highlighted_line(
    text: &str,
    line_start_byte: usize,
    spans: &[editor_syntax::highlight::Span],
    syntax: &SyntaxTheme,
    font: &egui::FontId,
    fallback: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let line_end_byte = line_start_byte + text.len();
    let default = syntax.default_style();

    let mut cursor = 0usize; // byte offset within `text`
    let push = |job: &mut egui::text::LayoutJob, range: std::ops::Range<usize>, style| {
        let Some(slice) = text.get(range) else { return };
        if slice.is_empty() {
            return;
        }
        job.append(slice, 0.0, format_for(style, font, fallback));
    };

    for span in spans {
        if span.range.end <= line_start_byte {
            continue;
        }
        if span.range.start >= line_end_byte {
            break;
        }
        let from = span
            .range
            .start
            .saturating_sub(line_start_byte)
            .min(text.len());
        let to = (span.range.end - line_start_byte).min(text.len());
        if from >= to {
            continue;
        }
        if from > cursor {
            push(&mut job, cursor..from, default);
        }
        push(&mut job, from..to, span.style);
        cursor = to;
    }
    if cursor < text.len() {
        push(&mut job, cursor..text.len(), default);
    }

    job
}

pub(super) fn format_for(
    style: editor_syntax::theme::Style,
    font: &egui::FontId,
    fallback: egui::Color32,
) -> egui::TextFormat {
    let editor_syntax::theme::Rgb(r, g, b) = style.colour;
    let colour = if style == editor_syntax::theme::Style::default() {
        fallback
    } else {
        egui::Color32::from_rgb(r, g, b)
    };
    // `style.bold` is deliberately not applied. egui has no synthetic bold:
    // rendering it needs a bold face registered in the font family, which
    // arrives when JetBrains Mono is embedded in M9. Themes can express bold
    // now so theme files do not need rewriting later; it is simply not drawn
    // yet, and no built-in colour relies on it to be distinguishable.
    egui::TextFormat {
        font_id: font.clone(),
        color: colour,
        italics: style.italic,
        ..Default::default()
    }
}

/// Draw a wavy underline.
///
/// A squiggle rather than a straight line because it is the one underline
/// convention nobody confuses with a hyperlink or a spelling of emphasis, and
/// because it survives being drawn under a selection.
pub(super) fn paint_squiggle(
    painter: &egui::Painter,
    x0: f32,
    x1: f32,
    y: f32,
    colour: egui::Color32,
) {
    const WAVELENGTH: f32 = 4.0;
    const AMPLITUDE: f32 = 1.5;

    let mut points = Vec::new();
    let mut x = x0;
    let mut up = true;
    while x < x1 {
        points.push(egui::pos2(
            x,
            if up { y - AMPLITUDE } else { y + AMPLITUDE },
        ));
        x += WAVELENGTH / 2.0;
        up = !up;
    }
    points.push(egui::pos2(
        x1,
        if up { y - AMPLITUDE } else { y + AMPLITUDE },
    ));

    if points.len() >= 2 {
        painter.add(egui::Shape::line(points, egui::Stroke::new(1.0, colour)));
    }
}

/// Theme-aware colour for a diagnostic severity.
///
/// Public so the Problems panel colours its rows the same way as the squiggles
/// — two palettes for the same thing would be worse than one imperfect one.
pub fn severity_colour(
    visuals: &egui::Visuals,
    severity: editor_lsp::diagnostics::Severity,
) -> egui::Color32 {
    use editor_lsp::diagnostics::Severity;
    match severity {
        Severity::Error => visuals.error_fg_color,
        Severity::Warning => visuals.warn_fg_color,
        Severity::Information | Severity::Hint => visuals.weak_text_color(),
    }
}

/// Theme-aware colour for a gutter change bar.
///
/// Public for the same reason [`severity_colour`] is: anything else that
/// explains these marks — a legend, a diff view — has to agree with them.
///
/// Fixed colours rather than the theme's, because these three have to be
/// distinguishable from each other at three pixels wide, and a palette that
/// merely contrasts with the background does not guarantee that. They are
/// lightened on a dark background so all three stay legible either way.
#[must_use]
pub fn change_colour(visuals: &egui::Visuals, status: LineStatus) -> egui::Color32 {
    let (light, dark) = match status {
        LineStatus::Added => (
            egui::Color32::from_rgb(0x2d, 0x8c, 0x4a),
            egui::Color32::from_rgb(0x4b, 0xb5, 0x6b),
        ),
        LineStatus::Changed => (
            egui::Color32::from_rgb(0x1a, 0x6f, 0xb8),
            egui::Color32::from_rgb(0x54, 0xa2, 0xe0),
        ),
        LineStatus::DeletedAbove => (
            egui::Color32::from_rgb(0xc0, 0x3a, 0x3a),
            egui::Color32::from_rgb(0xe0, 0x66, 0x66),
        ),
    };
    if visuals.dark_mode { dark } else { light }
}
