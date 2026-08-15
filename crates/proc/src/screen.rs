//! A terminal screen: a grid of cells, a cursor, and the escape sequences that
//! move them about.
//!
//! [`crate::ansi`] turns a byte stream into a list of lines, which is what a
//! build log is. This is the other thing a terminal can be: a fixed grid that a
//! program draws on and redraws, addressing any cell it likes. `cargo` needs
//! the first; `claude`, `vim`, `htop` and `git rebase -i` need this.
//!
//! What is implemented is what full-screen programs actually use — cursor
//! addressing, erase, insert and delete of lines and characters, scroll
//! regions, the alternate screen, and the handful of DEC private modes that
//! decide whether the cursor is visible and how paste is delivered. Not a
//! complete DEC VT: no double-width lines, no character sets, no mouse
//! reporting yet.
//!
//! The primary screen keeps scrollback, because that is where a shell lives and
//! scrolling back through what a command printed is the point. The alternate
//! screen does not: a program that asks for it is drawing a whole window and
//! will redraw it, and keeping its intermediate frames would fill the
//! scrollback with the corpses of previous ones.

use crate::ansi::{Colour, Line, Run, Style};

/// One cell of the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    ch: char,
    style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

/// A grid of cells with a cursor.
#[derive(Debug, Clone)]
struct Grid {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
}

impl Grid {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
        }
    }

    fn at(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells.get_mut(row * self.cols + col)
    }

    fn row(&self, row: usize) -> &[Cell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }

    fn clear_row(&mut self, row: usize, style: Style) {
        if row >= self.rows {
            return;
        }
        let start = row * self.cols;
        for cell in &mut self.cells[start..start + self.cols] {
            *cell = Cell { ch: ' ', style };
        }
    }
}

/// A terminal screen driven by a byte stream.
pub struct Screen {
    grid: Grid,
    /// The alternate screen, when a program has asked for one.
    alternate: Option<Grid>,
    parser: vte::Parser,
    state: State,
}

/// Everything the escape sequences mutate, kept apart from the parser so
/// `vte::Perform` can borrow it alone.
#[derive(Debug)]
struct State {
    row: usize,
    col: usize,
    style: Style,
    saved: Option<(usize, usize, Style)>,
    /// Rows the scroll region covers, inclusive. Set by DECSTBM.
    scroll_top: usize,
    scroll_bottom: usize,
    cursor_visible: bool,
    /// Set when the next printed character should wrap first. Deferring the
    /// wrap is what stops a character written to the last column from moving
    /// the cursor down before anything else has been printed — which would put
    /// a blank line into everything that fills a row exactly.
    wrap_pending: bool,
    /// Lines that have scrolled off the primary screen.
    scrollback: Vec<Line>,
    max_scrollback: usize,
    /// True while the alternate screen is in use, so scrolled-off rows are
    /// discarded rather than kept.
    in_alternate: bool,
    /// Set when a program asks for bracketed paste, so pasted text is wrapped
    /// in the markers it is waiting for.
    bracketed_paste: bool,
    /// DECCKM. When set, the arrow keys must be sent as `ESC O A` rather than
    /// `ESC [ A` — readline and every full-screen program turns this on, and
    /// sending the wrong one gives you a stray `A` in the buffer instead of
    /// moving the cursor.
    application_cursor: bool,
    /// What the program last set the window title to.
    title: Option<String>,
    /// True once anything at all has been written, so a fresh terminal can be
    /// told from one showing a screen of spaces.
    written: bool,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("rows", &self.grid.rows)
            .field("cols", &self.grid.cols)
            .field("alternate", &self.state.in_alternate)
            .field("scrollback", &self.state.scrollback.len())
            .finish()
    }
}

