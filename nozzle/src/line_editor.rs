//! Line editor for Nozzle shell
//!
//! Provides a full-featured line editor with:
//! - Backspace and character insertion
//! - Cursor movement (left, right, home, end)
//! - Command history (up, down arrows)
//! - Ctrl+U (clear line), Ctrl+C (cancel)
//! - Delete key

use crate::terminal::Terminal;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::string::ToString;

const HISTORY_MAX: usize = 128;

/// Line editor state
pub struct LineEditor {
    /// Current input buffer
    buffer: alloc::vec::Vec<u8>,
    /// Cursor position within the buffer
    cursor: usize,
    /// Command history (most recent at front)
    history: VecDeque<String>,
    /// Index into history being browsed (None = fresh input)
    browsing: Option<usize>,
    /// Saved input while browsing history
    saved_line: String,
    /// Maximum line length
    max_len: usize,
}

impl LineEditor {
    /// Create a new line editor with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    /// Create a new line editor with a specific maximum line length.
    pub fn with_capacity(max_len: usize) -> Self {
        Self {
            buffer: alloc::vec::Vec::with_capacity(max_len),
            cursor: 0,
            history: VecDeque::with_capacity(HISTORY_MAX),
            browsing: None,
            saved_line: String::new(),
            max_len,
        }
    }

    /// Read one line of input.
    ///
    /// Returns `None` on Ctrl+C / Ctrl+D (cancel / EOF),
    /// or `Some(line)` on Enter.
    pub fn read_line(&mut self, term: &mut dyn Terminal) -> Option<String> {
        self.buffer.clear();
        self.cursor = 0;
        self.browsing = None;

        loop {
            match term.read_byte() {
                None => return None, // end of input (EOF)
                Some(b'\n') | Some(b'\r') => {
                    // Enter — finish line
                    term.write_str("\n");
                    break;
                }
                Some(0x08) | Some(0x7F) => {
                    // Backspace
                    self.do_backspace(term);
                }
                Some(0x1B) => {
                    // Escape sequence
                    self.handle_escape(term);
                }
                Some(0x15) => {
                    // Ctrl+U — clear whole line
                    self.do_clear_line(term);
                }
                Some(0x03) => {
                    // Ctrl+C — cancel
                    self.do_clear_line(term);
                    term.write_str("^C\n");
                    return None;
                }
                Some(0x04) => {
                    // Ctrl+D — EOF on empty line
                    if self.buffer.is_empty() {
                        return None;
                    }
                    // otherwise ignore
                }
                Some(ch) if ch.is_ascii_graphic() || ch == b' ' => {
                    // Printable character
                    if self.buffer.len() < self.max_len {
                        self.do_insert(ch, term);
                    }
                }
                Some(0x09) => {
                    // Tab — ignore for now
                }
                _ => {}
            }
        }

        let line = String::from_utf8_lossy(&self.buffer).to_string();
        self.add_to_history(&line);
        Some(line)
    }

    // ── editing primitives ──────────────────────────────────────────

    /// Insert a character at the cursor position.
    fn do_insert(&mut self, ch: u8, term: &mut dyn Terminal) {
        if self.cursor >= self.buffer.len() {
            // Append at end
            self.buffer.push(ch);
            self.cursor = self.buffer.len();
            let buf = [ch];
            term.write_str(core::str::from_utf8(&buf).unwrap_or("?"));
        } else {
            self.buffer.insert(self.cursor, ch);
            self.cursor += 1;

            // Redraw from the insertion point (including the new character) and reposition
            let tail = &self.buffer[self.cursor - 1..];
            let s = core::str::from_utf8(tail).unwrap_or("?");
            term.write_str(s);
            for _ in 0..tail.len() - 1 {
                term.write_str("\x08");
            }
        }
    }

    /// Delete the character before the cursor.
    fn do_backspace(&mut self, term: &mut dyn Terminal) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.buffer.remove(self.cursor);

        // Move terminal cursor back to deletion point
        term.write_str("\x08");

        // Redraw from cursor, leaving a space to erase the old last char
        let tail = &self.buffer[self.cursor..];
        let tail_len = tail.len();
        for _ in 0..tail_len + 1 {
            term.write_str(" ");
        }
        for _ in 0..tail_len + 1 {
            term.write_str("\x08");
        }

