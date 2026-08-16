//! Turning a byte stream from a PTY into styled lines.
//!
//! Only what program output actually uses is implemented: SGR colour and
//! attributes, carriage return, backspace, and erase-line. That is enough for
//! `cargo`'s diagnostics, coloured test runners, and the progress bars that
//! redraw a line with `\r`. It is not a terminal emulator — there is no cursor
//! addressing, no alternate screen, no scroll regions. A program that wants
//! those wants a real terminal, which is a post-1.0 feature.

use vte::{Params, Parser, Perform};

/// A colour in output. Kept abstract so the widget maps the sixteen standard
/// ones onto the active theme rather than hard-coding a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// One of the 16 standard terminal colours, 0-15.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How a run of output text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub foreground: Option<Colour>,
    pub background: Option<Colour>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// A run of text sharing one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// One line of output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub runs: Vec<Run>,
}

impl Line {
    /// The line's text with styling stripped, for link detection and copying.
    #[must_use]
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.is_empty())
    }
}

/// Strip escape sequences from a line of output.
///
/// For code that needs to *read* what a program printed rather than draw it —
/// a test runner matching `test foo ... ok`, say, against a runner that colours
/// its output because it can see a terminal. Turning the colour off instead
/// would be simpler and would also take the colour away from the console, where
/// it is wanted.
///
/// Handles the two forms that appear in program output: CSI sequences
/// (`ESC [ … final`) and the two-character ones (`ESC` plus a byte). OSC
/// strings are consumed up to their terminator. Anything else escape-like is
/// dropped rather than guessed at, which is right for a function whose job is
/// to leave only the text.
#[must_use]
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            // A lone carriage return means the line was redrawn — a progress
            // bar — and what matters is what it was redrawn *as*.
            if c == '\r' {
                out.clear();
            } else {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then a final byte in @-~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: a string, terminated by BEL or by ESC \.
            Some(']') => {
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if c == '\u{7}' || (previous == '\u{1b}' && c == '\\') {
                        break;
                    }
                    previous = c;
                }
            }
            // Anything else is a short sequence. Those in `0x20..=0x2f` are
            // *intermediate* bytes with a final byte still to come — `ESC ( B`,
            // which selects a character set, is three long — so keep taking
            // until the final one. Everything else is two, already consumed.
            Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                for c in chars.by_ref() {
                    if !('\u{20}'..='\u{2f}').contains(&c) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Accumulates styled lines from a byte stream.
pub struct AnsiSink {
    parser: Parser,
    state: SinkState,
}

impl std::fmt::Debug for AnsiSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnsiSink")
            .field("lines", &self.state.lines.len())
            .finish()
    }
}

struct SinkState {
    lines: Vec<Line>,
    current: Line,
    style: Style,
    /// Column the next character is written at. Tracked so `\r` can overwrite
    /// rather than starting a new line, which is how progress bars work.
    column: usize,
    max_lines: usize,
    /// Set when scrollback was trimmed, so the console can say so.
    trimmed: bool,
}

impl AnsiSink {
    /// `max_lines` caps the scrollback: a runaway loop printing forever must
    /// not exhaust memory.
    #[must_use]
    pub fn new(max_lines: usize) -> Self {
        Self {
            parser: Parser::new(),
            state: SinkState {
                lines: Vec::new(),
                current: Line::default(),
                style: Style::default(),
                column: 0,
                max_lines: max_lines.max(1),
                trimmed: false,
            },
        }
    }

    /// Feed bytes from the PTY.
    ///
    /// The parser is stateful across calls, which matters because PTY reads
    /// land on arbitrary boundaries: both an escape sequence and a multi-byte
    /// character can be torn in half between two reads.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
        self.state.trim();
    }

    /// Completed lines, oldest first. The line currently being written is
    /// included, so partial output is visible as it arrives.
    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.state
            .lines
            .iter()
            .chain(std::iter::once(&self.state.current))
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.state.lines.len() + 1
    }

    /// True if output was dropped from the top to stay within the limit.
    #[must_use]
    pub fn was_trimmed(&self) -> bool {
        self.state.trimmed
    }

    pub fn clear(&mut self) {
        self.state.lines.clear();
        self.state.current = Line::default();
        self.state.column = 0;
        self.state.trimmed = false;
    }

    /// Append text that did not come from the child — the command header, the
    /// exit banner — in the default style.
    ///
    /// Only breaks the current line if there is something on it. Ending an
    /// already-empty line would emit a blank one before every banner, which is
    /// what put a gap between the command header and the working directory.
    pub fn push_line(&mut self, text: &str) {
        if !self.state.current.is_empty() {
            self.state.finish_line();
        }
        self.state.current.runs.push(Run {
            text: text.to_owned(),
            style: Style::default(),
        });
        self.state.finish_line();
        self.state.trim();
    }
}