impl Screen {
    #[must_use]
    pub fn new(rows: usize, cols: usize, max_scrollback: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            grid: Grid::new(rows, cols),
            alternate: None,
            parser: vte::Parser::new(),
            state: State {
                row: 0,
                col: 0,
                style: Style::default(),
                saved: None,
                scroll_top: 0,
                scroll_bottom: rows - 1,
                cursor_visible: true,
                wrap_pending: false,
                scrollback: Vec::new(),
                max_scrollback,
                in_alternate: false,
                bracketed_paste: false,
                application_cursor: false,
                title: None,
                written: false,
            },
        }
    }

    /// Feed bytes from the pseudo-terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut performer = Performer {
            grid: &mut self.grid,
            alternate: &mut self.alternate,
            state: &mut self.state,
        };
        self.parser.advance(&mut performer, bytes);
    }

    /// Change the grid size.
    ///
    /// The contents are re-laid rather than reflowed: a program on the
    /// alternate screen redraws everything on resize anyway, and reflowing a
    /// shell's scrollback correctly means tracking which rows were
    /// continuations, which is a great deal of bookkeeping for something seen
    /// only while a window is being dragged.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.grid.rows && cols == self.grid.cols {
            return;
        }

        let mut grid = Grid::new(rows, cols);
        for row in 0..rows.min(self.grid.rows) {
            for col in 0..cols.min(self.grid.cols) {
                if let Some(cell) = grid.at(row, col) {
                    *cell = self.grid.row(row)[col].clone();
                }
            }
        }
        self.grid = grid;
        if let Some(alternate) = &self.alternate {
            let mut fresh = Grid::new(rows, cols);
            for row in 0..rows.min(alternate.rows) {
                for col in 0..cols.min(alternate.cols) {
                    if let Some(cell) = fresh.at(row, col) {
                        *cell = alternate.row(row)[col].clone();
                    }
                }
            }
            self.alternate = Some(fresh);
        }

        self.state.scroll_top = 0;
        self.state.scroll_bottom = rows - 1;
        self.state.row = self.state.row.min(rows - 1);
        self.state.col = self.state.col.min(cols - 1);
        self.state.wrap_pending = false;
    }

    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.grid.rows, self.grid.cols)
    }

    /// The cursor's row and column on the visible grid.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.state.row, self.state.col)
    }

    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.state.cursor_visible
    }

    #[must_use]
    pub fn is_alternate(&self) -> bool {
        self.state.in_alternate
    }

    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.state.bracketed_paste
    }

    /// True when the arrow keys should be sent in their application form.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.state.application_cursor
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.state.title.as_deref()
    }

    /// Lines that have scrolled off the top, oldest first.
    ///
    /// Empty on the alternate screen, where there is nothing to scroll back to.
    #[must_use]
    pub fn scrollback(&self) -> &[Line] {
        &self.state.scrollback
    }

    /// The visible grid as styled lines, top row first.
    #[must_use]
    pub fn visible_lines(&self) -> Vec<Line> {
        let grid = self.alternate.as_ref().unwrap_or(&self.grid);
        (0..grid.rows)
            .map(|row| row_to_line(grid.row(row)))
            .collect()
    }

    /// Everything on screen as plain text, for tests and for copying.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.visible_lines()
            .iter()
            .map(|line| line.plain().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Turn a row of cells into styled runs, merging neighbours that match.
fn row_to_line(cells: &[Cell]) -> Line {
    let mut runs: Vec<Run> = Vec::new();
    for cell in cells {
        match runs.last_mut() {
            Some(run) if run.style == cell.style => run.text.push(cell.ch),
            _ => runs.push(Run {
                text: cell.ch.to_string(),
                style: cell.style,
            }),
        }
    }
    Line { runs }
}

/// The `vte` callback target, holding the grid and the state it changes.
struct Performer<'a> {
    grid: &'a mut Grid,
    alternate: &'a mut Option<Grid>,
    state: &'a mut State,
}

