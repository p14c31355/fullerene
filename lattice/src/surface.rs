extern crate alloc;

use alloc::vec::Vec;
use core::iter;
use core::ops::Range;

/// A software pixel surface.
///
/// `Surface` owns a `Vec<u32>` of RGBA pixels (8 bits per channel,
/// stored as `0xRRGGBBAA` — though alpha is unused initially).
///
/// In the future this can be replaced with:
/// - shared memory mappings
/// - GPU allocations
/// - double‑buffered swap chains
pub struct Surface {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Surface {
    /// Create a new surface filled with `color`.
    pub fn new(width: u32, height: u32, color: u32) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            width,
            height,
            pixels: iter::repeat(color).take(len).collect(),
        }
    }

    // ── accessors ────────────────────────────────────────────

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw pixel buffer (RGBA, row‑major).
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// Check whether (x, y) is inside the surface bounds.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    // ── pixel access ─────────────────────────────────────────

    /// Get the pixel at (x, y), or `None` if out of bounds.
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x < self.width && y < self.height {
            Some(self.pixels[(y as usize) * (self.width as usize) + (x as usize)])
        } else {
            None
        }
    }

    /// Set the pixel at (x, y).  Does nothing if out of bounds.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            self.pixels[idx] = color;
        }
    }

    /// Fill an axis‑aligned rectangle with `color`.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        let x_range = clamp_range(x, x.saturating_add(w), self.width);
        let y_range = clamp_range(y, y.saturating_add(h), self.height);
        for row in y_range {
            let start = row as usize * (self.width as usize) + x_range.start as usize;
            let end = row as usize * (self.width as usize) + x_range.end as usize;
            self.pixels[start..end].fill(color);
        }
    }

    // ── blit helpers ─────────────────────────────────────────

    /// Copy the contents of `src` onto this surface at (dx, dy).
    /// No scaling — pixels are copied 1:1.
    pub fn blit_at(&mut self, src: &Surface, dx: i32, dy: i32) {
        let src_w = src.width as i32;
        let src_h = src.height as i32;

        // Destination clipping
        let src_start_x = 0i32.max(-dx);
        let src_start_y = 0i32.max(-dy);
        let src_end_x = (src_w as i64)
            .min((self.width as i64).saturating_sub(dx as i64))
            .max(0) as i32;
        let src_end_y = (src_h as i64)
            .min((self.height as i64).saturating_sub(dy as i64))
            .max(0) as i32;

        if src_start_x >= src_end_x || src_start_y >= src_end_y {
            return;
        }

        let dst_start_x = (dx + src_start_x) as u32;
        let dst_start_y = (dy + src_start_y) as u32;

        for sy in src_start_y..src_end_y {
            let dy_idx = dst_start_y + (sy - src_start_y) as u32;
            let src_row_base = sy as usize * src.width as usize;
            let dst_row_base = dy_idx as usize * self.width as usize;
            let count = (src_end_x - src_start_x) as usize;
            let src_slice = &src.pixels[src_row_base + src_start_x as usize..][..count];
            let dst_slice = &mut self.pixels[dst_row_base + dst_start_x as usize..][..count];
            dst_slice.copy_from_slice(src_slice);
        }
    }
}

// ── helpers ─────────────────────────────────────────────────

fn clamp_range(start: u32, end: u32, limit: u32) -> Range<u32> {
    let lo = start.min(limit);
    let hi = end.min(limit);
    lo..hi
}