impl SinkState {
    fn finish_line(&mut self) {
        let line = std::mem::take(&mut self.current);
        self.lines.push(line);
        self.column = 0;
    }

    fn trim(&mut self) {
        if self.lines.len() > self.max_lines {
            let excess = self.lines.len() - self.max_lines;
            self.lines.drain(..excess);
            self.trimmed = true;
        }
    }

    /// Write a character at the current column, overwriting if `\r` moved back.
    fn put(&mut self, c: char) {
        let plain = self.current.plain();
        let width = plain.chars().count();

        if self.column < width {
            // Overwriting: rebuild the line with the character replaced. Only
            // happens after a carriage return, which is rare enough that the
            // cost does not matter.
            let mut rebuilt: String = plain.chars().take(self.column).collect();
            rebuilt.push(c);
            rebuilt.extend(plain.chars().skip(self.column + 1));
            self.current.runs = vec![Run {
                text: rebuilt,
                style: self.style,
            }];
        } else if self
            .current
            .runs
            .last()
            .is_some_and(|r| r.style == self.style)
        {
            // Same style as the previous run: extend it rather than starting a
            // new one, so a line of plain text is one run and not one per byte.
            if let Some(run) = self.current.runs.last_mut() {
                run.text.push(c);
            }
        } else {
            self.current.runs.push(Run {
                text: c.to_string(),
                style: self.style,
            });
        }
        self.column += 1;
    }

    /// Apply a Select Graphic Rendition sequence.
    fn sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param.first().copied().unwrap_or(0) {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                22 => self.style.bold = false,
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                n @ 30..=37 => self.style.foreground = Some(Colour::Indexed((n - 30) as u8)),
                38 => self.style.foreground = extended(param, &mut iter),
                39 => self.style.foreground = None,
                n @ 40..=47 => self.style.background = Some(Colour::Indexed((n - 40) as u8)),
                48 => self.style.background = extended(param, &mut iter),
                49 => self.style.background = None,
                // Bright variants map onto 8-15.
                n @ 90..=97 => self.style.foreground = Some(Colour::Indexed((n - 90 + 8) as u8)),
                n @ 100..=107 => self.style.background = Some(Colour::Indexed((n - 100 + 8) as u8)),
                _ => {}
            }
        }
    }
}

/// `38;5;n` (256-colour) and `38;2;r;g;b` (truecolour), in both the
/// semicolon-separated and colon-separated forms.
fn extended(param: &[u16], iter: &mut vte::ParamsIter<'_>) -> Option<Colour> {
    // Colon form: the whole thing arrives as one parameter.
    if param.len() > 1 {
        return match param.get(1) {
            Some(5) => param.get(2).map(|n| Colour::Indexed(*n as u8)),
            Some(2) => Some(Colour::Rgb(
                *param.get(2)? as u8,
                *param.get(3)? as u8,
                *param.get(4)? as u8,
            )),
            _ => None,
        };
    }
    // Semicolon form: read the following parameters.
    match iter.next()?.first()? {
        5 => iter.next()?.first().map(|n| Colour::Indexed(*n as u8)),
        2 => {
            let r = *iter.next()?.first()? as u8;
            let g = *iter.next()?.first()? as u8;
            let b = *iter.next()?.first()? as u8;
            Some(Colour::Rgb(r, g, b))
        }
        _ => None,
    }
}

