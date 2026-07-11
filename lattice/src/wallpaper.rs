//! Desktop wallpaper management.
//!
//! Supports solid-color backgrounds, procedural grid/gradient patterns, and
//! named preset wallpapers (beach, mountain, city).  Wallpaper state is
//! managed globally so it can be changed at runtime from a settings app or
//! shell command.

use crate::theme;
use core::sync::atomic::{AtomicU32, Ordering};

/// Wallpaper mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperMode {
    /// Solid colour from the current theme.
    SolidColor,
    /// Lattice grid pattern (two-tone).
    GridPattern,
    /// Centred gradient (top → bottom).
    Gradient,
    /// Named preset (index into [`wallpaper_presets`]).
    Preset(usize),
}

// ── Atomic walllpaper mode encoding ──────────────────────────
//
// WallpaperMode is packed into a u32 so it can be stored in an AtomicU32,
// avoiding spin::Mutex lock contention (and potential deadlocks) in bare‑metal
// no_std environments where the same CPU may re-enter the lock from interrupt
// or nested runtime contexts.
//
// Encoding:
//   bits 0..1  : discriminant (0=SolidColor, 1=GridPattern, 2=Gradient, 3=Preset)
//   bits 2..31 : preset index (only valid when discriminant == 3)

const DISC_SOLID: u32 = 0;
const DISC_GRID: u32 = 1;
const DISC_GRADIENT: u32 = 2;
const DISC_PRESET: u32 = 3;

impl WallpaperMode {
    /// Pack this mode into a u32.
    pub const fn into_u32(self) -> u32 {
        match self {
            WallpaperMode::SolidColor => DISC_SOLID,
            WallpaperMode::GridPattern => DISC_GRID,
            WallpaperMode::Gradient => DISC_GRADIENT,
            WallpaperMode::Preset(idx) => DISC_PRESET | ((idx as u32) << 2),
        }
    }

    /// Unpack a u32 back into a WallpaperMode.
    /// Unknown discriminants default to SolidColor.
    pub const fn from_u32(raw: u32) -> Self {
        match raw & 0b11 {
            DISC_SOLID => WallpaperMode::SolidColor,
            DISC_GRID => WallpaperMode::GridPattern,
            DISC_GRADIENT => WallpaperMode::Gradient,
            DISC_PRESET => WallpaperMode::Preset((raw >> 2) as usize),
            _ => WallpaperMode::SolidColor,
        }
    }
}

/// A named wallpaper preset with raw pixel data (tileable).
#[derive(Debug, Clone, Copy)]
pub struct WallpaperPreset {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub pixels: &'static [u32],
}

// ── Preset generators (const fn) ──────────────────────────────
// Note: const fn in current stable Rust (2024 edition) does not support
// Ord::min / Ord::clamp on primitive types.  Use manual if/else clamping.

/// Const-compatible colour blend: `(a * (255-t) + b * t) / 255`.
const fn blend_const(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255 - t;
    let r = (((a >> 16) & 0xFF) * it + ((b >> 16) & 0xFF) * t) / 255;
    let g = (((a >> 8) & 0xFF) * it + ((b >> 8) & 0xFF) * t) / 255;
    let b = ((a & 0xFF) * it + (b & 0xFF) * t) / 255;
    (r << 16) | (g << 8) | b
}

/// Manual clamp for u8: `if v > max { max } else { v }`.
const fn clamp_u8(v: u8, max: u8) -> u8 {
    if v > max { max } else { v }
}

// Preset dimensions (tileable, power-of-two-friendly for alignment).
const BEACH_W: u32 = 160;
const BEACH_H: u32 = 120;
const MOUNTAIN_W: u32 = 200;
const MOUNTAIN_H: u32 = 120;
const CITY_W: u32 = 200;
const CITY_H: u32 = 120;

const BEACH_WU: usize = BEACH_W as usize;
const BEACH_HU: usize = BEACH_H as usize;
const MOUNTAIN_WU: usize = MOUNTAIN_W as usize;
const MOUNTAIN_HU: usize = MOUNTAIN_H as usize;
const CITY_WU: usize = CITY_W as usize;
const CITY_HU: usize = CITY_H as usize;

