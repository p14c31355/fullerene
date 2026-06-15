//! Terminal cell buffer — a portable, no_std text buffer.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

const ANSI_COLORS: [u32; 8] = [
    0x000000, 0xCC0000, 0x00CC00, 0xCCCC00, 0x0000CC, 0xCC00CC, 0x00CCCC, 0xCCCCCC,
];

const ANSI_BRIGHT_COLORS: [u32; 8] = [
    0x555555, 0xFF5555, 0x55FF55, 0xFFFF55, 0x5555FF, 0xFF55FF, 0x55FFFF, 0xFFFFFF,
];

const ANSI_STANDARD_256: [u32; 16] = [
    0x000000, 0xCC0000, 0x00CC00, 0xCCCC00, 0x0000CC, 0xCC00CC, 0x00CCCC, 0xCCCCCC, 0x555555,
    0xFF5555, 0x55FF55, 0xFFFF55, 0x5555FF, 0xFF55FF, 0x55FFFF, 0xFFFFFF,
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub fg: u32,
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
            fg: 0xCCCCCC,
            bg: 0x000000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: u8,
    pub fg: u32,
    pub bg: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: b' ',
            fg: 0xCCCCCC,
            bg: 0x000000,
        }
    }
}

pub struct TerminalBuffer {
    cells: Vec<Cell>,
    cols: u32,
    rows: u32,
    cursor_col: u32,
    cursor_row: u32,
    style: TextStyle,
    /// Scrollback buffer: rows that have scrolled off the top of the screen.
    /// Each entry is a full row of `Cell`s.
    scrollback: VecDeque<Vec<Cell>>,
    /// Current scroll offset into the scrollback buffer (0 = normal view).
    scroll_offset: usize,
}

