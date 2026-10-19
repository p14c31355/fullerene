//! v330: the phone-native home screen - the first portrait-first UI for
//! FullereneOS. Renders directly onto the ANE canvas with primitive
//! drawing (no font dependency for the first light), so it cannot hang
//! in the glyph-mapping path that froze the desktop render.
//!
//! Layout (all coordinates derived from the canvas dims):
//!   - status bar: a notch-clearing top band (the ANE-LX2J panel has a
//!     wide U notch ~80 px deep, centered ~330 px - the status bar sits
//!     BELOW it and paints only the notch sides)
//!   - app grid: 4 columns x N rows of rounded tiles
//!   - dock: the bottom band with 4 tiles
//!
//! This is the first backend of the reusable mobile shell: every constant
//! derives from the canvas size, so other ARM64 phones reuse it as-is.
//! The notch geometry comes from the panel descriptor (the boe_otm1911a
//! 5.84" 1080x2280), expressed as fractions of the width/height so other
//! panels substitute their own.

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

/// The launcher geometry, derived once from the canvas so the renderer,
/// the press feedback and the hit test cannot disagree about a tile.
#[derive(Clone, Copy)]
pub struct HomeLayout {
    pub width: u32,
    pub height: u32,
    pub bar_h: u32,
    pub notch_l: u32,
    pub notch_w: u32,
    pub notch_h: u32,
    pub grid_top: u32,
    pub grid_bottom: u32,
    pub margin: u32,
    pub gap: u32,
    pub tile_w: u32,
    pub dock_top: u32,
}

/// What a touch at a point asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HomeHit {
    App { row: u32, col: u32 },
    Dock(u32),
    None,
}

impl HomeLayout {
    pub fn new(width: u32, height: u32) -> Self {
        let notch_w = width * 330 / 1080;
        let notch_h = height * 80 / 2280;
        let bar_h = notch_h + 56;
        let margin = 40u32;
        let gap = 28u32;
        let tile_w = (width - margin * 2 - gap * 3) / 4;
        let dock_top = height.saturating_sub(200);
        Self {
            width,
            height,
            bar_h,
            notch_l: (width - notch_w) / 2,
            notch_w,
            notch_h,
            grid_top: bar_h + 40,
            grid_bottom: dock_top.saturating_sub(32),
            margin,
            gap,
            tile_w,
            dock_top,
        }
    }

    /// Which tile, if any, covers this panel point. Pure geometry: no
    /// canvas, no state, so it is testable off-device.
    pub fn hit_test(&self, x: u32, y: u32) -> HomeHit {
        let pitch = self.tile_w + self.gap;
        if y >= self.dock_top {
            let dock_tile = 120u32;
            let dock_gap = self.width.saturating_sub(4 * dock_tile) / 5;
            if y >= self.dock_top + (200 - dock_tile) / 2 {
                for i in 0..4u32 {
                    let x0 = dock_gap + i * (dock_tile + dock_gap);
                    if x >= x0 && x < x0 + dock_tile {
                        return HomeHit::Dock(i);
                    }
                }
            }
            return HomeHit::None;
        }
        if y < self.grid_top || y >= self.grid_bottom || x < self.margin {
            return HomeHit::None;
        }
        let col = (x - self.margin) / pitch;
        if col >= 4 {
            return HomeHit::None;
        }
        let rel_y = y - self.grid_top;
        if rel_y % pitch >= self.tile_w {
            return HomeHit::None;
        }
        HomeHit::App {
            row: rel_y / pitch,
            col,
        }
    }

    /// Top-left corner of a grid tile.
    pub fn app_origin(&self, row: u32, col: u32) -> (u32, u32) {
        (
            self.margin + col * (self.tile_w + self.gap),
            self.grid_top + row * (self.tile_w + self.gap),
        )
    }
}

/// Status-bar battery capsule: outline, then a fill proportional to charge.
///
/// The charge comes from the Hi6555V100 coulomb counter via the PMU cell
/// (`platform::ane_battery`), two byte reads, so it is re-sampled on every
/// repaint.  A pack whose voltage cannot be read leaves the capsule empty
/// rather than inventing a percentage.
fn draw_battery(fb: &mut [u32], fb_w: u32, fb_h: u32, l: &HomeLayout) {
    let state = crate::platform::ane_battery::state();
    let w = 46u32;
    let h = 20u32;
    let x = fb_w.saturating_sub(l.margin + w);
    let y = l.notch_h + (l.bar_h - l.notch_h).saturating_sub(h) / 2;
    let outline: u32 = 0xFF8A_94_A0;

    fill_rect(fb, fb_w, fb_h, x, y, w, h, outline);
    fill_rect(fb, fb_w, fb_h, x + 2, y + 2, w - 4, h - 4, 0xFF10_14_18);
    // The nub on the right edge, so the capsule reads as a battery.
    fill_rect(fb, fb_w, fb_h, x + w, y + 6, 3, h - 12, outline);

    let Some(permille) = state.charge_permille else {
        return;
    };
    let inner = w - 4;
    let filled = inner * permille.min(1000) as u32 / 1000;
    let color = if state.charging {
        0xFF4C_8D_FF
    } else if permille > 500 {
        0xFF4C_AF_50
    } else if permille > 200 {
        0xFFE0_A0_30
    } else {
        0xFFE0_40_40
    };
    fill_rect(fb, fb_w, fb_h, x + 2, y + 2, filled, h - 4, color);
}

