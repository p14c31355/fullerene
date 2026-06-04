//! Text selection and copy for the terminal buffer.
//!
//! Provides a [`Selection`] struct that tracks the mouse‑selected region
//! in the terminal's cell grid, and a method to extract the selected text.

use crate::terminal_buffer::TerminalBuffer;

/// A rectangular text selection in terminal cell coordinates.
///
/// Coordinates are (col, row) with (0, 0) at the top‑left.
/// `start` is always the anchor point (where the drag began);
/// the actual selected region is the bounding rectangle between start and end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Anchor column (where drag started).
    pub anchor_col: u32,
    /// Anchor row.
    pub anchor_row: u32,
    /// Drag‑end column.
    pub end_col: u32,
    /// Drag‑end row.
    pub end_row: u32,
    /// Whether the selection is active (mouse dragged).
    pub active: bool,
}

impl Selection {
    /// Create an inactive selection.
    pub fn new() -> Self {
        Self {
            anchor_col: 0,
            anchor_row: 0,
            end_col: 0,
            end_row: 0,
            active: false,
        }
    }

    /// Start a selection at the given cell coordinates.
    pub fn start(&mut self, col: u32, row: u32) {
        self.anchor_col = col;
        self.anchor_row = row;
        self.end_col = col;
        self.end_row = row;
        self.active = true;
    }

    /// Extend the selection to the given cell coordinates.
    pub fn extend(&mut self, col: u32, row: u32) {
        self.end_col = col;
        self.end_row = row;
    }

    /// End the selection (mark as inactive, but keep the region).
    pub fn end(&mut self) {
        self.active = false;
    }

    /// Cancel the selection (no region selected).
    pub fn cancel(&mut self) {
        self.active = false;
    }

    /// Get the bounding rectangle of the selection: (min_col, min_row, max_col, max_row).
    pub fn bounds(&self) -> (u32, u32, u32, u32) {
        let c0 = self.anchor_col.min(self.end_col);
        let c1 = self.anchor_col.max(self.end_col);
        let r0 = self.anchor_row.min(self.end_row);
        let r1 = self.anchor_row.max(self.end_row);
        (c0, r0, c1, r1)
    }

    /// Check whether the selection is non‑empty.
    pub fn is_empty(&self) -> bool {
        self.anchor_col == self.end_col && self.anchor_row == self.end_row
    }

    /// Check if a cell is within the selection bounds.
    pub fn contains(&self, col: u32, row: u32) -> bool {
        if !self.active || self.is_empty() {
            return false;
        }
        let (c0, r0, c1, r1) = self.bounds();
        col >= c0 && col <= c1 && row >= r0 && row <= r1
    }
}

/// Extract the selected text from the terminal buffer.
///
/// Returns a String with newlines between rows. Trailing spaces are trimmed
/// from each row.
pub fn extract_selection(buf: &TerminalBuffer, sel: &Selection) -> alloc::string::String {
    if !sel.active || sel.is_empty() {
        return alloc::string::String::new();
    }

    let (c0, r0, c1, r1) = sel.bounds();
    let mut result = alloc::string::String::new();

    for row in r0..=r1.min(buf.rows().saturating_sub(1)) {
        let mut line = alloc::string::String::new();
        for col in c0..=c1.min(buf.cols().saturating_sub(1)) {
            let idx = row as usize * buf.cols() as usize + col as usize;
            let cells = buf.cells();
            if idx < cells.len() {
                let ch = cells[idx].ch;
                if ch >= 0x20 {
                    line.push(ch as char);
                } else {
                    line.push(' ');
                }
            }
        }
        // Trim trailing spaces
        let trimmed = line.trim_end_matches(' ');
        if !trimmed.is_empty() || row < r1 {
            result.push_str(trimmed);
        }
        if row < r1 {
            result.push('\n');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selection_bounds() {
        let mut sel = Selection::new();
        sel.start(2, 1);
        sel.extend(5, 4);
        assert_eq!(sel.bounds(), (2, 1, 5, 4));
        assert!(!sel.is_empty());
    }

    #[test]
    fn test_empty_selection() {
        let mut sel = Selection::new();
        sel.start(3, 3);
        assert!(sel.is_empty());
    }

    #[test]
    fn test_selection_contains() {
        let mut sel = Selection::new();
        sel.start(1, 1);
        sel.extend(3, 2);
        assert!(sel.contains(2, 1));
        assert!(sel.contains(1, 2));
        assert!(!sel.contains(0, 1));
        assert!(!sel.contains(4, 2));
    }

    #[test]
    fn test_extract_selection() {
        let mut buf = TerminalBuffer::new(10, 5);
        // Fill buffer with test content
        buf.put_str("Hello");
        buf.newline();
        buf.put_str("World");
        buf.newline();
        buf.put_str("Test");

        let mut sel = Selection::new();
        sel.start(0, 0);
        sel.extend(4, 1);
        let text = extract_selection(&buf, &sel);
        assert_eq!(text, "Hello\nWorld");
    }
}