impl TerminalBuffer {
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
            scrollback: VecDeque::new(),
            scroll_offset: 0,
        }
    }

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
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cell_mut(&mut self, col: u32, row: u32) -> Option<&mut Cell> {
        if col < self.cols && row < self.rows {
            let idx = row as usize * self.cols as usize + col as usize;
            self.cells.get_mut(idx)
        } else {
            None
        }
    }

    pub fn set_fg(&mut self, color: u32) {
        self.style.fg = color;
    }
    pub fn set_bg(&mut self, color: u32) {
        self.style.bg = color;
    }
    pub fn set_style(&mut self, style: TextStyle) {
        self.style = style;
    }

    pub fn set_cursor(&mut self, col: u32, row: u32) {
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        self.cursor_row = row.min(self.rows.saturating_sub(1));
    }

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

    pub fn put_str(&mut self, s: &str) {
        #[derive(PartialEq)]
        enum AnsiState {
            Normal,
            Esc,
            Csi,
        }
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
                        self.put_byte(0x1B);
                        self.put_byte(b);
                        state = AnsiState::Normal;
                    }
                }
                AnsiState::Csi => {
                    if (0x30..=0x3F).contains(&b) {
                        if param_len < param_buf.len() {
                            param_buf[param_len] = b;
                            param_len += 1;
                        }
                    } else {
                        self.handle_csi(b, &param_buf[..param_len]);
                        state = AnsiState::Normal;
                    }
                }
            }
        }
        match state {
            AnsiState::Esc => {
                self.put_byte(0x1B);
            }
            AnsiState::Csi => {
                self.put_byte(0x1B);
                self.put_byte(b'[');
                for &pb in &param_buf[..param_len] {
                    self.put_byte(pb);
                }
            }
            AnsiState::Normal => {}
        }
    }

    fn put_byte(&mut self, b: u8) {
        match b {
            b'\n' => self.newline(),
            0x08 => self.backspace(),
            b'\r' => self.cursor_col = 0,
            ch if ch >= 0x20 => self.put_char(ch),
            _ => {}
        }
    }

    fn handle_csi(&mut self, final_byte: u8, params: &[u8]) {
        let param_str = core::str::from_utf8(params).unwrap_or("");
        let mut nums: [u32; 8] = [0; 8];
        let mut ni = 0;
        if !param_str.is_empty() {
            for part in param_str.split(';') {
                if ni >= 8 {
                    break;
                }
                nums[ni] = part.parse::<u32>().unwrap_or(0);
                ni += 1;
            }
        }

        match final_byte {
            b'A' => {
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            b'B' => {
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_row = self
                    .cursor_row
                    .saturating_add(n)
                    .min(self.rows.saturating_sub(1));
            }
            b'C' => {
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_col = self
                    .cursor_col
                    .saturating_add(n)
                    .min(self.cols.saturating_sub(1));
            }
            b'D' => {
                let n = if ni > 0 { nums[0].max(1) } else { 1 };
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            b'H' => {
                let row = if ni > 0 { nums[0].saturating_sub(1) } else { 0 };
                let col = if ni > 1 { nums[1].saturating_sub(1) } else { 0 };
                self.cursor_row = row.min(self.rows.saturating_sub(1));
                self.cursor_col = col.min(self.cols.saturating_sub(1));
            }
            b'J' => {
                let mode = if ni > 0 { nums[0] } else { 0 };
                if mode == 2 {
                    self.clear();
                }
            }
            b'm' => {
                if ni == 0 {
                    self.handle_sgr(&[0]);
                } else {
                    self.handle_sgr(&nums[..ni]);
                }
            }
            _ => {}
        }
    }

    fn ansi_256_color(idx: u8) -> u32 {
        match idx {
            0..=15 => ANSI_STANDARD_256[idx as usize],
            16..=231 => {
                let v = idx - 16;
                let scale = |c: u8| if c == 0 { 0 } else { (c * 40 + 55) as u32 };
                (scale((v / 36) % 6) << 16) | (scale((v / 6) % 6) << 8) | scale(v % 6)
            }
            232..=255 => {
                let l = (idx - 232) as u32 * 10 + 8;
                (l << 16) | (l << 8) | l
            }
        }
    }

    fn handle_sgr(&mut self, codes: &[u32]) {
        let mut i = 0;
        while i < codes.len() {
            let c = codes[i];
            match c {
                0 => self.style = TextStyle::default(),
                30..=37 => self.style.fg = ANSI_COLORS[(c - 30) as usize],
                38 => {
                    if i + 2 < codes.len() && codes[i + 1] == 5 {
                        self.style.fg = Self::ansi_256_color(codes[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < codes.len() && codes[i + 1] == 2 {
                        self.style.fg = ((codes[i + 2] & 0xFF) << 16)
                            | ((codes[i + 3] & 0xFF) << 8)
                            | (codes[i + 4] & 0xFF);
                        i += 4;
                    }
                }
                40..=47 => self.style.bg = ANSI_COLORS[(c - 40) as usize],
                48 => {
                    if i + 2 < codes.len() && codes[i + 1] == 5 {
                        self.style.bg = Self::ansi_256_color(codes[i + 2] as u8);
                        i += 2;
                    } else if i + 4 < codes.len() && codes[i + 1] == 2 {
                        self.style.bg = ((codes[i + 2] & 0xFF) << 16)
                            | ((codes[i + 3] & 0xFF) << 8)
                            | (codes[i + 4] & 0xFF);
                        i += 4;
                    }
                }
                90..=97 => self.style.fg = ANSI_BRIGHT_COLORS[(c - 90) as usize],
                100..=107 => self.style.bg = ANSI_BRIGHT_COLORS[(c - 100) as usize],
                _ => {}
            }
            i += 1;
        }
    }

    pub fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll();
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            let idx = self.cursor_row as usize * self.cols as usize + self.cursor_col as usize;
            if idx < self.cells.len() {
                self.cells[idx] = Cell::default();
            }
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cols.saturating_sub(1);
        }
    }

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

    pub fn scroll(&mut self) {
        let row_len = self.cols as usize;
        if self.cells.len() <= row_len {
            let blank = Cell {
                ch: b' ',
                fg: self.style.fg,
                bg: self.style.bg,
            };
            self.cells.fill(blank);
            return;
        }
        // Save the top row into scrollback before shifting.
        let saved_row: Vec<Cell> = self.cells[..row_len].to_vec();
        self.scrollback.push_back(saved_row);
        // Limit scrollback to 10000 lines to avoid unbounded memory growth.
        if self.scrollback.len() > 10000 {
            self.scrollback.remove(0);
        }
        self.cells.copy_within(row_len.., 0);
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

    // ── Scrollback navigation ─────────────────────────────────

    /// Get the number of lines currently in the scrollback buffer.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Get the current scroll offset (0 = normal view).
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Scroll back by `n` lines.  Clamped to scrollback length.
    pub fn scroll_back(&mut self, n: usize) {
        self.scroll_offset = (self.scroll_offset + n).min(self.scrollback.len());
    }

    /// Scroll forward by `n` lines.
    pub fn scroll_forward(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Reset scroll offset to normal view (bottom of buffer).
    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
    }

    /// Get the effective visible cells, taking scroll offset into account.
    ///
    /// When `scroll_offset > 0`, returns a view that mixes scrollback
    /// rows with current buffer rows.  When `scroll_offset == 0`,
    /// returns a slice of the current cells (same as `self.cells()`).
    pub fn visible_cells(&self) -> alloc::vec::Vec<Cell> {
        let total_rows = self.rows as usize;
        let sb_len = self.scrollback.len();
        if self.scroll_offset == 0 || sb_len == 0 {
            return self.cells.clone();
        }
        let row_len = self.cols as usize;
        let start = sb_len.saturating_sub(self.scroll_offset);
        // Limit scrollback rows to at most total_rows so the returned
        // vector never exceeds total_rows * row_len elements.
        let num_sb_rows = self.scroll_offset.min(total_rows);

        let mut result = Vec::with_capacity(total_rows * row_len);
        // Add scrollback rows from the offset.
        for i in start..(start + num_sb_rows) {
            if i < sb_len {
                let row = &self.scrollback[i];
                result.extend_from_slice(row);
            }
        }
        // Fill remaining rows from the current buffer.
        let remaining = total_rows.saturating_sub(num_sb_rows);
        let buf_cells = remaining * row_len;
        let buf_slice = if buf_cells <= self.cells.len() {
            &self.cells[..buf_cells]
        } else {
            &self.cells[..]
        };
        result.extend_from_slice(buf_slice);
        // Pad if necessary.
        while result.len() < total_rows * row_len {
            result.push(Cell::default());
        }
        result
    }
}