/// Generate a "beach" wallpaper: sky → ocean → sand, with a sun.
const fn gen_beach() -> ([u32; BEACH_WU * BEACH_HU], u32, u32) {
    let mut buf = [0u32; BEACH_WU * BEACH_HU];
    let sky_bottom = BEACH_HU * 5 / 10; // horizon
    let ocean_bottom = BEACH_HU * 7 / 10;
    let sun_cx = BEACH_WU * 3 / 4;
    let sun_cy = BEACH_HU * 3 / 10;
    let sun_r = 18i32;
    let sun_r2 = sun_r * sun_r;
    let mut y = 0usize;
    while y < BEACH_HU {
        let in_sky = y < sky_bottom;
        let in_ocean = y >= sky_bottom && y < ocean_bottom;
        let mut x = 0usize;
        while x < BEACH_WU {
            let px_idx = y * BEACH_WU + x;
            // Sun
            let dx = x as i32 - sun_cx as i32;
            let dy = y as i32 - sun_cy as i32;
            let in_sun = dx * dx + dy * dy <= sun_r2;
            if in_sun {
                buf[px_idx] = 0xFFD700; // gold sun
            } else if in_sky {
                // Sky gradient: darker at top, lighter near horizon
                let t = (y as u32 * 255 / sky_bottom as u32) as u8;
                buf[px_idx] = blend_const(0x1a3a5c, 0x4a8ac4, t);
            } else if in_ocean {
                // Ocean: slightly wavy colour
                let wave = ((x as u32 + y as u32 * 3) / 4 % 16) as u8;
                let base = 0x1a6a8a;
                let light = 0x2a8aaa;
                buf[px_idx] = blend_const(base, light, wave.saturating_mul(16));
            } else {
                // Sand
                let grain = ((x as u32 * 7 + y as u32 * 13) % 32) as u8;
                buf[px_idx] = blend_const(0xc2a060, 0xd4b878, grain.saturating_mul(8));
            }
            x += 1;
        }
        y += 1;
    }
    (buf, BEACH_W, BEACH_H)
}

/// Generate a "mountain" wallpaper: night sky, mountain silhouette, lake reflection.
const fn gen_mountain() -> ([u32; MOUNTAIN_WU * MOUNTAIN_HU], u32, u32) {
    let mut buf = [0u32; MOUNTAIN_WU * MOUNTAIN_HU];
    let horizon = MOUNTAIN_HU * 6 / 10;
    // Mountain peak positions
    let p1_cx = MOUNTAIN_WU as i32 / 4;
    let p1_h = (MOUNTAIN_HU * 4 / 10) as i32;
    let p2_cx = MOUNTAIN_WU as i32 * 5 / 8;
    let p2_h = (MOUNTAIN_HU * 5 / 10) as i32;
    let mut y = 0usize;
    while y < MOUNTAIN_HU {
        let in_sky = y < horizon;
        let mut x = 0usize;
        while x < MOUNTAIN_WU {
            let px_idx = y * MOUNTAIN_WU + x;
            let xi = x as i32;
            let yi = y as i32;
            if in_sky {
                // Night sky with stars
                let star_seed = (xi.wrapping_mul(1271) ^ yi.wrapping_mul(3169)) as u32;
                let is_star = star_seed % 73 == 0 && yi % 3 == 0;
                if is_star {
                    // Vary star brightness
                    let bright = 180u8 + (star_seed % 76) as u8;
                    buf[px_idx] = (bright as u32) * 0x010101;
                } else {
                    let t_raw = (y as u32 * 255 / horizon as u32) as u8;
                    let t = clamp_u8(t_raw, 200);
                    buf[px_idx] = blend_const(0x0a0a1e, 0x1a2a4a, t);
                }
            } else {
                // Below horizon: mountains + reflection
                let y_from_horizon = yi - horizon as i32;
                // Mountain 1: triangle from p1_cx
                let m1_top = horizon as i32 - p1_h;
                let m1_half_w = p1_h * 3 / 2;
                let inside_m1 = yi >= m1_top
                    && (xi - p1_cx).abs()
                        <= (m1_half_w as i64 * (yi - m1_top) as i64 / p1_h as i64) as i32;
                // Mountain 2: triangle from p2_cx
                let m2_top = horizon as i32 - p2_h;
                let m2_half_w = p2_h;
                let inside_m2 = yi >= m2_top
                    && (xi - p2_cx).abs()
                        <= (m2_half_w as i64 * (yi - m2_top) as i64 / p2_h as i64) as i32;

                if inside_m1 {
                    let shade = ((yi - m1_top) as u32 * 80 / p1_h as u32) as u8;
                    buf[px_idx] = blend_const(0x1a2a3a, 0x2a3a4a, shade);
                } else if inside_m2 {
                    let shade = ((yi - m2_top) as u32 * 80 / p2_h as u32) as u8;
                    buf[px_idx] = blend_const(0x1e2e3e, 0x2e3e4e, shade);
                } else {
                    // Lake / ground reflection
                    let reflect = y_from_horizon as u32;
                    let shade_raw = (reflect * 30 / (MOUNTAIN_HU - horizon) as u32) as u8;
                    let shade = clamp_u8(shade_raw, 255);
                    buf[px_idx] = blend_const(0x0a1a2a, 0x1a2a3a, shade);
                }
            }
            x += 1;
        }
        y += 1;
    }
    (buf, MOUNTAIN_W, MOUNTAIN_H)
}

