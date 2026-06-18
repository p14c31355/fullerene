//! Text editor buffer engine for Toluene SDK.
//!
//! Provides a multi-line text buffer with cursor movement,
//! insert/delete operations, and viewport scrolling.
//! Designed for embedding in a windowed GUI editor application.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// A single text buffer row.
#[derive(Debug, Clone)]
pub struct Row {
    /// Raw bytes of the line (no newline character).
    pub data: Vec<u8>,
}

impl Row {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    pub fn from_str(s: &str) -> Self {
        Self {
            data: s.as_bytes().to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.data).unwrap_or("")
    }

    pub fn insert(&mut self, col: usize, ch: u8) {
        let pos = col.min(self.data.len());
        self.data.insert(pos, ch);
    }

    pub fn remove(&mut self, col: usize) {
        if col < self.data.len() {
            self.data.remove(col);
        }
    }

    pub fn split_off(&mut self, col: usize) -> Self {
        let tail = self.data.split_off(col.min(self.data.len()));
        Self { data: tail }
    }

    pub fn append(&mut self, other: &Row) {
        self.data.extend_from_slice(&other.data);
    }
}

/// The editor buffer: a list of rows, cursor position, and viewport offset.
#[derive(Debug, Clone)]
pub struct EditorBuffer {
    /// All text rows.
    pub rows: Vec<Row>,
    /// Cursor row index (0-based).
    pub cursor_row: usize,
    /// Cursor column index (0-based, in bytes).
    pub cursor_col: usize,
    /// First visible row in the viewport (for scrolling).
    pub scroll_row: usize,
    /// Whether the buffer has been modified.
    pub dirty: bool,
}

