//! Render a terminal cell buffer onto a Lattice [`Surface`].
//!
//! This module provides the bridge between a text buffer (character cells)
//! and the Lixel compositor: it paints glyphs from the built‑in 8×16 bitmap
//! font onto a [`Surface`] pixel buffer.
//!
//! # Future
//!
//! - ANSI colour support (fg/bg per cell)
//! - Cursor rendering (blink state toggled externally)
//! - Scrollback / dirty‑rect optimisation

use crate::font;
use crate::surface::Surface;

/// Terminal cells keep a 3px line gap below the embedded 8x13 glyph.
pub const CELL_HEIGHT: u32 = 16;

/// A single terminal cell — the minimal unit the renderer consumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// The character to display.
    pub ch: u8,
    /// Foreground colour (0xRRGGBB).
    pub fg: u32,
    /// Background colour (0xRRGGBB).
    pub bg: u32,
}

/// Parameters for rendering a terminal buffer onto a surface.
pub struct RenderParams<'a> {
    /// The target surface to draw onto.
    pub surface: &'a mut Surface,
    /// Grid of cells (row‑major, left‑to‑right, top‑to‑bottom).
    pub cells: &'a [Cell],
    /// Number of columns (characters per row).  Rows = `cells.len() / cols`.
    pub cols: u32,
    /// Cursor column, or `None` to hide cursor.
    pub cursor_col: Option<u32>,
    /// Cursor row, or `None` to hide cursor.
    pub cursor_row: Option<u32>,
    /// Whether the cursor is currently visible (blink phase).
    pub cursor_visible: bool,
}

/// Render one terminal cell without walking the rest of the terminal grid.
///
/// Terminal windows can become as large as the framebuffer. Keeping this
/// operation cell-sized lets the runtime update only the cells that changed
/// instead of repainting a multi-million-pixel surface for every keystroke.
pub fn render_cell(surface: &mut Surface, cell: Cell, col: u32, row: u32, cursor: bool) {
    let glyph_w = font::GLYPH_WIDTH as usize;
    let glyph_h = font::GLYPH_HEIGHT as usize;
    let cell_h = CELL_HEIGHT as usize;
    let surf_w = surface.width() as usize;
    let surf_h = surface.height() as usize;
    let dx = col as usize * glyph_w;
    let dy = row as usize * cell_h;

    if dx > surf_w.saturating_sub(glyph_w) || dy > surf_h.saturating_sub(cell_h) {
        return;
    }

    let pixels = surface.pixels_mut();
    for gy in 0..cell_h {
        let row_base = (dy + gy) * surf_w;
        pixels[row_base + dx..row_base + dx + glyph_w].fill(cell.bg);
    }

    let glyph = font::glyph_fast(cell.ch);
    for gy in 0..glyph_h {
        let row_base = (dy + gy) * surf_w;
        let bits = glyph.row_byte(gy as u32);
        for gx in 0..glyph_w {
            if bits & (0x80 >> gx) != 0 {
                pixels[row_base + dx + gx] = cell.fg;
            }
        }
    }

    if cursor {
        for gy in (cell_h - 2)..cell_h {
            let row_base = (dy + gy) * surf_w;
            pixels[row_base + dx..row_base + dx + glyph_w].fill(cell.fg);
        }
    }
}

/// Render a terminal cell grid onto a surface using the 8×16 bitmap font.
///
/// Each cell occupies `font::GLYPH_WIDTH × CELL_HEIGHT` pixels.
/// The surface is filled cell‑by‑cell from the top‑left.
pub fn render(params: RenderParams<'_>) {
    let RenderParams {
        surface,
        cells,
        cols,
        cursor_col,
        cursor_row,
        cursor_visible,
    } = params;

    if cols == 0 {
        return;
    }
    let rows = (cells.len() as u32).div_ceil(cols);

    for (i, cell) in cells.iter().enumerate() {
        let col = (i as u32) % cols;
        let row = (i as u32) / cols;
        if row >= rows {
            break;
        }

        // Check if this cell is the cursor position
        let is_cursor = cursor_visible
            && cursor_col.map_or(false, |cc| cc == col)
            && cursor_row.map_or(false, |rr| rr == row);
        render_cell(surface, *cell, col, row, is_cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL_HEIGHT, Cell, render_cell};
    use crate::font;
    use crate::surface::Surface;

    #[test]
    fn cell_redraw_clears_the_line_gap() {
        let mut surface = Surface::new(8, CELL_HEIGHT, 0x112233);
        render_cell(
            &mut surface,
            Cell {
                ch: b'A',
                fg: 0xFFFFFF,
                bg: 0x445566,
            },
            0,
            0,
            false,
        );
        render_cell(
            &mut surface,
            Cell {
                ch: b'B',
                fg: 0xFFFFFF,
                bg: 0x778899,
            },
            0,
            0,
            false,
        );

        assert!(
            (font::GLYPH_HEIGHT..CELL_HEIGHT).all(|row| {
                (0..8).all(|column| surface.get_pixel(column, row) == Some(0x778899))
            })
        );
    }

    #[test]
    fn cursor_is_drawn_at_the_bottom_of_the_terminal_cell() {
        let mut surface = Surface::new(8, CELL_HEIGHT, 0);
        render_cell(
            &mut surface,
            Cell {
                ch: b' ',
                fg: 0xFFFFFF,
                bg: 0,
            },
            0,
            0,
            true,
        );

        assert_eq!(surface.get_pixel(0, CELL_HEIGHT - 2), Some(0xFFFFFF));
        assert_eq!(surface.get_pixel(0, CELL_HEIGHT - 1), Some(0xFFFFFF));
    }
}
