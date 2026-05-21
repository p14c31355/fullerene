//! Terminal cell buffer — a portable, no_std text buffer.
//!
//! `TerminalBuffer` stores a grid of [`Cell`]s and implements:
//!
//! - `put_char` / `put_str`  — write text at the cursor
//! - `newline`              — advance to next row (scroll if needed)
//! - `backspace`            — delete character before cursor
//! - `scroll`               — push lines up, bottom row clears
//! - Cursor movement        — `cursor` / `set_cursor`
//! - `cells()`              — flat slice for the renderer
//!
//! The buffer knows nothing about pixels, surfaces, or rendering.
//! A downstream renderer (e.g. Lattice's `terminal_surface`) consumes
//! `cells()` and paints glyphs.
//!
//! # ANSI colour
//!
//! `Cell` stores separate foreground / background colours.  The parser
//! or shell can set them; the renderer uses them.

extern crate alloc;

use alloc::vec::Vec;

/// Foreground / background colour pair for terminal styling.
///
/// Kept separate from [`Cell`] because `TextStyle` represents the
/// *current* colour state of the terminal, while `Cell` stores the
/// colours of an already‑written cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Foreground colour (0xRRGGBB).
    pub fg: u32,
    /// Background colour (0xRRGGBB).
    pub bg: u32,
}

impl TextStyle {
    pub const fn new(fg: u32, bg: u32) -> Self {
        Self { fg, bg }
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            fg: 0xCCCCCC,  // light gray
            bg: 0x000000,  // black
        }
    }
}

/// A single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// The character to display.
    pub ch: u8,
    /// Foreground colour (0xRRGGBB).
    pub fg: u32,
    /// Background colour (0xRRGGBB).
    pub bg: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: b' ',
            fg: 0xCCCCCC,  // light gray
            bg: 0x000000,  // black
        }
    }
}

/// A scrollable terminal cell buffer.
pub struct TerminalBuffer {
    /// Flat array of cells, row‑major.
    cells: Vec<Cell>,
    /// Columns (characters per row).
    cols: u32,
    /// Rows (visible lines).
    rows: u32,
    /// Cursor column.
    cursor_col: u32,
    /// Cursor row (0 = top).
    cursor_row: u32,
    /// Current text style (foreground / background).
    style: TextStyle,
}

impl TerminalBuffer {
    /// Create a new buffer with `cols × rows` cells.
    pub fn new(cols: u32, rows: u32) -> Self {
        let len = (cols as usize).saturating_mul(rows as usize);
        let mut cells = Vec::with_capacity(len);
        cells.resize(len, Cell::default());
        Self {
            cells,
            cols,
            rows,
            cursor_col: 0,
            cursor_row: 0,
            style: TextStyle::default(),
        }
    }

    // ── accessors ────────────────────────────────────────────

    pub fn cols(&self) -> u32 { self.cols }
    pub fn rows(&self) -> u32 { self.rows }
    pub fn cursor_col(&self) -> u32 { self.cursor_col }
    pub fn cursor_row(&self) -> u32 { self.cursor_row }
    pub fn fg(&self) -> u32 { self.style.fg }
    pub fn bg(&self) -> u32 { self.style.bg }
    pub fn style(&self) -> TextStyle { self.style }

    /// Flat slice of all cells (row‑major).
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Mutable access to a single cell at `(col, row)`.
    ///
    /// Returns `None` if the coordinates are out of bounds.
    /// This is the **only** way for external code to mutate cells —
    /// prefer this over exposing the full mutable slice.
    pub fn cell_mut(&mut self, col: u32, row: u32) -> Option<&mut Cell> {
        if col < self.cols && row < self.rows {
            let idx = row as usize * self.cols as usize + col as usize;
            self.cells.get_mut(idx)
        } else {
            None
        }
    }

    // ── colour ───────────────────────────────────────────────

    pub fn set_fg(&mut self, color: u32) { self.style.fg = color; }
    pub fn set_bg(&mut self, color: u32) { self.style.bg = color; }
    pub fn set_style(&mut self, style: TextStyle) { self.style = style; }

    // ── cursor ───────────────────────────────────────────────

    pub fn set_cursor(&mut self, col: u32, row: u32) {
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        self.cursor_row = row.min(self.rows.saturating_sub(1));
    }

    // ── writing ──────────────────────────────────────────────

    /// Write a single character at the cursor position.
    pub fn put_char(&mut self, ch: u8) {
        let idx = self.cursor_row as usize * self.cols as usize + self.cursor_col as usize;
        if idx < self.cells.len() {
            self.cells[idx] = Cell { ch, fg: self.style.fg, bg: self.style.bg };
        }
        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.newline();
        }
    }

    /// Write a string at the cursor position.
    pub fn put_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            match b {
                b'\n' => self.newline(),
                0x08  => self.backspace(),   // backspace
                b'\r' => self.cursor_col = 0,
                ch    if ch >= 0x20 => self.put_char(ch),
                _    => {}                    // ignore other control chars
            }
        }
    }

    /// Advance to the next line (scroll if on the last row).
    pub fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll();
        }
    }

    /// Delete character before cursor and move cursor left.
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            let idx = self.cursor_row as usize * self.cols as usize + self.cursor_col as usize;
            if idx < self.cells.len() {
                self.cells[idx] = Cell::default();
            }
        } else if self.cursor_row > 0 {
            // Move to end of previous row
            self.cursor_row -= 1;
            self.cursor_col = self.cols.saturating_sub(1);
        }
    }

    /// Clear the screen (fills all cells with default).
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::default();
        }
        self.cursor_col = 0;
        self.cursor_row = 0;
    }

    // ── scrolling ────────────────────────────────────────────

    /// Scroll contents up by one row.  Bottom row is cleared.
    pub fn scroll(&mut self) {
        let row_len = self.cols as usize;
        if self.cells.len() <= row_len {
            self.cells.fill(Cell::default());
            return;
        }
        // Shift rows up
        let src_start = row_len;
        let _copy_count = self.cells.len() - row_len;
        self.cells.copy_within(src_start.., 0);
        // Clear bottom row
        let bottom_start = self.cells.len() - row_len;
        for cell in &mut self.cells[bottom_start..] {
            *cell = Cell::default();
        }
    }
}