/// Render the phone home screen onto the canvas. Every color/position
/// derives from the canvas dimensions.
pub fn render_home() {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };

    // v331k: the mobile shell owns the panel from this moment - the boot
    // strip, the S-value diag, and the uart mirror all stop painting (the
    // ane_fb paint paths early-return on DESKTOP_OWNED). The diagnostics
    // live in the ring.
    crate::platform::ane_fb::DESKTOP_OWNED.store(true, Ordering::Relaxed);

    // Palette (Android-ish dark theme).
    let bg: u32 = 0xFF10_14_18;
    let bar: u32 = 0xFF1A_1E_24;
    let tile: u32 = 0xFF2A_30_38;
    let tile_hi: u32 = 0xFF3A_44_50;
    let accent: u32 = 0xFF4C_8D_FF;
    let dock: u32 = 0xFF16_1A_20;

    // Background.
    fill_rect(fb, width, height, 0, 0, width, height, bg);

    // v331i: the notch geometry. The ANE-LX2J (boe_otm1911a 5.84")
    // carries a wide U notch: ~330 px wide, ~80 px deep, centered. The
    // reusable shell expresses it in fractions of the panel so other
    // phones substitute their own descriptor values.
    let notch_w = l.notch_w;
    let notch_h = l.notch_h;
    let notch_l = l.notch_l;
    let notch_r = notch_l + notch_w;

    // Status bar: the full-width band. Its content row sits BELOW the
    // notch row: the band covers the notch depth plus the content strip,
    // and the notch rectangle itself is left as background (the cutout).
    let bar_h = l.bar_h;
    fill_rect(fb, width, height, 0, 0, width, bar_h, bar);
    fill_rect(fb, width, height, notch_l, 0, notch_w, notch_h, bg);
    draw_battery(fb, width, height, &l);

    // App grid: 4 cols x N rows of tiles between the bars.
    let grid_top = bar_h + 40;
    let cols = 4u32;
    let rows = 5u32;
    let margin = l.margin;
    let gap = l.gap;
    let tile_w = l.tile_w;
    let tile_h = tile_w; // square tiles
    let dock_top = l.dock_top;
    let grid_bottom = l.grid_bottom;
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

    // A thin accent line under the status bar (skipping the notch span).
    fill_rect(fb, width, height, 0, bar_h, notch_l, 4, accent);
    fill_rect(
        fb,
        width,
        height,
        notch_r,
        bar_h,
        width - notch_r,
        4,
        accent,
    );
}

/// Where the last poll put the finger, in panel pixels.
pub fn touch_point() -> Option<(u32, u32)> {
    let (x, y) = crate::platform::ane_touch::snapshot(0)?;
    Some((x as u32, y as u32))
}

/// v331t: press feedback - repaint the tile the finger is on in the
/// accent color. Returns the hit so the caller can decide what a
/// release on it means, instead of the geometry being re-derived there.
pub fn touch_feedback() -> HomeHit {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let Some((x, y)) = touch_point() else {
        return HomeHit::None;
    };
    let hit = l.hit_test(x, y);
    let (tx, ty) = match hit {
        HomeHit::App { row, col } => l.app_origin(row, col),
        // Dock presses keep their tile lit too; the dock origin is the
        // same arithmetic with the dock pitch.
        HomeHit::Dock(i) => {
            let dock_tile = 120u32;
            let dock_gap = width.saturating_sub(4 * dock_tile) / 5;
            (
                dock_gap + i * (dock_tile + dock_gap),
                l.dock_top + (200 - dock_tile) / 2,
            )
        }
        HomeHit::None => return hit,
    };
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };
    let side = if let HomeHit::App { .. } = hit {
        l.tile_w
    } else {
        120
    };
    fill_tile(fb, width, height, tx, ty, side, side, 24, 0xFF4C_8D_FF);
    hit
}