        let tail_str = core::str::from_utf8(tail).unwrap_or("?");
        term.write_str(tail_str);
        for _ in 0..tail_len {
            term.write_str("\x08");
        }
    }

    /// Delete character at cursor (Delete key).
    fn do_delete(&mut self, term: &mut dyn Terminal) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.buffer.remove(self.cursor);

        let tail = &self.buffer[self.cursor..];
        let tail_len = tail.len();
        // Overwrite displayed tail with spaces, then rewrite
        for _ in 0..tail_len + 1 {
            term.write_str(" ");
        }
        for _ in 0..tail_len + 1 {
            term.write_str("\x08");
        }
        let tail_str = core::str::from_utf8(tail).unwrap_or("?");
        term.write_str(tail_str);
        for _ in 0..tail_len {
            term.write_str("\x08");
        }
    }

    /// Move cursor left.
    fn do_cursor_left(&mut self, term: &mut dyn Terminal) {
        if self.cursor > 0 {
            self.cursor -= 1;
            term.write_str("\x08");
        }
    }

    /// Move cursor right.
    fn do_cursor_right(&mut self, term: &mut dyn Terminal) {
        if self.cursor < self.buffer.len() {
            let byte = self.buffer[self.cursor];
            let buf = [byte];
            let s = core::str::from_utf8(&buf).unwrap_or("?");
            term.write_str(s);
            self.cursor += 1;
        }
    }

    /// Move cursor to the beginning of the line.
    fn do_home(&mut self, term: &mut dyn Terminal) {
        while self.cursor > 0 {
            self.do_cursor_left(term);
        }
    }

    /// Move cursor to the end of the line.
    fn do_end(&mut self, term: &mut dyn Terminal) {
        while self.cursor < self.buffer.len() {
            self.do_cursor_right(term);
        }
    }

    /// Clear the entire line.
    fn do_clear_line(&mut self, term: &mut dyn Terminal) {
        let len = self.buffer.len();
        // Move cursor to start
        self.do_home(term);
        // Overwrite with spaces
        for _ in 0..len {
            term.write_str(" ");
        }
        // Move cursor back
        for _ in 0..len {
            term.write_str("\x08");
        }
        self.buffer.clear();
        self.cursor = 0;
    }

    // ── history ─────────────────────────────────────────────────────

    fn history_up(&mut self, term: &mut dyn Terminal) {
        let idx = match self.browsing {
            None => {
                if self.history.is_empty() {
                    return;
                }
                self.saved_line = String::from_utf8_lossy(&self.buffer).to_string();
                0
            }
            Some(i) if i + 1 < self.history.len() => i + 1,
            _ => return,
        };
        self.browsing = Some(idx);
        let text = self.history[idx].clone();
        self.replace_buffer(&text, term);
    }

    fn history_down(&mut self, term: &mut dyn Terminal) {
        let idx = match self.browsing {
            None => return,
            Some(0) => {
                self.browsing = None;
                let text = self.saved_line.clone();
                self.replace_buffer(&text, term);
                return;
            }
            Some(i) => i - 1,
        };
        self.browsing = Some(idx);
        let text = self.history[idx].clone();
        self.replace_buffer(&text, term);
    }

    /// Replace the current buffer with a new string and redraw.
    fn replace_buffer(&mut self, new_text: &str, term: &mut dyn Terminal) {
        let old_len = self.buffer.len();
        // Move cursor to start
        self.do_home(term);
        // Overwrite old content with spaces
        for _ in 0..old_len {
            term.write_str(" ");
        }
        // Move cursor back
        for _ in 0..old_len {
            term.write_str("\x08");
        }

        // Set new buffer
        self.buffer.clear();
        self.buffer.extend_from_slice(new_text.as_bytes());
        self.cursor = self.buffer.len();

        // Display new text
        if !new_text.is_empty() {
            term.write_str(new_text);
        }
    }

    /// Add a non-empty line to history, avoiding duplicates.
    fn add_to_history(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        if self.history.front().map_or(false, |h| h == line) {
            return;
        }
        if self.history.len() >= HISTORY_MAX {
            self.history.pop_back();
        }
        self.history.push_front(line.into());
    }

    // ── escape sequences ────────────────────────────────────────────

    /// Non‑blocking read with a short spin‑wait for escape sequence bytes.
    ///
    /// Avoids blocking on a standalone ESC key by only calling `read_byte()`
    /// when `input_available()` indicates data is ready.  The spin loop
    /// covers the gap between ESC and its sequence (typically <1 ms).
    fn read_byte_retry(&self, term: &mut dyn Terminal) -> Option<u8> {
        for _ in 0..100 {
            if term.input_available() {
                return term.read_byte();
            }
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
        None
    }

    fn handle_escape(&mut self, term: &mut dyn Terminal) {
        let bracket = match self.read_byte_retry(term) {
            Some(b'[') => b'[',
            Some(b'O') => b'O',
            _ => return,
        };

        let code = match self.read_byte_retry(term) {
            Some(c) => c,
            None => return,
        };

        match (bracket, code) {
            (b'[', b'A') => self.history_up(term),
            (b'[', b'B') => self.history_down(term),
            (b'[', b'C') => self.do_cursor_right(term),
            (b'[', b'D') => self.do_cursor_left(term),
            (b'[', b'H') | (b'O', b'H') => self.do_home(term),
            (b'[', b'F') | (b'O', b'F') => self.do_end(term),
            (b'[', b'3') => {
                if self.read_byte_retry(term) == Some(b'~') {
                    self.do_delete(term);
                }
            }
            _ => {}
        }
    }
}