/// Generate a "city" wallpaper: night city skyline with lit windows.
const fn gen_city() -> ([u32; CITY_WU * CITY_HU], u32, u32) {
    let mut buf = [0u32; CITY_WU * CITY_HU];
    // Building heights (skyline)
    let buildings: [u32; 14] = [40, 65, 55, 90, 70, 50, 85, 60, 95, 45, 75, 55, 80, 50];
    let col_per_building = CITY_WU / buildings.len();
    let horizon = CITY_HU * 7 / 10;
    let mut y = 0usize;
    while y < CITY_HU {
        let in_sky = y < horizon;
        let mut x = 0usize;
        while x < CITY_WU {
            let px_idx = y * CITY_WU + x;
            if in_sky {
                // Night sky with gradient
                let t = (y as u32 * 255 / horizon as u32) as u8;
                let sky = blend_const(0x050510, 0x0a1a3a, t);
                // Stars
                let star_seed = (x.wrapping_mul(271) ^ y.wrapping_mul(1169)) as u32;
                let is_star = star_seed % 67 == 0 && (y as u32) % 4 < 2;
                if is_star {
                    buf[px_idx] = 0xCCCCDD;
                } else {
                    buf[px_idx] = sky;
                }
            } else {
                // City buildings
                let bld_idx = x / col_per_building;
                let bld_idx = if bld_idx < buildings.len() {
                    bld_idx
                } else {
                    buildings.len() - 1
                };
                let bld_h = buildings[bld_idx] as usize * CITY_HU / 100;
                let bld_top = CITY_HU - bld_h;
                let in_building = y >= bld_top;
                if in_building {
                    let is_window = (x % 8 >= 2 && x % 8 < 6) && (y % 8 >= 2 && y % 8 < 6);
                    // Random-looking lit windows
                    let win_seed = (x.wrapping_mul(541) ^ y.wrapping_mul(1063)) as u32;
                    let lit = win_seed % 3 == 0;
                    if is_window && lit {
                        buf[px_idx] = 0xFFCC66; // warm window light
                    } else if is_window {
                        buf[px_idx] = 0x1a1a2e; // dark window
                    } else {
                        // Building wall - vary slightly
                        let shade = (win_seed % 40) as u8;
                        buf[px_idx] = blend_const(0x1a1e28, 0x222838, shade.saturating_mul(6));
                    }
                } else {
                    // Ground / street level
                    buf[px_idx] = 0x0a0a14;
                }
            }
            x += 1;
        }
        y += 1;
    }
    (buf, CITY_W, CITY_H)
}