impl Perform for SinkState {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.finish_line(),
            // Carriage return moves to the start of the line without ending it,
            // which is exactly how a progress bar redraws itself.
            b'\r' => self.column = 0,
            b'\t' => {
                let next_stop = (self.column / 8 + 1) * 8;
                for _ in self.column..next_stop {
                    self.put(' ');
                }
            }
            0x08 => self.column = self.column.saturating_sub(1),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match action {
            'm' => self.sgr(params),
            'K' => {
                // Erase in line. Only mode 0 (to end of line) and 2 (whole
                // line) show up in practice.
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let plain = self.current.plain();
                let kept: String = match mode {
                    2 => String::new(),
                    _ => plain.chars().take(self.column).collect(),
                };
                self.current.runs = if kept.is_empty() {
                    Vec::new()
                } else {
                    vec![Run {
                        text: kept,
                        style: self.style,
                    }]
                };
                if mode == 2 {
                    self.column = 0;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod strip_tests {
    use super::strip;

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(strip("test foo ... ok"), "test foo ... ok");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn colour_is_removed() {
        assert_eq!(strip("\u{1b}[32mok\u{1b}[0m"), "ok");
        assert_eq!(
            strip("test \u{1b}[1;31mFAILED\u{1b}[0m here"),
            "test FAILED here"
        );
    }

    /// What pytest and cargo actually emit around a percentage or a path.
    #[test]
    fn a_real_line_of_test_output_comes_out_readable() {
        let line = "\u{1b}[1mtests/test_a.py\u{1b}[0m::\u{1b}[1mtest_b\u{1b}[0m \
                    \u{1b}[32mPASSED\u{1b}[0m\u{1b}[36m [ 50%]\u{1b}[0m";
        assert_eq!(strip(line), "tests/test_a.py::test_b PASSED [ 50%]");
    }

    #[test]
    fn an_osc_title_is_removed_whole() {
        assert_eq!(strip("\u{1b}]0;a title\u{7}after"), "after");
        assert_eq!(strip("\u{1b}]0;a title\u{1b}\\after"), "after");
    }

    /// A progress bar redraws its line, and what it was redrawn as is what the
    /// line says.
    #[test]
    fn a_carriage_return_keeps_only_what_came_after_it() {
        assert_eq!(strip("first go\rsecond go"), "second go");
        assert_eq!(strip("  50%\r 100%"), " 100%");
    }

    #[test]
    fn a_truncated_sequence_does_not_leak_into_the_text() {
        // A read boundary can land inside a sequence; better to lose the
        // fragment than to print `[32m` in the middle of a name.
        assert_eq!(strip("before\u{1b}[32"), "before");
        assert_eq!(strip("before\u{1b}"), "before");
    }

    #[test]
    fn two_character_sequences_are_dropped() {
        assert_eq!(strip("a\u{1b}(Bb"), "ab");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(input: &str) -> Vec<Line> {
        let mut sink = AnsiSink::new(1000);
        sink.feed(input.as_bytes());
        sink.lines().cloned().collect()
    }

    fn plain_lines(input: &str) -> Vec<String> {
        feed(input).iter().map(Line::plain).collect()
    }

    #[test]
    fn plain_text_is_split_into_lines() {
        assert_eq!(plain_lines("one\ntwo\n"), ["one", "two", ""]);
    }

    #[test]
    fn a_line_of_plain_text_is_one_run_not_one_per_character() {
        let lines = feed("hello world");
        assert_eq!(
            lines[0].runs.len(),
            1,
            "adjacent characters in the same style must be coalesced"
        );
    }

    #[test]
    fn sgr_colours_are_applied_and_reset() {
        let lines = feed("\x1b[31mred\x1b[0m plain");
        let runs = &lines[0].runs;
        assert_eq!(runs[0].text, "red");
        assert_eq!(runs[0].style.foreground, Some(Colour::Indexed(1)));
        assert_eq!(runs[1].text, " plain");
        assert_eq!(runs[1].style.foreground, None);
    }

    #[test]
    fn bold_and_italic_are_tracked() {
        let lines = feed("\x1b[1mbold\x1b[22m\x1b[3mitalic\x1b[0m");
        assert!(lines[0].runs[0].style.bold);
        assert!(!lines[0].runs[1].style.bold);
        assert!(lines[0].runs[1].style.italic);
    }

    #[test]
    fn bright_colours_map_to_the_upper_eight() {
        let lines = feed("\x1b[91mbright red\x1b[0m");
        assert_eq!(lines[0].runs[0].style.foreground, Some(Colour::Indexed(9)));
    }

    #[test]
    fn truecolour_is_parsed_in_the_semicolon_form() {
        let lines = feed("\x1b[38;2;10;20;30mrgb\x1b[0m");
        assert_eq!(
            lines[0].runs[0].style.foreground,
            Some(Colour::Rgb(10, 20, 30))
        );
    }

    #[test]
    fn indexed_256_colour_is_parsed() {
        let lines = feed("\x1b[38;5;208morange\x1b[0m");
        assert_eq!(
            lines[0].runs[0].style.foreground,
            Some(Colour::Indexed(208))
        );
    }

    #[test]
    fn carriage_return_overwrites_rather_than_starting_a_new_line() {
        // How every progress bar works. Getting this wrong fills the console
        // with thousands of near-identical lines.
        assert_eq!(plain_lines("50%\r100%"), ["100%"]);
    }

    #[test]
    fn overwriting_a_longer_line_leaves_its_tail_behind() {
        // Not a bug: a terminal has no idea the new text is shorter, so the
        // remainder of the old line stays on screen. Programs that care emit
        // an erase-line, which is covered by the next test. Asserting
        // "shorter" here would have been asserting the wrong behaviour.
        assert_eq!(plain_lines("longer\rshort"), ["shortr"]);
    }

    #[test]
    fn erase_line_after_a_carriage_return_clears_the_remainder() {
        assert_eq!(plain_lines("longer\rshort\x1b[K"), ["short"]);
    }

    #[test]
    fn tabs_advance_to_the_next_stop() {
        assert_eq!(plain_lines("a\tb"), ["a       b"]);
    }

    #[test]
    fn backspace_moves_the_cursor_back() {
        assert_eq!(plain_lines("abc\x1b[Kx"), ["abcx"]);
        assert_eq!(plain_lines("ab\x08c"), ["ac"]);
    }

    #[test]
    fn scrollback_is_capped_and_reports_that_it_trimmed() {
        let mut sink = AnsiSink::new(10);
        for i in 0..100 {
            sink.feed(format!("line {i}\n").as_bytes());
        }
        assert!(sink.line_count() <= 11, "got {}", sink.line_count());
        assert!(sink.was_trimmed());

        let last = sink.lines().map(Line::plain).nth(sink.line_count() - 2);
        assert_eq!(
            last.as_deref(),
            Some("line 99"),
            "the newest output must survive, not the oldest"
        );
    }

    #[test]
    fn unrecognised_escape_sequences_are_ignored_not_printed() {
        // Cursor addressing is not implemented, but it must not leak into the
        // output as visible junk.
        assert_eq!(plain_lines("\x1b[2Jclean"), ["clean"]);
        assert_eq!(plain_lines("\x1b[10;20Hclean"), ["clean"]);
    }

    #[test]
    fn injected_lines_appear_in_the_default_style() {
        let mut sink = AnsiSink::new(100);
        sink.feed(b"\x1b[31mred output");
        sink.push_line("Process exited with code 0");

        let lines: Vec<Line> = sink.lines().cloned().collect();
        let banner = lines
            .iter()
            .find(|l| l.plain().contains("exited"))
            .expect("banner present");
        assert_eq!(
            banner.runs[0].style,
            Style::default(),
            "the exit banner must not inherit the program's colour"
        );
    }

    #[test]
    fn injected_lines_do_not_leave_blank_lines_behind_them() {
        // Regression: `push_line` always ended the current line, so a banner
        // written when nothing was part-way through emitted an empty line
        // first — visible as a gap between the command header and the working
        // directory.
        let mut sink = AnsiSink::new(100);
        sink.push_line("> command");
        sink.push_line("  in /somewhere");

        let lines: Vec<String> = sink.lines().map(Line::plain).collect();
        assert_eq!(
            lines,
            ["> command", "  in /somewhere", ""],
            "expected no blank line between the banners"
        );
    }

    #[test]
    fn a_banner_after_partial_output_starts_on_its_own_line() {
        // The other half of the same rule: output that stopped mid-line must
        // not have the banner appended to it.
        let mut sink = AnsiSink::new(100);
        sink.feed(b"no trailing newline");
        sink.push_line("[Finished]");

        let lines: Vec<String> = sink.lines().map(Line::plain).collect();
        assert_eq!(lines, ["no trailing newline", "[Finished]", ""]);
    }

    #[test]
    fn a_banner_after_a_complete_line_does_not_add_a_gap() {
        let mut sink = AnsiSink::new(100);
        sink.feed(b"one\ntwo\n");
        sink.push_line("[Finished]");

        let lines: Vec<String> = sink.lines().map(Line::plain).collect();
        assert_eq!(
            lines,
            ["one", "two", "[Finished]", ""],
            "a trailing newline already ended the line"
        );
    }

    #[test]
    fn clearing_resets_everything() {
        let mut sink = AnsiSink::new(100);
        sink.feed(b"some output\nmore\n");
        sink.clear();
        assert_eq!(sink.line_count(), 1);
        assert!(!sink.was_trimmed());
        assert!(sink.lines().all(Line::is_empty));
    }

    #[test]
    fn multibyte_output_is_decoded_correctly() {
        assert_eq!(
            plain_lines("caf\u{e9} \u{1f600}\n"),
            ["caf\u{e9} \u{1f600}", ""]
        );
    }

    #[test]
    fn a_sequence_split_across_two_reads_still_parses() {
        // PTY reads land on arbitrary boundaries, so an escape sequence can be
        // torn in half. The parser is stateful precisely for this.
        let mut sink = AnsiSink::new(100);
        sink.feed(b"\x1b[3");
        sink.feed(b"1mred");
        let lines: Vec<Line> = sink.lines().cloned().collect();
        assert_eq!(lines[0].runs[0].text, "red");
        assert_eq!(lines[0].runs[0].style.foreground, Some(Colour::Indexed(1)));
    }
}
