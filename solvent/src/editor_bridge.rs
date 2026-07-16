//! Editor bridge — editor event handling dispatched from Solvent.
//!
//! Extracted from lib.rs to keep the main module focused on orchestration.

use crate::{DEFAULT_COLS, DEFAULT_ROWS, GLYPH_H, GLYPH_W, HEAP_EXTEND_RESERVE, RUNTIME_CONTEXT};
use alloc::vec;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use resonance::KeyCode;

/// Render the editor buffer into its window surface.
pub(crate) fn render_editor(rt: &mut crate::RuntimeState) {
    let editor_window = match rt.editor_window {
        Some(id) => id,
        None => return,
    };
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == editor_window)
    {
        Some(w) => w,
        None => {
            rt.editor_window = None;
            rt.editor_dirty = false;
            return;
        }
    };

    let new_cols = (window.width / GLYPH_W).max(1);
    let new_rows = (window.height / GLYPH_H).max(1);

    let cur_surf_w = window.surface.width();
    let cur_surf_h = window.surface.height();
    let new_surf_w = new_cols * GLYPH_W;
    let new_surf_h = new_rows * GLYPH_H;
    if cur_surf_w != new_surf_w || cur_surf_h != new_surf_h {
        let needed = (new_surf_w * new_surf_h) as usize * 4;
        if needed > HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed) {
            let additional = needed
                .saturating_sub(HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed))
                .next_multiple_of(4096);
            let extend_fn = RUNTIME_CONTEXT.callback_snapshot().heap_extend;
            if extend_fn.is_none() || extend_fn.unwrap()(additional).is_err() {
                return;
            }
            HEAP_EXTEND_RESERVE.fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
        }
        let bg = window.surface.get_pixel(0, 0).unwrap_or(0x0a0a1e);
        window.surface = lattice::surface::Surface::new(new_surf_w, new_surf_h, bg);
    }

    rt.editor_buf.ensure_cursor_visible(new_rows as usize);

    let visible = rt.editor_buf.visible_lines(new_rows as usize);
    let total = (new_cols * new_rows) as usize;
    let mut cells = vec![
        LatticeCell {
            ch: b' ',
            fg: 0xCCCCCC,
            bg: 0x0a0a1e
        };
        total
    ];

    let scroll = rt.editor_buf.scroll_row;
    for (row_idx, line) in visible.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            if col < new_cols as usize {
                let idx = row_idx * (new_cols as usize) + col;
                if idx < total {
                    cells[idx] = LatticeCell {
                        ch: ch as u8,
                        fg: 0xCCCCCC,
                        bg: 0x0a0a1e,
                    };
                }
            }
        }
    }

    if rt.editor_buf.cursor_row >= scroll && rt.editor_buf.cursor_row < scroll + new_rows as usize {
        let cursor_row = rt.editor_buf.cursor_row - scroll;
        let cursor_col = rt.editor_buf.cursor_col.min((new_cols - 1) as usize);
        let idx = cursor_row * (new_cols as usize) + cursor_col;
        if idx < total && rt.cursor_visible {
            cells[idx] = LatticeCell {
                ch: cells[idx].ch,
                fg: 0x0a0a1e,
                bg: 0xCCCCCC,
            };
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols: new_cols,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });
    rt.desktop.invalidate_window(editor_window);
    rt.editor_dirty = false;
}

/// Ensure an editor window exists, creating one if necessary.
pub(crate) fn ensure_editor_window(
    rt: &mut crate::RuntimeState,
) -> Option<lattice::window::WindowId> {
    if let Some(id) = rt.editor_window {
        if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
            return Some(id);
        }
    }
    let id = rt.desktop.wm.create_titled_window(
        100,
        80,
        DEFAULT_COLS * GLYPH_W,
        DEFAULT_ROWS * GLYPH_H,
        0x0a0a1e,
        "Text Editor",
    );
    rt.editor_window = Some(id);
    rt.editor_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
    Some(id)
}

