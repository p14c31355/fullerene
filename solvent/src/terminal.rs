//! Terminal renderer and `LatticeTerminal` (Carrier `Terminal` impl).
//!
//! Extracted from `lib.rs` to reduce the size of the god-module.

use crate::{HEAP_EXTEND_RESERVE, RUNTIME_CONTEXT};
use alloc::string::String;
use lattice::scene::DirtyRect;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use spin::Mutex;

// ── Constants ────────────────────────────────────────────────
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = terminal_surface::CELL_HEIGHT;

/// Render the terminal window into its surface, then invalidate it.
///
/// Returns early when `rt.term_dirty` is `false` (nothing to do) or when
/// no terminal window exists.
pub fn render_terminal(rt: &mut crate::RuntimeState, term_window: Option<WindowId>) {
    if !rt.term_dirty {
        return;
    }
    let term_window = match term_window {
        Some(id) => id,
        None => return,
    };
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == term_window)
    {
        Some(w) => w,
        None => return,
    };
    let new_cols = (window.width / GLYPH_W).max(1);
    let new_rows = (window.height / GLYPH_H).max(1);
    let cur_cols = rt.term_buf.cols();
    let cur_rows = rt.term_buf.rows();

    if new_cols != cur_cols || new_rows != cur_rows {
        let needed = (new_cols as usize)
            .saturating_mul(new_rows as usize)
            .saturating_mul(GLYPH_W as usize)
            .saturating_mul(GLYPH_H as usize)
            .saturating_mul(4)
            .saturating_add(
                (new_cols as usize)
                    .saturating_mul(new_rows as usize)
                    .saturating_mul(core::mem::size_of::<LatticeCell>()),
            );
        let reserve = HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed);
        if needed > reserve {
            let additional = needed.saturating_sub(reserve).next_multiple_of(4096);
            match RUNTIME_CONTEXT.callback_snapshot().heap_extend {
                Some(f) if f(additional).is_ok() => {
                    HEAP_EXTEND_RESERVE
                        .fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                }
                _ => return,
            }
        }
        let old_cur_col = rt.term_buf.cursor_col();
        let old_cur_row = rt.term_buf.cursor_row();
        let new_buf = TerminalBuffer::new(new_cols, new_rows);
        let old_buf = core::mem::replace(&mut rt.term_buf, new_buf);
        {
            let src_cells = old_buf.cells();
            let src_cols = cur_cols as usize;
            for row in 0..(cur_rows as usize).min(new_rows as usize) {
                for col in 0..(cur_cols as usize).min(new_cols as usize) {
                    let src_idx = row * src_cols + col;
                    if src_idx < src_cells.len() {
                        if let Some(dst) = rt.term_buf.cell_mut(col as u32, row as u32) {
                            *dst = nozzle::terminal_buffer::Cell {
                                ch: src_cells[src_idx].ch,
                                fg: src_cells[src_idx].fg,
                                bg: src_cells[src_idx].bg,
                            };
                        }
                    }
                }
            }
        }
        rt.term_buf.set_cursor(
            old_cur_col.min(new_cols.saturating_sub(1)),
            old_cur_row.min(new_rows.saturating_sub(1)),
        );
        drop(old_buf);
        window.surface = lattice::surface::Surface::new(
            new_cols * GLYPH_W,
            new_rows * GLYPH_H,
            window.surface.get_pixel(0, 0).unwrap_or(0x000000),
        );
        rt.term_cells.clear();
        rt.term_cells.resize(
            (new_cols * new_rows) as usize,
            LatticeCell {
                ch: b' ',
                fg: 0,
                bg: 0,
            },
        );
    }

    let visible = rt.term_buf.visible_cells();
    let cols = rt.term_buf.cols().max(1);
    let current_cursor = (rt.term_buf.cursor_col(), rt.term_buf.cursor_row());
    let client_x = window.x;
    let client_y = window.y
        + if window.title.is_some() {
            lattice::style::title_bar_height() as i32
        } else {
            0
        };
    let mut dirty_cells: Option<DirtyRect> = None;
    let mut mark_cell_dirty = |col: u32, row: u32| {
        let x = client_x.saturating_add((col * GLYPH_W) as i32).max(0) as u32;
        let y = client_y.saturating_add((row * GLYPH_H) as i32).max(0) as u32;
        let cell_rect = DirtyRect::new(x, y, GLYPH_W, GLYPH_H);
        if let Some(dirty) = dirty_cells.as_mut() {
            dirty.merge(&cell_rect);
        } else {
            dirty_cells = Some(cell_rect);
        }
    };

    // `term_cells` is the last grid actually painted to the surface. Update
    // only changed cells; this matters when a shell is maximized, because the
    // grid can contain tens of thousands of cells.
    let old_cursor = rt.term_rendered_cursor;
    if let Some((col, row, _)) = old_cursor {
        if (col, row) != current_cursor {
            let index = row as usize * cols as usize + col as usize;
            if let Some(cell) = rt.term_cells.get(index).copied() {
                terminal_surface::render_cell(&mut window.surface, cell, col, row, false);
                mark_cell_dirty(col, row);
            }
        }
    }

    for (i, source) in visible.iter().enumerate() {
        let cell = LatticeCell {
            ch: source.ch,
            fg: source.fg,
            bg: source.bg,
        };
        let col = (i as u32) % cols;
        let row = (i as u32) / cols;
        let changed = rt.term_cells.get(i).copied() != Some(cell);
        let cursor_changed = old_cursor
            != Some((current_cursor.0, current_cursor.1, rt.cursor_visible))
            && (col, row) == current_cursor;
        if changed || cursor_changed {
            terminal_surface::render_cell(
                &mut window.surface,
                cell,
                col,
                row,
                rt.cursor_visible && (col, row) == current_cursor,
            );
            mark_cell_dirty(col, row);
        }
    }

    rt.term_cells.clear();
    rt.term_cells
        .extend(visible.iter().map(|source| LatticeCell {
            ch: source.ch,
            fg: source.fg,
            bg: source.bg,
        }));
    rt.term_rendered_cursor = Some((current_cursor.0, current_cursor.1, rt.cursor_visible));
    if let Some(dirty) = dirty_cells {
        rt.desktop.push_dirty_rect(dirty);
    }
    rt.term_dirty = false;
}

