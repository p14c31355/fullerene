//! Desktop wallpaper management.
//!
//! Supports solid-color backgrounds and a simple procedural grid-pattern
//! wallpaper.  Wallpaper state is managed globally so it can be changed
//! at runtime from a settings app or shell command.

use crate::theme;
use spin::Mutex;

/// Wallpaper mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperMode {
    /// Solid colour from the current theme.
    SolidColor,
    /// Lattice grid pattern (two-tone).
    GridPattern,
    /// Centred gradient (top → bottom).
    Gradient,
}

/// Global wallpaper state.
static WALLPAPER_MODE: Mutex<WallpaperMode> = Mutex::new(WallpaperMode::GridPattern);

/// Set the wallpaper mode.
pub fn set_wallpaper(mode: WallpaperMode) {
    *WALLPAPER_MODE.lock() = mode;
}

/// Get the current wallpaper mode.
pub fn get_wallpaper() -> WallpaperMode {
    *WALLPAPER_MODE.lock()
}

/// Get the background colour for the wallpaper.
/// This is consumed by `Desktop::bg_color()`.
pub fn background_color() -> u32 {
    let colors = theme::current_colors();
    match *WALLPAPER_MODE.lock() {
        WallpaperMode::SolidColor => colors.bg,
        WallpaperMode::GridPattern | WallpaperMode::Gradient => colors.bg,
    }
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

    let mode = *WALLPAPER_MODE.lock();
    let colors = theme::current_colors();
    let fb_w = fb_width as usize;
    let fb_h = fb_height as usize;
    let cx = clipped_x0;
    let cy = clipped_y0;
    let cw = clipped_x1 - clipped_x0;
    let ch = clipped_y1 - clipped_y0;

    match mode {
        WallpaperMode::SolidColor => {
            // Fill the solid background colour
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
                    // Entire row is a horizontal grid line — fill with grid_color.
                    let start = rs + cx as usize;
                    let end = (rs + (cx + cw) as usize).min(fb.len());
                    fb[start..end].fill(grid_color);
                } else {
                    // Fill row with background, then draw vertical grid dots.
                    let start = rs + cx as usize;
                    let end = (rs + (cx + cw) as usize).min(fb.len());
                    fb[start..end].fill(colors.bg);

                    // Find the first grid column within the clip rect.
                    let first_col = ((cx + grid_spacing - 1) / grid_spacing) * grid_spacing;
                    let mut gx = first_col;
                    while gx < cx + cw && gx < fb_width {
                        let col_start = rs + gx as usize;
                        let col_end = (col_start + grid_thickness as usize).min(fb.len());
                        fb[col_start..col_end].fill(grid_color);
                        gx += grid_spacing;
                    }
                }
            }
        }
        WallpaperMode::Gradient => {
            // Top → bottom gradient from bg to a slightly lighter shade.
            let from = colors.bg;
            let to = colors.surface;
            for row in cy..cy + ch {
                let y = row;
                if y >= fb_height {
                    continue;
                }
                let t = (y as u64 * 256 / fb_h as u64).min(255) as u32;
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
