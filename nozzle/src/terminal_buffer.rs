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
//! # ANSI escape sequences
//!
//! `put_str()` parses CSI sequences inline:
//! - SGR (`m`)  — set foreground / background colour, reset
//! - CUP (`H`)  — cursor position
//! - CUU/CUD/CUF/CUB (`A`/`B`/`C`/`D`) — cursor movement
//! - ED (`J`)   — erase in display
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
            fg: 0xCCCCCC, // light gray
            bg: 0x000000, // black
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
            fg: 0xCCCCCC, // light gray
            bg: 0x000000, // black
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

    pub fn cols(&self) -> u32 {
        self.cols
    }
    pub fn rows(&self) -> u32 {
        self.rows
    }
    pub fn cursor_col(&self) -> u32 {
        self.cursor_col
    }
    pub fn cursor_row(&self) -> u32 {
        self.cursor_row
    }
    pub fn fg(&self) -> u32 {
        self.style.fg
    }
    pub fn bg(&self) -> u32 {
        self.style.bg
    }
    pub fn style(&self) -> TextStyle {
        self.style
    }

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

    pub fn set_fg(&mut self, color: u32) {
        self.style.fg = color;
    }
    pub fn set_bg(&mut self, color: u32) {
        self.style.bg = color;
    }
    pub fn set_style(&mut self, style: TextStyle) {
        self.style = style;
    }

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
            self.cells[idx] = Cell {
                ch,
                fg: self.style.fg,
                bg: self.style.bg,
            };
        }
        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.newline();
        }
    }

    /// Write a string at the cursor position.
    ///
    /// Supports ANSI escape sequences:
    /// - `\x1b[0m`           — reset style
    /// - `\x1b[30m`–`\x1b[37m`  — set foreground (standard)
    /// - `\x1b[90m`–`\x1b[97m`  — set foreground (bright)
    /// - `\x1b[40m`–`\x1b[47m`  — set background (standard)
    /// - `\x1b[100m`–`\x1b[107m` — set background (bright)
    /// - `\x1b[<n>A`          — cursor up
    /// - `\x1b[<n>B`          — cursor down
    /// - `\x1b[<n>C`          — cursor forward
    /// - `\x1b[<n>D`          — cursor back
    /// - `\x1b[2J`            — clear screen
    /// - `\x1b[H` / `\x1b[;H`  — cursor home
    pub fn put_str(&mut self, s: &str) {
        #[derive(PartialEq)]
        enum AnsiState { Normal, Esc, Csi }
        let mut state = AnsiState::Normal;
        let mut param_buf: [u8; 8] = [0; 8];
        let mut param_len: usize = 0;

        for &b in s.as_bytes() {
            match state {
                AnsiState::Normal => {
                    if b == 0x1B {
                        state = AnsiState::Esc;
                    } else {
                        self.put_byte(b);
                    }
                }
                AnsiState::Esc => {
                    if b == b'[' {
                        state = AnsiState::Csi;
                        param_len = 0;
                    } else {
                        // Unknown escape — emit both bytes as-is
                        self.put_byte(0x1B);
                        self.put_byte(b);
                        state = AnsiState::Normal;
                    }
                }
                AnsiState::Csi => {
                    if b.is_ascii_digit() || b == b';' {
                        if param_len < param_buf.len() {
                            param_buf[param_len] = b;
                            param_len += 1;
                        }
                    } else {
                        // Final byte — execute CSI command
                        self.handle_csi(b, &param_buf[..param_len]);
                        state = AnsiState::Normal;
                    }
                }
            }
        }
        // Flush unterminated sequence as literal text
        match state {
            AnsiState::Esc => { self.put_byte(0x1B); }
            AnsiState::Csi => {
                self.put_byte(0x1B);
                self.put_byte(b'[');
                for &pb in &param_buf[..param_len] { self.put_byte(pb); }
            }
            AnsiState::Normal => {}
        }
    }

    /// Write a single raw byte (no ANSI processing).
    fn put_byte(&mut self, b: u8) {
        match b {
            b'\n' => self.newline(),
            0x08 => self.backspace(),
            b'\r' => self.cursor_col = 0,
            ch if ch >= 0x20 => self.put_char(ch),
            _ => {}
        }
    }

    /// Execute a single CSI (Control Sequence Introducer) command.
    fn handle_csi(&mut self, final_byte: u8, params: &[u8]) {
        // Parse semicolon-separated numeric parameters.
        // Empty or missing parameters default to 0 for SGR,
        // but cursor commands use 1 as default.
        let param_str = core::str::from_utf8(params).unwrap_or("");
        let mut nums: [u32; 8] = [0; 8];
        let mut ni = 0;
        if !param_str.is_empty() {
            for part in param_str.split(';') {
                if ni >= 8 { break; }
                nums[ni] = part.parse::<u32>().unwrap_or(0);
                ni += 1;
            }
        }

        match final_byte {
            b'A' => {
                // Cursor up (default 1)
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            b'B' => {
                // Cursor down (default 1)
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_row = self.cursor_row.saturating_add(n).min(self.rows.saturating_sub(1));
            }
            b'C' => {
                // Cursor forward (default 1)
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_col = self.cursor_col.saturating_add(n).min(self.cols.saturating_sub(1));
            }
            b'D' => {
                // Cursor back (default 1)
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            b'H' => {
                // Cursor position (row;col, 1-based, default 1)
                let row = if ni > 0 { nums[0].saturating_sub(1) } else { 0 };
                let col = if ni > 1 { nums[1].saturating_sub(1) } else { 0 };
                self.cursor_row = row.min(self.rows.saturating_sub(1));
                self.cursor_col = col.min(self.cols.saturating_sub(1));
            }
            b'J' => {
                // Erase in display (default 0)
                let mode = if ni > 0 { nums[0] } else { 0 };
                if mode == 2 {
                    self.clear();
                }
            }
            b'm' => {
                // SGR — Select Graphic Rendition (default 0 = reset)
                if ni == 0 {
                    self.handle_sgr(&[0]);
                } else {
                    self.handle_sgr(&nums[..ni]);
                }
            }
            _ => {} // Unknown — silently ignore
        }
    }

    /// Map standard ANSI colour (30–37 / 40–47) → 0xRRGGBB.
    fn ansi_color(code: u32) -> u32 {
        match code {
            30 => 0x000000, // Black
            31 => 0xCC0000, // Red
            32 => 0x00CC00, // Green
            33 => 0xCCCC00, // Yellow
            34 => 0x0000CC, // Blue
            35 => 0xCC00CC, // Magenta
            36 => 0x00CCCC, // Cyan
            37 => 0xCCCCCC, // White
            _ => 0xCCCCCC,
        }
    }

    /// Map bright ANSI colour (90–97 / 100–107) → 0xRRGGBB.
    fn ansi_bright_color(code: u32) -> u32 {
        match code {
            0 => 0x555555, // Bright Black (gray)
            1 => 0xFF5555, // Bright Red
            2 => 0x55FF55, // Bright Green
            3 => 0xFFFF55, // Bright Yellow
            4 => 0x5555FF, // Bright Blue
            5 => 0xFF55FF, // Bright Magenta
            6 => 0x55FFFF, // Bright Cyan
            7 => 0xFFFFFF, // Bright White
            _ => 0xFFFFFF,
        }
    }

    /// Map 256-colour palette index → 0xRRGGBB.
    fn ansi_256_color(idx: u8) -> u32 {
        match idx {
            0..=15 => Self::ansi_standard_256(idx),
            16..=231 => {
                // 6×6×6 colour cube
                let v = idx - 16;
                let r = (v / 36) % 6;
                let g = (v / 6) % 6;
                let b = v % 6;
                let scale = |c: u8| if c == 0 { 0 } else { (c * 40 + 55) as u32 };
                (scale(r) << 16) | (scale(g) << 8) | scale(b)
            }
            232..=255 => {
                // Grayscale ramp
                let l = (idx - 232) as u32 * 10 + 8;
                (l << 16) | (l << 8) | l
            }
        }
    }

    /// Standard 16 ANSI colours (0–15).
    fn ansi_standard_256(idx: u8) -> u32 {
        match idx {
            0 => 0x000000, 1 => 0xCC0000, 2 => 0x00CC00, 3 => 0xCCCC00,
            4 => 0x0000CC, 5 => 0xCC00CC, 6 => 0x00CCCC, 7 => 0xCCCCCC,
            8 => 0x555555, 9 => 0xFF5555, 10 => 0x55FF55, 11 => 0xFFFF55,
            12 => 0x5555FF, 13 => 0xFF55FF, 14 => 0x55FFFF, 15 => 0xFFFFFF,
            _ => 0xCCCCCC,
        }
    }

    /// Apply SGR (Select Graphic Rendition) parameters.
    fn handle_sgr(&mut self, codes: &[u32]) {
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            match c {
                0 => {
                    // Reset
                    self.style = TextStyle::default();
                }
                1 => {
                    // Bold — use bright version of current fg
                    // (simplified: no-op for now)
                }
                30..=37 => {
                    self.style.fg = Self::ansi_color(c);
                }
                38 => {
                    // Extended foreground: 38;5;<n> or 38;2;<r>;<g>;<b>
                    if i + 2 < codes.len() && codes[i + 1] == 5 {
                        self.style.fg = Self::ansi_256_color(codes[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < codes.len() && codes[i + 1] == 2 {
                        self.style.fg =
                            ((codes[i + 2] & 0xFF) << 16)
                            | ((codes[i + 3] & 0xFF) << 8)
                            | (codes[i + 4] & 0xFF);
                        i += 4;
                    }
                }
                40..=47 => {
                    self.style.bg = Self::ansi_color(c - 10);
                }
                48 => {
                    // Extended background
                    if i + 2 < codes.len() && codes[i + 1] == 5 {
                        self.style.bg = Self::ansi_256_color(codes[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < codes.len() && codes[i + 1] == 2 {
                        self.style.bg =
                            ((codes[i + 2] & 0xFF) << 16)
                            | ((codes[i + 3] & 0xFF) << 8)
                            | (codes[i + 4] & 0xFF);
                        i += 4;
                    }
                }
                90..=97 => {
                    self.style.fg = Self::ansi_bright_color(c - 90);
                }
                100..=107 => {
                    self.style.bg = Self::ansi_bright_color(c - 100);
                }
                _ => {}
            }
            i += 1;
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

    /// Clear the screen (fills all cells using the current style's background).
    pub fn clear(&mut self) {
        let blank = Cell {
            ch: b' ',
            fg: self.style.fg,
            bg: self.style.bg,
        };
        for cell in &mut self.cells {
            *cell = blank;
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
        // Clear bottom row using the current style's background
        let bottom_start = self.cells.len() - row_len;
        let blank = Cell {
            ch: b' ',
            fg: self.style.fg,
            bg: self.style.bg,
        };
        for cell in &mut self.cells[bottom_start..] {
            *cell = blank;
        }
    }
}