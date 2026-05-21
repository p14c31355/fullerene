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

/// Render a terminal cell grid onto a surface using the 8×16 bitmap font.
///
/// Each cell occupies `font::GLYPH_WIDTH × font::GLYPH_HEIGHT` pixels.
/// The surface is filled cell‑by‑cell from the top‑left.
pub fn render(params: RenderParams<'_>) {
    let RenderParams { surface, cells, cols, cursor_col, cursor_row, cursor_visible } = params;

    let rows = if cols > 0 {
        (cells.len() as u32).div_ceil(cols)
    } else {
        0
    };

    let glyph_w = font::GLYPH_WIDTH;
    let glyph_h = font::GLYPH_HEIGHT;

    for (i, cell) in cells.iter().enumerate() {
        let col = (i as u32) % cols;
        let row = (i as u32) / cols;
        if row >= rows { break; }

        let dx = col * glyph_w;
        let dy = row * glyph_h;

        // Check if this cell is the cursor position
        let is_cursor = cursor_visible
            && cursor_col.map_or(false, |cc| cc == col)
            && cursor_row.map_or(false, |rr| rr == row);

        // Draw background
        surface.fill_rect(dx, dy, glyph_w, glyph_h, cell.bg);

        // Draw glyph pixels
        for gy in 0..glyph_h {
            for gx in 0..glyph_w {
                if font::get_glyph_pixel(cell.ch, gy, gx) {
                    surface.set_pixel(dx + gx, dy + gy, cell.fg);
                }
            }
        }

        // Draw cursor (invert fg/bg for the cell)
        if is_cursor {
            // Simple cursor: underline on the bottom 2 rows
            for gx in 0..glyph_w {
                surface.set_pixel(dx + gx, dy + glyph_h - 2, cell.fg);
                surface.set_pixel(dx + gx, dy + glyph_h - 1, cell.fg);
            }
        }
    }
}
