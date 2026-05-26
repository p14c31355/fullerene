//! Line editor for Nozzle shell — with TAB completion support.

use crate::terminal::Terminal;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::string::ToString;

const HISTORY_MAX: usize = 128;

pub struct LineEditor {
    buffer: alloc::vec::Vec<u8>,
    cursor: usize,
    history: VecDeque<String>,
    browsing: Option<usize>,
    saved_line: String,
    max_len: usize,
}

impl LineEditor {
    pub fn new() -> Self { Self::with_capacity(256) }

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

    pub fn read_line(&mut self, term: &mut dyn Terminal) -> Option<String> {
        self.buffer.clear();
        self.cursor = 0;
        self.browsing = None;
        loop {
            match term.read_byte() {
                None => return None,
                Some(b'\n') | Some(b'\r') => { term.write_str("\n"); break; }
                Some(0x08) | Some(0x7F) => { self.do_backspace(term); }
                Some(0x1B) => { self.handle_escape(term); }
                Some(0x15) => { self.do_clear_line(term); }
                Some(0x03) => { self.do_clear_line(term); term.write_str("^C\n"); return None; }
                Some(0x04) => { if self.buffer.is_empty() { return None; } }
                Some(ch) if ch.is_ascii_graphic() || ch == b' ' => {
                    if self.buffer.len() < self.max_len { self.do_insert(ch, term); }
                }
                Some(0x09) => { self.do_tab_complete(term); }
                _ => {}
            }
        }
        let line = String::from_utf8_lossy(&self.buffer).to_string();
        self.add_to_history(&line);
        Some(line)
    }

    fn do_tab_complete(&mut self, term: &mut dyn Terminal) {
        let line = String::from_utf8_lossy(&self.buffer).to_string();
        let completions = crate::exec::get_completions(&line);
        if completions.is_empty() { return; }
        if completions.len() == 1 {
            // Replace current word with completion
            let word_start = line.rfind(' ').map(|i| i + 1).unwrap_or(0);
            let prefix = &line[word_start..];
            let completion = &completions[0];
            let suffix = &completion[prefix.len()..];
            for &ch in suffix.as_bytes() { self.do_insert(ch, term); }
        } else {
            // List possible completions
            term.write_str("\n");
            for c in &completions { term.write_str(c); term.write_str("  "); }
            term.write_str("\n");
            // Redraw prompt
            term.write_str("> ");
            let s = String::from_utf8_lossy(&self.buffer);
            term.write_str(&s);
            // Move cursor back to end by counting remaining chars
            let remaining = self.buffer.len().saturating_sub(self.cursor);
            for _ in 0..remaining { term.write_str("\x08"); }
        }
    }

    // ── editing primitives ──────────────────────────────────────────
    fn do_insert(&mut self, ch: u8, term: &mut dyn Terminal) {
        if self.cursor >= self.buffer.len() {
            self.buffer.push(ch); self.cursor = self.buffer.len();
            let buf = [ch];
            term.write_str(core::str::from_utf8(&buf).unwrap_or("?"));
        } else {
            self.buffer.insert(self.cursor, ch); self.cursor += 1;
            let tail = &self.buffer[self.cursor - 1..];
            let s = core::str::from_utf8(tail).unwrap_or("?");
            term.write_str(s);
            for _ in 0..tail.len() - 1 { term.write_str("\x08"); }
        }
    }

    fn do_backspace(&mut self, term: &mut dyn Terminal) {
        if self.cursor == 0 { return; }
        self.cursor -= 1; self.buffer.remove(self.cursor);
        term.write_str("\x08");
        let tail = &self.buffer[self.cursor..]; let tl = tail.len();
        for _ in 0..tl + 1 { term.write_str(" "); }
        for _ in 0..tl + 1 { term.write_str("\x08"); }
        let ts = core::str::from_utf8(tail).unwrap_or("?");
        term.write_str(ts);
        for _ in 0..tl { term.write_str("\x08"); }
    }