/// Pre-computed beach wallpaper pixels.
static BEACH_PIXELS: ([u32; BEACH_WU * BEACH_HU], u32, u32) = gen_beach();
/// Pre-computed mountain wallpaper pixels.
static MOUNTAIN_PIXELS: ([u32; MOUNTAIN_WU * MOUNTAIN_HU], u32, u32) = gen_mountain();
/// Pre-computed city wallpaper pixels.
static CITY_PIXELS: ([u32; CITY_WU * CITY_HU], u32, u32) = gen_city();

/// Available wallpaper presets (module-level static array).
///
/// Module-level `static` avoids the lazy-initialisation guard that
/// function-scoped `static` would require — in `no_std` bare‑metal
/// kernels the guard atomics may not work correctly, causing hangs.
static WALLPAPER_PRESETS: [WallpaperPreset; 3] = [
    WallpaperPreset {
        name: "beach",
        width: BEACH_W,
        height: BEACH_H,
        pixels: &BEACH_PIXELS.0,
    },
    WallpaperPreset {
        name: "mountain",
        width: MOUNTAIN_W,
        height: MOUNTAIN_H,
        pixels: &MOUNTAIN_PIXELS.0,
    },
    WallpaperPreset {
        name: "city",
        width: CITY_W,
        height: CITY_H,
        pixels: &CITY_PIXELS.0,
    },
];

/// Return the static wallpaper presets slice.
pub fn wallpaper_presets() -> &'static [WallpaperPreset] {
    &WALLPAPER_PRESETS
}

/// Global wallpaper state (lock‑free atomic).
static WALLPAPER_MODE: AtomicU32 = AtomicU32::new(DISC_GRADIENT);

/// Set the wallpaper mode.
pub fn set_wallpaper(mode: WallpaperMode) {
    WALLPAPER_MODE.store(mode.into_u32(), Ordering::SeqCst);
}

/// Get the current wallpaper mode.
pub fn get_wallpaper() -> WallpaperMode {
    WallpaperMode::from_u32(WALLPAPER_MODE.load(Ordering::SeqCst))
}

/// Look up a wallpaper preset by name (case-insensitive).
pub fn find_preset(name: &str) -> Option<usize> {
    let name_lower = name.to_lowercase();
    wallpaper_presets()
        .iter()
        .position(|p| p.name.to_lowercase() == name_lower.as_str() || p.name == name_lower.as_str())
}

/// Get the background colour for the wallpaper.
/// This is consumed by `Desktop::bg_color()`.
pub fn background_color() -> u32 {
    let colors = theme::current_colors();
    let _mode = get_wallpaper();
    // All modes use the theme bg as fallback.
    colors.bg
}