// ── LatticeTerminal ──────────────────────────────────────────

pub struct LatticeTerminal;

fn record_session_history(
    history: &mut alloc::collections::VecDeque<String>,
    line: &str,
    capacity: usize,
) {
    if line.is_empty() || history.front().is_some_and(|entry| entry == line) {
        return;
    }
    if history.len() >= capacity {
        history.pop_back();
    }
    history.push_front(String::from(line));
}

impl carrier::terminal::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        if let Some(ref mut out) = *crate::PIPE_STDOUT.lock() {
            out.push_str(s);
        } else {
            let mut rt = crate::RUNTIME_CONTEXT.runtime();
            if let Some(ref mut r) = *rt {
                r.term_buf.put_str(s);
                r.term_dirty = true;
                r.frame_due = true;
            }
        }
    }
    fn read_byte(&mut self) -> Option<u8> {
        loop {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
                    runtime.term_buf.reset_scroll();
                }
                return Some(ch);
            }
            // A launch command arms a one-shot direct handoff. Run it before
            // the runtime tick so no compositor/runtime lock is reacquired
            // between command return and the process context switch.
            crate::yield_scheduler();
            crate::runtime_tick_no_fb();
            // The callback is also kept after the tick for ordinary polling;
            // it is a no-op unless a command has armed a handoff.
            crate::yield_scheduler();
        }
    }
    fn input_available(&self) -> bool {
        nitrogen::ps2::keyboard::input_available()
    }
    fn set_stdin(&mut self, data: String) {
        *crate::PIPE_STDIN.lock() = Some(data);
    }
    fn take_stdout(&mut self) -> Option<String> {
        crate::PIPE_STDOUT.lock().take()
    }
    fn take_stdin(&mut self) -> Option<String> {
        crate::PIPE_STDIN.lock().take()
    }
    fn arm_pipe_stdout(&mut self) {
        *crate::PIPE_STDOUT.lock() = Some(String::new());
    }
    fn clear_pipe_stdin(&mut self) {
        *crate::PIPE_STDIN.lock() = None;
    }
    fn record_history(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        let mut runtime = crate::RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        record_session_history(&mut runtime.command_history, line, 128);
    }
    fn history_snapshot(&self) -> alloc::vec::Vec<String> {
        crate::RUNTIME_CONTEXT
            .runtime()
            .as_ref()
            .map(|runtime| runtime.command_history.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::record_session_history;
    use alloc::collections::VecDeque;
    use alloc::string::String;

    #[test]
    fn terminal_history_is_session_local_bounded_and_deduplicated() {
        let mut first = VecDeque::new();
        let second: VecDeque<String> = VecDeque::new();
        record_session_history(&mut first, "ls", 2);
        record_session_history(&mut first, "ls", 2);
        record_session_history(&mut first, "pwd", 2);
        record_session_history(&mut first, "uname", 2);

        assert_eq!(
            first
                .iter()
                .map(String::as_str)
                .collect::<alloc::vec::Vec<_>>(),
            ["uname", "pwd"]
        );
        assert!(second.is_empty());
    }
}

/// Shared pipe buffers for shell I/O.
pub static PIPE_STDIN: Mutex<Option<String>> = Mutex::new(None);
pub static PIPE_STDOUT: Mutex<Option<String>> = Mutex::new(None);