    fn do_delete(&mut self, term: &mut dyn Terminal) {
        if self.cursor >= self.buffer.len() { return; }
        self.buffer.remove(self.cursor);
        let tail = &self.buffer[self.cursor..]; let tl = tail.len();
        for _ in 0..tl + 1 { term.write_str(" "); }
        for _ in 0..tl + 1 { term.write_str("\x08"); }
        let ts = core::str::from_utf8(tail).unwrap_or("?");
        term.write_str(ts);
        for _ in 0..tl { term.write_str("\x08"); }
    }

    fn do_cursor_left(&mut self, term: &mut dyn Terminal) {
        if self.cursor > 0 { self.cursor -= 1; term.write_str("\x08"); }
    }

    fn do_cursor_right(&mut self, term: &mut dyn Terminal) {
        if self.cursor < self.buffer.len() {
            let b = self.buffer[self.cursor]; let buf = [b];
            term.write_str(core::str::from_utf8(&buf).unwrap_or("?"));
            self.cursor += 1;
        }
    }

    fn do_home(&mut self, term: &mut dyn Terminal) {
        while self.cursor > 0 { self.do_cursor_left(term); }
    }

    fn do_end(&mut self, term: &mut dyn Terminal) {
        while self.cursor < self.buffer.len() { self.do_cursor_right(term); }
    }

    fn do_clear_line(&mut self, term: &mut dyn Terminal) {
        let len = self.buffer.len();
        self.do_home(term);
        for _ in 0..len { term.write_str(" "); }
        for _ in 0..len { term.write_str("\x08"); }
        self.buffer.clear(); self.cursor = 0;
    }

    // ── history ─────────────────────────────────────────────────────
    fn history_up(&mut self, term: &mut dyn Terminal) {
        let idx = match self.browsing {
            None => { if self.history.is_empty() { return; }
                self.saved_line = String::from_utf8_lossy(&self.buffer).to_string(); 0 }
            Some(i) if i + 1 < self.history.len() => i + 1,
            _ => return,
        };
        self.browsing = Some(idx);
        let t = self.history[idx].clone();
        self.replace_buffer(&t, term);
    }

    fn history_down(&mut self, term: &mut dyn Terminal) {
        let idx = match self.browsing {
            None => return,
            Some(0) => { self.browsing = None;
                let t = self.saved_line.clone(); self.replace_buffer(&t, term); return; }
            Some(i) => i - 1,
        };
        self.browsing = Some(idx);
        let t = self.history[idx].clone();
        self.replace_buffer(&t, term);
    }

    fn replace_buffer(&mut self, new_text: &str, term: &mut dyn Terminal) {
        let old = self.buffer.len();
        self.do_home(term);
        for _ in 0..old { term.write_str(" "); }
        for _ in 0..old { term.write_str("\x08"); }
        self.buffer.clear();
        self.buffer.extend_from_slice(new_text.as_bytes());
        self.cursor = self.buffer.len();
        if !new_text.is_empty() { term.write_str(new_text); }
    }

    fn add_to_history(&mut self, line: &str) {
        if line.is_empty() { return; }
        if self.history.front().map_or(false, |h| h == line) { return; }
        if self.history.len() >= HISTORY_MAX { self.history.pop_back(); }
        self.history.push_front(line.into());
    }

    // ── escape sequences ────────────────────────────────────────────
    fn read_byte_retry(&self, term: &mut dyn Terminal) -> Option<u8> {
        for _ in 0..100 {
            if term.input_available() { return term.read_byte(); }
            for _ in 0..1000 { core::hint::spin_loop(); }
        }
        None
    }

    fn handle_escape(&mut self, term: &mut dyn Terminal) {
        let bracket = match self.read_byte_retry(term) {
            Some(b'[') => b'[', Some(b'O') => b'O', _ => return,
        };
        let code = match self.read_byte_retry(term) {
            Some(c) => c, None => return,
        };
        match (bracket, code) {
            (b'[', b'A') => self.history_up(term),
            (b'[', b'B') => self.history_down(term),
            (b'[', b'C') => self.do_cursor_right(term),
            (b'[', b'D') => self.do_cursor_left(term),
            (b'[', b'H') | (b'O', b'H') => self.do_home(term),
            (b'[', b'F') | (b'O', b'F') => self.do_end(term),
            (b'[', b'3') => { if self.read_byte_retry(term) == Some(b'~') { self.do_delete(term); } }
            _ => {}
        }
    }
}