impl EditorBuffer {
    /// Create an empty editor buffer.
    pub fn new() -> Self {
        Self {
            rows: alloc::vec![Row::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            dirty: false,
        }
    }

    /// Create a buffer pre-filled with text lines.
    pub fn from_text(text: &str) -> Self {
        let rows: Vec<Row> = text.lines().map(Row::from_str).collect();
        let has_trailing_newline = text.ends_with('\n');
        let mut buf = Self {
            rows,
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            dirty: false,
        };
        if buf.rows.is_empty() || has_trailing_newline {
            buf.rows.push(Row::new());
        }
        buf
    }

    /// Total number of rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Get a reference to a row by index.
    pub fn row(&self, idx: usize) -> Option<&Row> {
        self.rows.get(idx)
    }

    /// Current cursor row reference.
    pub fn current_row(&self) -> &Row {
        &self.rows[self.cursor_row]
    }

    /// Current cursor row mutable reference.
    pub fn current_row_mut(&mut self) -> &mut Row {
        &mut self.rows[self.cursor_row]
    }

    // ── Cursor movement ─────────────────────────────────────

    /// Move cursor left by one UTF-8 character.
    pub fn cursor_left(&mut self) {
        if self.cursor_col > 0 {
            let row = &self.rows[self.cursor_row];
            let s = row.as_str();
            self.cursor_col = s
                .char_indices()
                .map(|(idx, _)| idx)
                .filter(|&idx| idx < self.cursor_col)
                .last()
                .unwrap_or(0);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.rows[self.cursor_row].len();
        }
        self.clamp_scroll();
    }

    /// Move cursor right by one UTF-8 character.
    pub fn cursor_right(&mut self) {
        let row = &self.rows[self.cursor_row];
        let s = row.as_str();
        if self.cursor_col < s.len() {
            self.cursor_col = s
                .char_indices()
                .map(|(idx, _)| idx)
                .find(|&idx| idx > self.cursor_col)
                .unwrap_or(s.len());
        } else if self.cursor_row + 1 < self.rows.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.clamp_scroll();
    }

    /// Move cursor up by one row.
    pub fn cursor_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let row_len = self.rows[self.cursor_row].len();
            if self.cursor_col > row_len {
                self.cursor_col = row_len;
            }
        }
        self.clamp_scroll();
    }

    /// Move cursor down by one row.
    pub fn cursor_down(&mut self) {
        if self.cursor_row + 1 < self.rows.len() {
            self.cursor_row += 1;
            let row_len = self.rows[self.cursor_row].len();
            if self.cursor_col > row_len {
                self.cursor_col = row_len;
            }
        }
        self.clamp_scroll();
    }

    /// Move cursor to beginning of line.
    pub fn cursor_home(&mut self) {
        self.cursor_col = 0;
        self.clamp_scroll();
    }

    /// Move cursor to end of line.
    pub fn cursor_end(&mut self) {
        self.cursor_col = self.rows[self.cursor_row].len();
        self.clamp_scroll();
    }

    /// Page up: move cursor up by viewport height.
    pub fn page_up(&mut self, viewport_rows: usize) {
        for _ in 0..viewport_rows {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
            } else {
                break;
            }
        }
        let row_len = self.rows[self.cursor_row].len();
        if self.cursor_col > row_len {
            self.cursor_col = row_len;
        }
        self.clamp_scroll();
    }

    /// Page down: move cursor down by viewport height.
    pub fn page_down(&mut self, viewport_rows: usize) {
        for _ in 0..viewport_rows {
            if self.cursor_row + 1 < self.rows.len() {
                self.cursor_row += 1;
            } else {
                break;
            }
        }
        let row_len = self.rows[self.cursor_row].len();
        if self.cursor_col > row_len {
            self.cursor_col = row_len;
        }
        self.clamp_scroll();
    }

    // ── Editing ─────────────────────────────────────────────

    /// Insert a byte at the current cursor position.
    pub fn insert_char(&mut self, ch: u8) {
        self.dirty = true;
        if ch == b'\n' {
            let col = self.cursor_col;
            let tail = self.rows[self.cursor_row].split_off(col);
            self.cursor_row += 1;
            self.cursor_col = 0;
            self.rows.insert(self.cursor_row, tail);
        } else {
            let row_idx = self.cursor_row;
            let col = self.cursor_col;
            self.rows[row_idx].insert(col, ch);
            self.cursor_col = col + 1;
        }
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        self.dirty = true;
        if self.cursor_col > 0 {
            let row_idx = self.cursor_row;
            self.cursor_col -= 1;
            let col = self.cursor_col;
            self.rows[row_idx].remove(col);
        } else if self.cursor_row > 0 {
            // Join with previous row
            let current_row = self.cursor_row;
            let current = self.rows.remove(current_row);
            self.cursor_row -= 1;
            self.cursor_col = self.rows[self.cursor_row].len();
            self.rows[self.cursor_row].append(&current);
        }
    }

    /// Delete the character under the cursor (delete key).
    pub fn delete_char(&mut self) {
        self.dirty = true;
        let row_len = self.rows[self.cursor_row].len();
        if self.cursor_col < row_len {
            let row_idx = self.cursor_row;
            let col = self.cursor_col;
            self.rows[row_idx].remove(col);
        } else if self.cursor_row + 1 < self.rows.len() {
            // Join with next row
            let current_row = self.cursor_row;
            let next = self.rows.remove(current_row + 1);
            self.rows[current_row].append(&next);
        }
    }

    // ── Scrolling ───────────────────────────────────────────

    /// Ensure the cursor is visible in the viewport.
    pub fn clamp_scroll(&mut self) {
        // scroll_row is the first visible row index
        if self.cursor_row < self.scroll_row {
            self.scroll_row = self.cursor_row;
        }
        // We don't know viewport_rows here, so caller should call
        // `ensure_cursor_visible(viewport_rows)` after this.
    }

    /// Ensure cursor is within [scroll_row, scroll_row + viewport_rows).
    pub fn ensure_cursor_visible(&mut self, viewport_rows: usize) {
        if viewport_rows == 0 {
            return;
        }
        let last_visible = self.scroll_row + viewport_rows - 1;
        if self.cursor_row < self.scroll_row {
            self.scroll_row = self.cursor_row;
        } else if self.cursor_row > last_visible {
            self.scroll_row = self.cursor_row.saturating_sub(viewport_rows - 1);
        }
        if self.scroll_row + viewport_rows > self.rows.len() {
            let end = self.rows.len().saturating_sub(1);
            self.scroll_row = end.saturating_sub(viewport_rows.saturating_sub(1));
        }
    }

    /// Get the visible rows (as &str) for the given viewport height.
    pub fn visible_lines(&self, viewport_rows: usize) -> Vec<&str> {
        let start = self.scroll_row.min(self.rows.len());
        let end = (start + viewport_rows).min(self.rows.len());
        self.rows[start..end].iter().map(|r| r.as_str()).collect()
    }

    /// Get the full text content as a String.
    pub fn full_text(&self) -> String {
        let mut s = String::new();
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(row.as_str());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer_is_empty() {
        let buf = EditorBuffer::new();
        assert_eq!(buf.row_count(), 1);
        assert_eq!(buf.current_row().len(), 0);
    }

    #[test]
    fn test_insert_char() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'H');
        buf.insert_char(b'i');
        assert_eq!(buf.current_row().as_str(), "Hi");
        assert_eq!(buf.cursor_col, 2);
    }

    #[test]
    fn test_insert_newline() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'a');
        buf.insert_char(b'\n');
        buf.insert_char(b'b');
        assert_eq!(buf.row_count(), 2);
        assert_eq!(buf.rows[0].as_str(), "a");
        assert_eq!(buf.rows[1].as_str(), "b");
        assert_eq!(buf.cursor_row, 1);
        assert_eq!(buf.cursor_col, 1);
    }

    #[test]
    fn test_backspace() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'a');
        buf.insert_char(b'b');
        buf.backspace();
        assert_eq!(buf.current_row().as_str(), "a");
        assert_eq!(buf.cursor_col, 1);
    }

    #[test]
    fn test_backspace_join_rows() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'a');
        buf.insert_char(b'\n');
        buf.insert_char(b'b');
        // cursor is at row 1, col 1 after 'b'
        buf.backspace(); // delete 'b'
        buf.backspace(); // join rows
        assert_eq!(buf.row_count(), 1);
        assert_eq!(buf.rows[0].as_str(), "a");
    }

    #[test]
    fn test_delete_join_rows() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'a');
        buf.insert_char(b'\n');
        buf.insert_char(b'b');
        buf.cursor_left(); // to end of 'a' line
        buf.cursor_up(); // to row 0, col 1
        buf.delete_char(); // join rows
        assert_eq!(buf.row_count(), 1);
        assert_eq!(buf.rows[0].as_str(), "ab");
    }

    #[test]
    fn test_cursor_movement() {
        let mut buf = EditorBuffer::new();
        buf.insert_char(b'a');
        buf.insert_char(b'b');
        buf.cursor_left();
        assert_eq!(buf.cursor_col, 1);
        buf.cursor_home();
        assert_eq!(buf.cursor_col, 0);
        buf.cursor_end();
        assert_eq!(buf.cursor_col, 2);
    }

    #[test]
    fn test_from_text() {
        let buf = EditorBuffer::from_text("hello\nworld\n");
        assert_eq!(buf.row_count(), 3); // trailing newline → extra empty row
        assert_eq!(buf.rows[0].as_str(), "hello");
        assert_eq!(buf.rows[1].as_str(), "world");
        assert_eq!(buf.rows[2].as_str(), "");
    }

    #[test]
    fn test_scroll() {
        let mut buf = EditorBuffer::from_text("a\nb\nc\nd\ne\n");
        buf.cursor_down();
        buf.cursor_down();
        buf.cursor_down();
        buf.cursor_down(); // cursor at row 4 (last)
        buf.ensure_cursor_visible(3); // viewport = 3 rows
        assert_eq!(buf.scroll_row, 2); // rows 2,3,4 visible
    }
}