impl Performer<'_> {
    /// The grid being drawn on: the alternate one when a program asked for it.
    fn active(&mut self) -> &mut Grid {
        match self.alternate {
            Some(grid) => grid,
            None => self.grid,
        }
    }

    fn rows(&mut self) -> usize {
        self.active().rows
    }

    fn cols(&mut self) -> usize {
        self.active().cols
    }

    fn put(&mut self, c: char) {
        let cols = self.cols();
        if self.state.wrap_pending {
            self.state.col = 0;
            self.line_feed();
            self.state.wrap_pending = false;
        }
        let (row, col) = (self.state.row, self.state.col);
        let style = self.state.style;
        if let Some(cell) = self.active().at(row, col) {
            *cell = Cell { ch: c, style };
        }
        self.state.written = true;

        if col + 1 >= cols {
            // At the right-hand edge the wrap waits for the next character;
            // see `wrap_pending`.
            self.state.wrap_pending = true;
        } else {
            self.state.col = col + 1;
        }
    }

    /// Move down one row, scrolling the region if that would leave it.
    fn line_feed(&mut self) {
        if self.state.row == self.state.scroll_bottom {
            self.scroll_up(1);
        } else if self.state.row + 1 < self.rows() {
            self.state.row += 1;
        }
    }

    /// Move the scroll region's contents up by `n`, filling from the bottom.
    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.state.scroll_top, self.state.scroll_bottom);
        let cols = self.cols();
        for _ in 0..n {
            // Only the primary screen's whole-width top row is worth keeping,
            // and only when the region starts at the top: a program scrolling
            // an inner region is animating, not producing history.
            if !self.state.in_alternate && top == 0 {
                let line = row_to_line(self.grid.row(0));
                self.state.scrollback.push(line);
                let max = self.state.max_scrollback;
                if self.state.scrollback.len() > max {
                    let excess = self.state.scrollback.len() - max;
                    self.state.scrollback.drain(0..excess);
                }
            }
            for row in top..bottom {
                let next: Vec<Cell> = self.active().row(row + 1).to_vec();
                for (col, cell) in next.into_iter().enumerate().take(cols) {
                    if let Some(target) = self.active().at(row, col) {
                        *target = cell;
                    }
                }
            }
            let style = self.state.style;
            self.active().clear_row(bottom, style);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.state.scroll_top, self.state.scroll_bottom);
        let cols = self.cols();
        for _ in 0..n {
            for row in (top..bottom).rev() {
                let above: Vec<Cell> = self.active().row(row).to_vec();
                for (col, cell) in above.into_iter().enumerate().take(cols) {
                    if let Some(target) = self.active().at(row + 1, col) {
                        *target = cell;
                    }
                }
            }
            let style = self.state.style;
            self.active().clear_row(top, style);
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        let (rows, cols) = (self.rows(), self.cols());
        self.state.row = row.min(rows.saturating_sub(1));
        self.state.col = col.min(cols.saturating_sub(1));
        self.state.wrap_pending = false;
    }

    /// Switch to or from the alternate screen.
    fn set_alternate(&mut self, on: bool) {
        if on == self.state.in_alternate {
            return;
        }
        self.state.in_alternate = on;
        if on {
            let (rows, cols) = (self.grid.rows, self.grid.cols);
            *self.alternate = Some(Grid::new(rows, cols));
        } else {
            *self.alternate = None;
        }
        // Both switches leave the cursor at the top-left in practice, because
        // every program that uses the alternate screen positions the cursor
        // itself immediately afterwards.
        self.state.row = 0;
        self.state.col = 0;
        self.state.wrap_pending = false;
        self.state.scroll_top = 0;
        self.state.scroll_bottom = self.rows().saturating_sub(1);
    }

    fn erase_in_display(&mut self, mode: u16) {
        let (rows, cols) = (self.rows(), self.cols());
        let (row, col) = (self.state.row, self.state.col);
        let style = self.state.style;
        match mode {
            // To the end of the screen.
            0 => {
                for c in col..cols {
                    if let Some(cell) = self.active().at(row, c) {
                        *cell = Cell { ch: ' ', style };
                    }
                }
                for r in row + 1..rows {
                    self.active().clear_row(r, style);
                }
            }
            // From the start of the screen.
            1 => {
                for r in 0..row {
                    self.active().clear_row(r, style);
                }
                for c in 0..=col.min(cols.saturating_sub(1)) {
                    if let Some(cell) = self.active().at(row, c) {
                        *cell = Cell { ch: ' ', style };
                    }
                }
            }
            // The whole screen. Mode 3 also clears scrollback, which is what
            // `clear` sends and what people expect it to do.
            _ => {
                for r in 0..rows {
                    self.active().clear_row(r, style);
                }
                if mode == 3 {
                    self.state.scrollback.clear();
                }
            }
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let cols = self.cols();
        let (row, col) = (self.state.row, self.state.col);
        let style = self.state.style;
        let range = match mode {
            0 => col..cols,
            1 => 0..(col + 1).min(cols),
            _ => 0..cols,
        };
        for c in range {
            if let Some(cell) = self.active().at(row, c) {
                *cell = Cell { ch: ' ', style };
            }
        }
    }

    /// Insert `n` blank lines at the cursor, pushing the rest of the region
    /// down. What an editor does when you press Enter.
    fn insert_lines(&mut self, n: usize) {
        if self.state.row < self.state.scroll_top || self.state.row > self.state.scroll_bottom {
            return;
        }
        let saved_top = self.state.scroll_top;
        self.state.scroll_top = self.state.row;
        self.scroll_down(n);
        self.state.scroll_top = saved_top;
    }

    fn delete_lines(&mut self, n: usize) {
        if self.state.row < self.state.scroll_top || self.state.row > self.state.scroll_bottom {
            return;
        }
        let saved_top = self.state.scroll_top;
        self.state.scroll_top = self.state.row;
        self.scroll_up(n);
        self.state.scroll_top = saved_top;
    }

    fn insert_chars(&mut self, n: usize) {
        let cols = self.cols();
        let (row, col) = (self.state.row, self.state.col);
        let style = self.state.style;
        let existing: Vec<Cell> = self.active().row(row).to_vec();
        for c in (col..cols).rev() {
            let source = c.checked_sub(n);
            let cell = match source {
                Some(from) if from >= col => existing[from].clone(),
                _ => Cell { ch: ' ', style },
            };
            if let Some(target) = self.active().at(row, c) {
                *target = cell;
            }
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let cols = self.cols();
        let (row, col) = (self.state.row, self.state.col);
        let style = self.state.style;
        let existing: Vec<Cell> = self.active().row(row).to_vec();
        for c in col..cols {
            let cell = existing
                .get(c + n)
                .cloned()
                .unwrap_or(Cell { ch: ' ', style });
            if let Some(target) = self.active().at(row, c) {
                *target = cell;
            }
        }
    }

    /// Apply a private mode set/reset (`CSI ? n h` / `CSI ? n l`).
    fn private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.state.application_cursor = on,
            25 => self.state.cursor_visible = on,
            // Both spellings of "give me the alternate screen". 1049 also saves
            // the cursor and clears, which is what the switch does here.
            47 | 1047 | 1049 => self.set_alternate(on),
            2004 => self.state.bracketed_paste = on,
            _ => {}
        }
    }

    fn sgr(&mut self, params: &vte::Params) {
        let mut iter = params.iter().flat_map(|p| p.iter().copied()).peekable();
        if iter.peek().is_none() {
            self.state.style = Style::default();
            return;
        }
        while let Some(code) = iter.next() {
            match code {
                0 => self.state.style = Style::default(),
                1 => self.state.style.bold = true,
                3 => self.state.style.italic = true,
                4 => self.state.style.underline = true,
                22 => self.state.style.bold = false,
                23 => self.state.style.italic = false,
                24 => self.state.style.underline = false,
                30..=37 => self.state.style.foreground = Some(Colour::Indexed(code as u8 - 30)),
                39 => self.state.style.foreground = None,
                40..=47 => self.state.style.background = Some(Colour::Indexed(code as u8 - 40)),
                49 => self.state.style.background = None,
                90..=97 => self.state.style.foreground = Some(Colour::Indexed(code as u8 - 90 + 8)),
                100..=107 => {
                    self.state.style.background = Some(Colour::Indexed(code as u8 - 100 + 8));
                }
                // Extended colour: `38;5;n` for indexed, `38;2;r;g;b` for true
                // colour, and the same at 48 for the background.
                38 | 48 => {
                    let foreground = code == 38;
                    let colour = match iter.next() {
                        Some(5) => iter.next().map(|n| Colour::Indexed(n as u8)),
                        Some(2) => {
                            let r = iter.next().unwrap_or(0) as u8;
                            let g = iter.next().unwrap_or(0) as u8;
                            let b = iter.next().unwrap_or(0) as u8;
                            Some(Colour::Rgb(r, g, b))
                        }
                        _ => None,
                    };
                    if foreground {
                        self.state.style.foreground = colour;
                    } else {
                        self.state.style.background = colour;
                    }
                }
                _ => {}
            }
        }
    }
}