/// Render the wallpaper pattern into the framebuffer.
///
/// Called by the compositor during the background layer pass.
/// Only renders the portion of the framebuffer within the given
/// clipping rectangle.
pub fn render_wallpaper(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    clip_x: u32,
    clip_y: u32,
    clip_w: u32,
    clip_h: u32,
) {
    // Normalize and clamp clip bounds
    if fb_width == 0 || fb_height == 0 {
        return;
    }

    let clipped_x0 = clip_x.min(fb_width);
    let clipped_y0 = clip_y.min(fb_height);
    let clipped_x1 = (clip_x.saturating_add(clip_w)).min(fb_width);
    let clipped_y1 = (clip_y.saturating_add(clip_h)).min(fb_height);

    if clipped_x0 >= clipped_x1 || clipped_y0 >= clipped_y1 {
        return;
    }

    let mode = get_wallpaper();
    let colors = theme::current_colors();
    let fb_w = fb_width as usize;
    let cx = clipped_x0;
    let cy = clipped_y0;
    let cw = clipped_x1 - clipped_x0;
    let ch = clipped_y1 - clipped_y0;

    match mode {
        WallpaperMode::Preset(idx) => {
            let presets = wallpaper_presets();
            if let Some(preset) = presets.get(idx) {
                let pw = preset.width as usize;
                let pixels = preset.pixels;
                for row_offset in 0..ch {
                    let y = cy + row_offset;
                    if y >= fb_height {
                        continue;
                    }
                    let src_y = (y % preset.height) as usize;
                    let rs = (y as usize) * fb_w;
                    let src_row_start = src_y * pw;
                    for col_offset in 0..cw {
                        let x = cx + col_offset;
                        if x >= fb_width {
                            continue;
                        }
                        let src_x = (x % preset.width) as usize;
                        let color = pixels[src_row_start + src_x];
                        fb[rs + x as usize] = color;
                    }
                }
            } else {
                // Invalid preset index — fill with bg
                for row in cy..cy + ch {
                    let rs = (row as usize) * fb_w;
                    let start = rs + cx as usize;
                    let end = (rs + (cx + cw) as usize).min(fb.len());
                    fb[start..end].fill(colors.bg);
                }
            }
        }
        WallpaperMode::SolidColor => {
            for row in cy..cy + ch {
                let y = row;
                if y >= fb_height {
                    continue;
                }
                let rs = (y as usize) * fb_w;
                let start = rs + cx as usize;
                let end = (rs + (cx + cw) as usize).min(fb.len());
                fb[start..end].fill(colors.bg);
            }
        }
        WallpaperMode::GridPattern => {
            let grid_spacing: u32 = 64;
            let grid_thickness: u32 = 2;
            let grid_color = blend_over(colors.bg, colors.surface, 30);

            for row in cy..cy + ch {
                let y = row;
                if y >= fb_height {
                    continue;
                }
                let on_grid_y = (y % grid_spacing) < grid_thickness;
                let rs = (y as usize) * fb_w;

                if on_grid_y {
                    let start = rs + cx as usize;
                    let end = (rs + (cx + cw) as usize).min(fb.len());
                    fb[start..end].fill(grid_color);
                } else {
                    let start = rs + cx as usize;
                    let end = (rs + (cx + cw) as usize).min(fb.len());
                    fb[start..end].fill(colors.bg);

                    let first_col = ((cx + grid_spacing - 1) / grid_spacing) * grid_spacing;
                    let row_max = (rs + (cx + cw) as usize).min(fb.len());
                    let mut gx = first_col;
                    while gx < cx + cw && gx < fb_width {
                        let col_start = rs + gx as usize;
                        let col_end = (col_start + grid_thickness as usize).min(row_max);
                        fb[col_start..col_end].fill(grid_color);
                        gx += grid_spacing;
                    }
                }
            }
        }
        WallpaperMode::Gradient => {
            let from = colors.bg;
            let to = colors.surface;
            for row in cy..cy + ch {
                let y = row;
                if y >= fb_height {
                    continue;
                }
                let t = (y as u64 * 256 / fb_height as u64).min(255) as u32;
                let color = blend(from, to, t as u8);
                let rs = (y as usize) * fb_w;
                let start = rs + cx as usize;
                let end = (rs + (cx + cw) as usize).min(fb.len());
                fb[start..end].fill(color);
            }
        }
    }
}

/// Blend color `a` over `b` with `alpha` (0-100) opacity of `a`.
fn blend_over(base: u32, top: u32, alpha_pct: u32) -> u32 {
    let a = alpha_pct.min(100);
    let ia = 100 - a;
    let r = (((top >> 16) & 0xFF) * a + ((base >> 16) & 0xFF) * ia) / 100;
    let g = (((top >> 8) & 0xFF) * a + ((base >> 8) & 0xFF) * ia) / 100;
    let b = ((top & 0xFF) * a + (base & 0xFF) * ia) / 100;
    (r << 16) | (g << 8) | b
}

/// Lerp between two colours by `t` (0-255).
fn blend(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255 - t;
    let r = (((a >> 16) & 0xFF) * it + ((b >> 16) & 0xFF) * t) / 255;
    let g = (((a >> 8) & 0xFF) * it + ((b >> 8) & 0xFF) * t) / 255;
    let b = ((a & 0xFF) * it + (b & 0xFF) * t) / 255;
    (r << 16) | (g << 8) | b
}

/// Wallpaper-friendly presets for the shell and settings app.
pub fn wallpaper_modes() -> &'static [(&'static str, WallpaperMode)] {
    &[
        ("solid", WallpaperMode::SolidColor),
        ("grid", WallpaperMode::GridPattern),
        ("gradient", WallpaperMode::Gradient),
    ]
}
