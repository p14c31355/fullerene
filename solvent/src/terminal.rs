//! Terminal renderer and `LatticeTerminal` (Carrier `Terminal` impl).
//!
//! Extracted from `lib.rs` to reduce the size of the god-module.

use crate::{HEAP_EXTEND_RESERVE, RUNTIME, SOLVENT_CALLBACKS};
use alloc::string::String;
use alloc::vec::Vec;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use spin::Mutex;

// ── Constants ────────────────────────────────────────────────
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;

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
        let needed = ((new_cols * new_rows * GLYPH_W * GLYPH_H) as usize * 4)
            .saturating_add((new_cols * new_rows) as usize * 12);
        let reserve = HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed);
        if needed > reserve {
            let additional = needed.saturating_sub(reserve).next_multiple_of(4096);
            match SOLVENT_CALLBACKS.lock().heap_extend {
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

    let total = (rt.term_buf.cols() * rt.term_buf.rows()) as usize;
    if rt.term_cells.len() != total {
        rt.term_cells.resize(
            total,
            LatticeCell {
                ch: b' ',
                fg: 0,
                bg: 0,
            },
        );
    }
    let visible = rt.term_buf.visible_cells();
    rt.term_cells.resize(
        visible.len(),
        LatticeCell {
            ch: b' ',
            fg: 0,
            bg: 0,
        },
    );
    for (i, c) in visible.iter().enumerate() {
        if i < rt.term_cells.len() {
            rt.term_cells[i] = LatticeCell {
                ch: c.ch,
                fg: c.fg,
                bg: c.bg,
            };
        }
    }
    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &rt.term_cells,
        cols: rt.term_buf.cols(),
        cursor_col: Some(rt.term_buf.cursor_col()),
        cursor_row: Some(rt.term_buf.cursor_row()),
        cursor_visible: rt.cursor_visible,
    });
    rt.desktop.invalidate_window(term_window);
    rt.term_dirty = false;
}

// ── LatticeTerminal ──────────────────────────────────────────

pub struct LatticeTerminal;

impl carrier::terminal::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        if let Some(ref mut out) = *crate::PIPE_STDOUT.lock() {
            out.push_str(s);
        } else {
            let mut rt = crate::RUNTIME.lock();
            if let Some(ref mut r) = *rt {
                r.term_buf.put_str(s);
                r.term_dirty = true;
            }
        }
    }
    fn read_byte(&mut self) -> Option<u8> {
        loop {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                return Some(ch);
            }
            crate::runtime_tick_no_fb();
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
}

/// Shared pipe buffers for shell I/O.
pub static PIPE_STDIN: Mutex<Option<String>> = Mutex::new(None);
pub static PIPE_STDOUT: Mutex<Option<String>> = Mutex::new(None);