/// First parameter, defaulting to `fallback` when absent or zero.
fn first(params: &vte::Params, fallback: u16) -> u16 {
    let value = params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .unwrap_or(0);
    if value == 0 { fallback } else { value }
}

fn nth(params: &vte::Params, index: usize, fallback: u16) -> u16 {
    let value = params
        .iter()
        .nth(index)
        .and_then(|p| p.first().copied())
        .unwrap_or(0);
    if value == 0 { fallback } else { value }
}

impl vte::Perform for Performer<'_> {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                self.state.wrap_pending = false;
                self.line_feed();
            }
            b'\r' => {
                self.state.col = 0;
                self.state.wrap_pending = false;
            }
            0x08 => {
                self.state.wrap_pending = false;
                self.state.col = self.state.col.saturating_sub(1);
            }
            b'\t' => {
                let cols = self.cols();
                let next = (self.state.col / 8 + 1) * 8;
                self.state.col = next.min(cols.saturating_sub(1));
                self.state.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        // `?` marks the DEC private modes, which mean something different from
        // the same numbers without it.
        let private = intermediates.first() == Some(&b'?');
        let n = first(params, 1) as usize;

        match (private, action) {
            (true, 'h') => self.private_mode(first(params, 0), true),
            (true, 'l') => self.private_mode(first(params, 0), false),
            (true, _) => {}

            (false, 'm') => self.sgr(params),
            (false, 'A') => self.state.row = self.state.row.saturating_sub(n),
            (false, 'B') => {
                let rows = self.rows();
                self.state.row = (self.state.row + n).min(rows.saturating_sub(1));
            }
            (false, 'C') => {
                let cols = self.cols();
                self.state.col = (self.state.col + n).min(cols.saturating_sub(1));
                self.state.wrap_pending = false;
            }
            (false, 'D') => {
                self.state.col = self.state.col.saturating_sub(n);
                self.state.wrap_pending = false;
            }
            // Next/previous line: down or up, and to column zero.
            (false, 'E') => {
                let rows = self.rows();
                self.state.row = (self.state.row + n).min(rows.saturating_sub(1));
                self.state.col = 0;
            }
            (false, 'F') => {
                self.state.row = self.state.row.saturating_sub(n);
                self.state.col = 0;
            }
            (false, 'G') => {
                let col = n.saturating_sub(1);
                let row = self.state.row;
                self.move_to(row, col);
            }
            (false, 'H' | 'f') => {
                let row = first(params, 1).saturating_sub(1) as usize;
                let col = nth(params, 1, 1).saturating_sub(1) as usize;
                self.move_to(row, col);
            }
            (false, 'J') => self.erase_in_display(first(params, 0)),
            (false, 'K') => self.erase_in_line(first(params, 0)),
            (false, 'L') => self.insert_lines(n),
            (false, 'M') => self.delete_lines(n),
            (false, 'P') => self.delete_chars(n),
            (false, '@') => self.insert_chars(n),
            (false, 'S') => self.scroll_up(n),
            (false, 'T') => self.scroll_down(n),
            // Erase characters: blank `n` cells without moving anything.
            (false, 'X') => {
                let cols = self.cols();
                let (row, col) = (self.state.row, self.state.col);
                let style = self.state.style;
                for c in col..(col + n).min(cols) {
                    if let Some(cell) = self.active().at(row, c) {
                        *cell = Cell { ch: ' ', style };
                    }
                }
            }
            (false, 'd') => {
                let row = n.saturating_sub(1);
                let col = self.state.col;
                self.move_to(row, col);
            }
            // Scroll region. Resetting it to the whole screen is what a program
            // sends on the way out, and `CSI r` with no parameters means that.
            (false, 'r') => {
                let rows = self.rows();
                let top = first(params, 1).saturating_sub(1) as usize;
                let bottom = nth(params, 1, rows as u16).saturating_sub(1) as usize;
                if top < bottom && bottom < rows {
                    self.state.scroll_top = top;
                    self.state.scroll_bottom = bottom;
                } else {
                    self.state.scroll_top = 0;
                    self.state.scroll_bottom = rows.saturating_sub(1);
                }
                self.move_to(self.state.scroll_top, 0);
            }
            (false, 's') => {
                self.state.saved = Some((self.state.row, self.state.col, self.state.style));
            }
            (false, 'u') => {
                if let Some((row, col, style)) = self.state.saved {
                    self.state.style = style;
                    self.move_to(row, col);
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            // Index, reverse index, next line.
            b'D' => self.line_feed(),
            b'M' => {
                if self.state.row == self.state.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.state.row = self.state.row.saturating_sub(1);
                }
            }
            b'E' => {
                self.state.col = 0;
                self.line_feed();
            }
            b'7' => self.state.saved = Some((self.state.row, self.state.col, self.state.style)),
            b'8' => {
                if let Some((row, col, style)) = self.state.saved {
                    self.state.style = style;
                    self.move_to(row, col);
                }
            }
            // Full reset.
            b'c' => {
                let (rows, cols) = (self.rows(), self.cols());
                *self.active() = Grid::new(rows, cols);
                self.state.style = Style::default();
                self.state.scroll_top = 0;
                self.state.scroll_bottom = rows.saturating_sub(1);
                self.move_to(0, 0);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // 0 and 2 both set the window title; 1 sets the icon name, which is the
        // same thing to anything with tabs rather than icons.
        let Some(kind) = params.first() else { return };
        if matches!(*kind, b"0" | b"1" | b"2")
            && let Some(text) = params.get(1)
        {
            self.state.title = Some(String::from_utf8_lossy(text).into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: usize, cols: usize) -> Screen {
        Screen::new(rows, cols, 100)
    }

    fn feed(screen: &mut Screen, text: &str) {
        screen.feed(text.as_bytes());
    }

    #[test]
    fn plain_text_lands_on_the_grid() {
        let mut s = screen(3, 10);
        feed(&mut s, "hello");
        assert_eq!(s.to_text(), "hello\n\n");
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn a_newline_moves_down_and_carriage_return_moves_to_the_left() {
        let mut s = screen(3, 10);
        feed(&mut s, "one\r\ntwo");
        assert_eq!(s.to_text(), "one\ntwo\n");
        assert_eq!(s.cursor(), (1, 3));
    }

    /// The whole point of a grid: a program can put a character anywhere.
    #[test]
    fn cursor_addressing_writes_where_it_is_told() {
        let mut s = screen(4, 10);
        feed(&mut s, "\x1b[3;5HX");
        assert_eq!(s.to_text(), "\n\n    X\n");
        assert_eq!(s.cursor(), (2, 5));
    }

    #[test]
    fn the_cursor_can_be_moved_relatively_in_all_four_directions() {
        let mut s = screen(5, 10);
        feed(&mut s, "\x1b[3;3H");
        feed(&mut s, "\x1b[A"); // up
        assert_eq!(s.cursor(), (1, 2));
        feed(&mut s, "\x1b[2B"); // down two
        assert_eq!(s.cursor(), (3, 2));
        feed(&mut s, "\x1b[3C"); // right three
        assert_eq!(s.cursor(), (3, 5));
        feed(&mut s, "\x1b[2D"); // left two
        assert_eq!(s.cursor(), (3, 3));
    }

    #[test]
    fn movement_cannot_leave_the_grid() {
        let mut s = screen(3, 5);
        feed(&mut s, "\x1b[99;99H");
        assert_eq!(s.cursor(), (2, 4));
        feed(&mut s, "\x1b[99A\x1b[99D");
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn erase_in_line_clears_the_right_part() {
        let mut s = screen(2, 8);
        feed(&mut s, "abcdefgh\x1b[1;4H\x1b[0K");
        assert_eq!(s.to_text(), "abc\n");

        let mut s = screen(2, 8);
        feed(&mut s, "abcdefgh\x1b[1;4H\x1b[1K");
        assert_eq!(s.to_text(), "    efgh\n");
    }

    #[test]
    fn erase_in_display_clears_the_right_part() {
        let mut s = screen(3, 4);
        feed(&mut s, "aaaa\r\nbbbb\r\ncccc");
        feed(&mut s, "\x1b[2;3H\x1b[0J");
        assert_eq!(s.to_text(), "aaaa\nbb\n");
    }

    /// What `clear` sends, and what people expect it to do.
    #[test]
    fn erasing_everything_also_empties_the_scrollback() {
        let mut s = screen(2, 4);
        feed(&mut s, "one\r\ntwo\r\nthree\r\n");
        assert!(!s.scrollback().is_empty());
        feed(&mut s, "\x1b[3J");
        assert!(s.scrollback().is_empty());
    }

    #[test]
    fn output_past_the_last_row_scrolls_and_keeps_history() {
        let mut s = screen(2, 6);
        feed(&mut s, "one\r\ntwo\r\nthree");
        assert_eq!(s.to_text(), "two\nthree");
        let history: Vec<String> = s
            .scrollback()
            .iter()
            .map(|l| l.plain().trim_end().to_owned())
            .collect();
        assert_eq!(history, ["one"]);
    }

    /// A character in the last column must not move the cursor down until
    /// something else is printed, or everything that fills a row exactly gains
    /// a blank line after it.
    #[test]
    fn wrapping_at_the_right_edge_is_deferred() {
        let mut s = screen(3, 4);
        feed(&mut s, "abcd");
        assert_eq!(s.cursor(), (0, 3), "still on the first row");
        assert_eq!(s.to_text(), "abcd\n\n");

        feed(&mut s, "e");
        assert_eq!(s.cursor(), (1, 1));
        assert_eq!(s.to_text(), "abcd\ne\n");
    }

    #[test]
    fn a_carriage_return_after_filling_a_row_stays_on_that_row() {
        let mut s = screen(3, 4);
        feed(&mut s, "abcd\rX");
        assert_eq!(s.to_text(), "Xbcd\n\n");
    }

    // ---- the alternate screen ---------------------------------------------

    /// The switch a full-screen program makes on the way in, and what makes it
    /// possible to leave one and find the shell as it was.
    #[test]
    fn the_alternate_screen_is_separate_and_the_primary_survives_it() {
        let mut s = screen(3, 8);
        feed(&mut s, "shell\r\n");
        assert!(!s.is_alternate());

        feed(&mut s, "\x1b[?1049h");
        assert!(s.is_alternate());
        feed(&mut s, "fullscreen");
        assert!(s.to_text().starts_with("fullscre"));

        feed(&mut s, "\x1b[?1049l");
        assert!(!s.is_alternate());
        assert!(
            s.to_text().starts_with("shell"),
            "the shell is where it was: {:?}",
            s.to_text()
        );
    }

    /// A program redrawing its whole window would otherwise fill the scrollback
    /// with the frames it has already replaced.
    #[test]
    fn the_alternate_screen_keeps_no_scrollback() {
        let mut s = screen(2, 6);
        feed(&mut s, "\x1b[?1049h");
        feed(&mut s, "one\r\ntwo\r\nthree\r\nfour");
        assert!(s.scrollback().is_empty());
    }

    #[test]
    fn the_older_alternate_screen_spellings_work_too() {
        for code in ["47", "1047"] {
            let mut s = screen(2, 4);
            feed(&mut s, &format!("\x1b[?{code}h"));
            assert!(s.is_alternate(), "code {code}");
            feed(&mut s, &format!("\x1b[?{code}l"));
            assert!(!s.is_alternate(), "code {code}");
        }
    }

    // ---- lines and characters ---------------------------------------------

    #[test]
    fn inserting_and_deleting_lines_moves_the_rest_of_the_region() {
        let mut s = screen(4, 4);
        feed(&mut s, "aaa\r\nbbb\r\nccc");
        feed(&mut s, "\x1b[2;1H\x1b[L");
        assert_eq!(s.to_text(), "aaa\n\nbbb\nccc");

        feed(&mut s, "\x1b[2;1H\x1b[M");
        assert_eq!(s.to_text(), "aaa\nbbb\nccc\n");
    }

    #[test]
    fn inserting_and_deleting_characters_shifts_the_rest_of_the_line() {
        let mut s = screen(1, 8);
        feed(&mut s, "abcdef\x1b[1;3H\x1b[2@");
        assert_eq!(s.to_text(), "ab  cdef");

        let mut s = screen(1, 8);
        feed(&mut s, "abcdef\x1b[1;3H\x1b[2P");
        assert_eq!(s.to_text(), "abef");
    }

    #[test]
    fn erase_characters_blanks_without_shifting() {
        let mut s = screen(1, 8);
        feed(&mut s, "abcdef\x1b[1;3H\x1b[2X");
        assert_eq!(s.to_text(), "ab  ef");
    }

    /// A scroll region is how a program keeps a header or a status line still
    /// while the middle of the screen moves.
    #[test]
    fn a_scroll_region_confines_scrolling_to_its_rows() {
        let mut s = screen(4, 4);
        feed(&mut s, "top\r\naaa\r\nbbb\r\nend");
        // Rows 2..3 only.
        feed(&mut s, "\x1b[2;3r");
        feed(&mut s, "\x1b[3;1H\r\nnew");
        assert_eq!(s.to_text(), "top\nbbb\nnew\nend");
    }

    #[test]
    fn scrolling_inside_a_region_is_not_kept_as_history() {
        let mut s = screen(4, 4);
        feed(&mut s, "\x1b[2;4r");
        feed(&mut s, "\x1b[4;1H\r\n\r\n\r\n");
        assert!(
            s.scrollback().is_empty(),
            "an inner region is animation, not history"
        );
    }

    // ---- modes and reporting ----------------------------------------------

    #[test]
    fn the_cursor_can_be_hidden_and_shown() {
        let mut s = screen(2, 4);
        assert!(s.cursor_visible());
        feed(&mut s, "\x1b[?25l");
        assert!(!s.cursor_visible());
        feed(&mut s, "\x1b[?25h");
        assert!(s.cursor_visible());
    }

    #[test]
    fn bracketed_paste_is_reported_when_asked_for() {
        let mut s = screen(2, 4);
        assert!(!s.bracketed_paste());
        feed(&mut s, "\x1b[?2004h");
        assert!(s.bracketed_paste());
        feed(&mut s, "\x1b[?2004l");
        assert!(!s.bracketed_paste());
    }

    /// readline and every full-screen program turns this on, and it changes
    /// what the arrow keys have to send.
    #[test]
    fn application_cursor_mode_is_tracked() {
        let mut s = screen(2, 4);
        assert!(!s.application_cursor());
        feed(&mut s, "[?1h");
        assert!(s.application_cursor());
        feed(&mut s, "[?1l");
        assert!(!s.application_cursor());
    }

    #[test]
    fn the_window_title_is_picked_up() {
        let mut s = screen(2, 4);
        feed(&mut s, "\x1b]0;my title\x07");
        assert_eq!(s.title(), Some("my title"));
    }

    #[test]
    fn saving_and_restoring_the_cursor_round_trips() {
        let mut s = screen(4, 8);
        feed(&mut s, "\x1b[3;5H\x1b7");
        feed(&mut s, "\x1b[1;1H");
        feed(&mut s, "\x1b8");
        assert_eq!(s.cursor(), (2, 4));
    }

    // ---- colour ------------------------------------------------------------

    #[test]
    fn colours_are_kept_per_cell() {
        let mut s = screen(1, 6);
        feed(&mut s, "\x1b[31mab\x1b[0mcd");
        let line = &s.visible_lines()[0];
        assert_eq!(line.runs[0].text, "ab");
        assert_eq!(line.runs[0].style.foreground, Some(Colour::Indexed(1)));
        assert!(line.runs[1].text.starts_with("cd"));
        assert_eq!(line.runs[1].style.foreground, None);
    }

    #[test]
    fn true_colour_and_indexed_extended_colour_are_understood() {
        let mut s = screen(1, 4);
        feed(&mut s, "\x1b[38;2;10;20;30mx");
        assert_eq!(
            s.visible_lines()[0].runs[0].style.foreground,
            Some(Colour::Rgb(10, 20, 30))
        );

        let mut s = screen(1, 4);
        feed(&mut s, "\x1b[38;5;200my");
        assert_eq!(
            s.visible_lines()[0].runs[0].style.foreground,
            Some(Colour::Indexed(200))
        );
    }

    // ---- resizing ----------------------------------------------------------

    #[test]
    fn resizing_keeps_what_still_fits_and_clamps_the_cursor() {
        let mut s = screen(4, 8);
        feed(&mut s, "hello\r\nworld");
        s.resize(2, 4);
        assert_eq!(s.size(), (2, 4));
        assert_eq!(s.to_text(), "hell\nworl");
        let (row, col) = s.cursor();
        assert!(row < 2 && col < 4, "cursor {row},{col} is off the grid");
    }

    #[test]
    fn a_resize_to_nothing_still_leaves_a_usable_grid() {
        let mut s = screen(4, 8);
        s.resize(0, 0);
        assert_eq!(s.size(), (1, 1));
        feed(&mut s, "x");
        assert_eq!(s.to_text(), "x");
    }

    /// Nothing in here may panic on a stream of arbitrary bytes: a program that
    /// writes a half-finished escape sequence and exits must not take the
    /// editor with it.
    #[test]
    fn malformed_and_partial_escapes_are_survived() {
        let mut s = screen(4, 8);
        for junk in [
            "\x1b[",
            "\x1b[999999;999999H",
            "\x1b[;;;;m",
            "\x1b]",
            "\x1b",
            "\x1b[?",
            "\x1b[99999L",
            "\x1b[0r",
            "\x1b[8;2r\x1b[MMMM",
        ] {
            feed(&mut s, junk);
        }
        s.feed(&[0x00, 0xff, 0xfe, 0x1b, 0x5b, 0x41]);
        // Still usable afterwards.
        feed(&mut s, "\x1b[1;1Hok");
        assert!(s.to_text().starts_with("ok"));
    }
}