/// Save the current editor buffer to its associated file.
pub(crate) fn editor_save_current(rt: &mut crate::RuntimeState) {
    let path = match rt.editor_file_path.as_ref() {
        Some(p) => p.clone(),
        None => return,
    };
    let content = rt.editor_buf.full_text();
    let write_fn = match RUNTIME_CONTEXT.callback_snapshot().vfs_write {
        Some(f) => f,
        None => return,
    };
    if write_fn(&path, content.as_bytes()).is_ok() {
        rt.editor_buf.dirty = false;
    }
    rt.editor_dirty = true;
    rt.frame_due = true;
}

/// Handle a key event for the editor.
pub fn editor_handle_key(scancode: u8, pressed: bool) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    let mut rt = RUNTIME_CONTEXT.runtime();
    let rt = match rt.as_mut() {
        Some(r) => r,
        None => return,
    };

    static EDITOR_CTRL_HELD: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);
    if key == KeyCode::Ctrl {
        EDITOR_CTRL_HELD.store(pressed, core::sync::atomic::Ordering::Relaxed);
        return;
    }
    if key == KeyCode::S && EDITOR_CTRL_HELD.load(core::sync::atomic::Ordering::Relaxed) && pressed
    {
        editor_save_current(rt);
        return;
    }
    if !pressed {
        return;
    }

    let vp = rt
        .editor_window
        .and_then(|id| rt.desktop.wm.windows().iter().find(|w| w.id == id))
        .map(|w| (w.height / GLYPH_H).max(1) as usize)
        .unwrap_or(10);

    match key {
        KeyCode::Enter => {
            rt.editor_buf.insert_char(b'\n');
        }
        KeyCode::Backspace => {
            rt.editor_buf.backspace();
        }
        KeyCode::Left => {
            rt.editor_buf.cursor_left();
        }
        KeyCode::Right => {
            rt.editor_buf.cursor_right();
        }
        KeyCode::Up => {
            rt.editor_buf.cursor_up();
        }
        KeyCode::Down => {
            rt.editor_buf.cursor_down();
        }
        KeyCode::Home => {
            rt.editor_buf.cursor_home();
        }
        KeyCode::End => {
            rt.editor_buf.cursor_end();
        }
        KeyCode::PageUp => {
            rt.editor_buf.page_up(vp);
        }
        KeyCode::PageDown => {
            rt.editor_buf.page_down(vp);
        }
        KeyCode::Space => {
            rt.editor_buf.insert_char(b' ');
        }
        KeyCode::Tab => {
            rt.editor_buf.insert_char(b' ');
            rt.editor_buf.insert_char(b' ');
        }
        _ => {
            if let Some(byte) = key_to_char(key) {
                rt.editor_buf.insert_char(byte);
            } else {
                return;
            }
        }
    }
    rt.editor_buf.clamp_scroll_with_viewport(vp);
    rt.editor_dirty = true;
    rt.frame_due = true;
}

const fn key_to_char(key: KeyCode) -> Option<u8> {
    Some(match key {
        KeyCode::A => b'a',
        KeyCode::B => b'b',
        KeyCode::C => b'c',
        KeyCode::D => b'd',
        KeyCode::E => b'e',
        KeyCode::F => b'f',
        KeyCode::G => b'g',
        KeyCode::H => b'h',
        KeyCode::I => b'i',
        KeyCode::J => b'j',
        KeyCode::K => b'k',
        KeyCode::L => b'l',
        KeyCode::M => b'm',
        KeyCode::N => b'n',
        KeyCode::O => b'o',
        KeyCode::P => b'p',
        KeyCode::Q => b'q',
        KeyCode::R => b'r',
        KeyCode::S => b's',
        KeyCode::T => b't',
        KeyCode::U => b'u',
        KeyCode::V => b'v',
        KeyCode::W => b'w',
        KeyCode::X => b'x',
        KeyCode::Y => b'y',
        KeyCode::Z => b'z',
        KeyCode::Digit1 => b'1',
        KeyCode::Digit2 => b'2',
        KeyCode::Digit3 => b'3',
        KeyCode::Digit4 => b'4',
        KeyCode::Digit5 => b'5',
        KeyCode::Digit6 => b'6',
        KeyCode::Digit7 => b'7',
        KeyCode::Digit8 => b'8',
        KeyCode::Digit9 => b'9',
        KeyCode::Digit0 => b'0',
        _ => return None,
    })
}
