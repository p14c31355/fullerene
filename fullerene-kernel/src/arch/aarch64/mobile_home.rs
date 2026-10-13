//! v330: the phone-native home screen - the first portrait-first UI for
//! FullereneOS. Renders directly onto the ANE canvas with primitive
//! drawing (no font dependency for the first light), so it cannot hang
//! in the glyph-mapping path that froze the desktop render.
//!
//! Layout (all coordinates derived from the canvas dims):
//!   - status bar: the top 64 px band
//!   - app grid: 4 columns x 5 rows of rounded tiles
//!   - dock: the bottom 200 px band with 4 tiles
//!
//! This is the first backend of the reusable mobile shell: every constant
//! derives from the canvas size, so other ARM64 phones reuse it as-is.

use core::sync::atomic::Ordering;

/// Fill a rectangle with a color, clipped to the canvas.
fn fill_rect(fb: &mut [u32], fb_w: u32, fb_h: u32, x: u32, y: u32, w: u32, h: u32, color: u32) {
    let x1 = x.min(fb_w);
    let y1 = y.min(fb_h);
    let x2 = x.saturating_add(w).min(fb_w);
    let y2 = y.saturating_add(h).min(fb_h);
    for row in y1..y2 {
        let base = (row * fb_w) as usize;
        for col in x1..x2 {
            fb[base + col as usize] = color;
        }
    }
}

/// Fill a rounded-rect tile (corner radius r) - simple circle test on the
/// corners, plain rect inside.
fn fill_tile(
    fb: &mut [u32],
    fb_w: u32,
    fb_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    r: u32,
    color: u32,
) {
    let x1 = x.min(fb_w);
    let y1 = y.min(fb_h);
    let x2 = x.saturating_add(w).min(fb_w);
    let y2 = y.saturating_add(h).min(fb_h);
    let r2 = r * r;
    for row in y1..y2 {
        let dy_corner_top = if row < y + r { y + r - row } else { 0 };
        let dy_corner_bot = if row >= y2.saturating_sub(r) {
            row - (y2 - r) + 1
        } else {
            0
        };
        let dy = dy_corner_top.max(dy_corner_bot);
        let dx_lim = if dy > 0 {
            let dy2 = dy * dy;
            if dy2 >= r2 {
                0
            } else {
                r - (r2 - dy2).integer_sqrt()
            }
        } else {
            0
        };
        let start = (x + dx_lim).min(x2);
        let end = (x + w).saturating_sub(dx_lim).min(x2);
        if start < end {
            let base = (row * fb_w) as usize;
            for col in start..end {
                fb[base + col as usize] = color;
            }
        }
    }
}

/// Integer square root (Newton) - no std dependency.
trait IntegerSqrt {
    fn integer_sqrt(self) -> Self;
}

impl IntegerSqrt for u32 {
    fn integer_sqrt(self) -> u32 {
        if self < 2 {
            return self;
        }
        let mut x = self;
        let mut y = (x + 1) / 2;
        while y < x {
            x = y;
            y = (x + self / x) / 2;
        }
        x
    }
}

/// Render the phone home screen onto the canvas. Every color/position
/// derives from the canvas dimensions.
pub fn render_home() {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };

    // Palette (Android-ish dark theme).
    let bg: u32 = 0xFF10_14_18;
    let bar: u32 = 0xFF1A_1E_24;
    let tile: u32 = 0xFF2A_30_38;
    let tile_hi: u32 = 0xFF3A_44_50;
    let accent: u32 = 0xFF4C_8D_FF;
    let dock: u32 = 0xFF16_1A_20;

    // Background.
    fill_rect(fb, width, height, 0, 0, width, height, bg);

    // Status bar (top band).
    fill_rect(fb, width, height, 0, 0, width, 64, bar);

    // App grid: 4 cols x 5 rows of tiles between the bars.
    let grid_top = 96u32;
    let margin = 40u32;
    let gap = 28u32;
    let cols = 4u32;
    let rows = 5u32;
    let tile_w = (width - margin * 2 - gap * (cols - 1)) / cols;
    let tile_h = tile_w; // square tiles
    let dock_top = height.saturating_sub(200);
    let grid_bottom = dock_top.saturating_sub(32);
    let row_pitch = tile_h + gap;
    for row in 0..rows {
        let y = grid_top + row * row_pitch;
        if y + tile_h > grid_bottom {
            break;
        }
        for col in 0..cols {
            let x = margin + col * (tile_w + gap);
            // Alternate shades so the grid is visible without fonts.
            let color = if (row + col) % 2 == 0 { tile } else { tile_hi };
            fill_tile(fb, width, height, x, y, tile_w, tile_h, 24, color);
        }
    }

    // Dock band with 4 tiles.
    fill_rect(fb, width, height, 0, dock_top, width, 200, dock);
    let dock_tile = 120u32;
    let dock_gap = (width - 4 * dock_tile) / 5;
    for i in 0..4u32 {
        let x = dock_gap + i * (dock_tile + dock_gap);
        let y = dock_top + (200 - dock_tile) / 2;
        fill_tile(fb, width, height, x, y, dock_tile, dock_tile, 28, accent);
    }

    // A thin accent line under the status bar.
    fill_rect(fb, width, height, 0, 64, width, 4, accent);
}
