//! Desktop wallpaper management.
//!
//! Supports solid-color backgrounds, procedural grid/gradient patterns, and
//! named preset wallpapers rendered from SVGs at build time.

use crate::theme;
use core::sync::atomic::{AtomicU32, Ordering};

/// Wallpaper mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperMode {
    SolidColor,
    GridPattern,
    Gradient,
    Preset(usize),
}

const DISC_SOLID: u32 = 0;
const DISC_GRID: u32 = 1;
const DISC_GRADIENT: u32 = 2;
const DISC_PRESET: u32 = 3;

impl WallpaperMode {
    pub const fn into_u32(self) -> u32 {
        match self {
            WallpaperMode::SolidColor => DISC_SOLID,
            WallpaperMode::GridPattern => DISC_GRID,
            WallpaperMode::Gradient => DISC_GRADIENT,
            WallpaperMode::Preset(idx) => DISC_PRESET | ((idx as u32) << 2),
        }
    }

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

/// A named wallpaper preset with raw pixel data.
#[derive(Debug, Clone, Copy)]
pub struct WallpaperPreset {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub pixels: &'static [u32],
    pub tileable: bool,
    pub smooth: bool,
}

// ── Pre-rendered wallpaper data (generated at build time) ──

include!(concat!(env!("OUT_DIR"), "/wallpaper_beach.rs"));
include!(concat!(env!("OUT_DIR"), "/wallpaper_mountain.rs"));
include!(concat!(env!("OUT_DIR"), "/wallpaper_city.rs"));
include!(concat!(env!("OUT_DIR"), "/wallpaper_fullerene.rs"));
include!(concat!(env!("OUT_DIR"), "/wallpaper_sharp.rs"));

static WALLPAPER_PRESETS: [WallpaperPreset; 5] = [
    WallpaperPreset {
        name: "beach",
        width: BEACH_W,
        height: BEACH_H,
        pixels: &BEACH_PIXELS,
        tileable: false,
        smooth: false,
    },
    WallpaperPreset {
        name: "mountain",
        width: MOUNTAIN_W,
        height: MOUNTAIN_H,
        pixels: &MOUNTAIN_PIXELS,
        tileable: false,
        smooth: false,
    },
    WallpaperPreset {
        name: "city",
        width: CITY_W,
        height: CITY_H,
        pixels: &CITY_PIXELS,
        tileable: false,
        smooth: false,
    },
    WallpaperPreset {
        name: "fullerene",
        width: FULLERENE_W,
        height: FULLERENE_H,
        pixels: &FULLERENE_PIXELS,
        tileable: false,
        smooth: true,
    },
    WallpaperPreset {
        name: "fullerene-sharp",
        width: SHARP_W,
        height: SHARP_H,
        pixels: &SHARP_PIXELS,
        tileable: false,
        smooth: false,
    },
];

pub fn wallpaper_presets() -> &'static [WallpaperPreset] {
    &WALLPAPER_PRESETS
}

static WALLPAPER_MODE: AtomicU32 = AtomicU32::new(DISC_PRESET | (3 << 2));

pub fn set_wallpaper(mode: WallpaperMode) {
    WALLPAPER_MODE.store(mode.into_u32(), Ordering::SeqCst);
}

pub fn get_wallpaper() -> WallpaperMode {
    WallpaperMode::from_u32(WALLPAPER_MODE.load(Ordering::SeqCst))
}

pub fn find_preset(name: &str) -> Option<usize> {
    let name_lower = name.to_lowercase();
    wallpaper_presets()
        .iter()
        .position(|p| p.name.to_lowercase() == name_lower.as_str())
}

pub fn background_color() -> u32 {
    let colors = theme::current_colors();
    colors.bg
}

pub fn render_wallpaper(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    clip_x: u32,
    clip_y: u32,
    clip_w: u32,
    clip_h: u32,
) {
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
                if preset.tileable {
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
                            fb[rs + x as usize] = pixels[src_row_start + src_x];
                        }
                    }
                } else {
                    let pw = preset.width as u64;
                    let ph = preset.height as u64;
                    let fw = fb_width as u64;
                    let fh = fb_height as u64;
                    let pixels = preset.pixels;

                    if preset.smooth {
                        let precision = 255u64;
                        if fw * ph >= fh * pw {
                            let oy = (ph * fw - fh * pw) / (2 * pw);
                            for row_offset in 0..ch {
                                let y = cy + row_offset;
                                if y >= fb_height {
                                    continue;
                                }
                                let src_y_fp = (y as u64 + oy) * pw * precision / fw;
                                let src_y_i = (src_y_fp / precision) as usize;
                                let fy = (src_y_fp % precision) as u8;
                                let src_y_i = src_y_i.min(ph as usize - 1);
                                let src_y_i1 = (src_y_i + 1).min(ph as usize - 1);
                                let rs = (y as usize) * fb_w;
                                for col_offset in 0..cw {
                                    let x = cx + col_offset;
                                    if x >= fb_width {
                                        continue;
                                    }
                                    let src_x_fp = (x as u64) * pw * precision / fw;
                                    let src_x_i = (src_x_fp / precision) as usize;
                                    let fx = (src_x_fp % precision) as u8;
                                    let src_x_i = src_x_i.min(pw as usize - 1);
                                    let src_x_i1 = (src_x_i + 1).min(pw as usize - 1);
                                    let tl = pixels[src_y_i * (pw as usize) + src_x_i];
                                    let tr = pixels[src_y_i * (pw as usize) + src_x_i1];
                                    let bl = pixels[src_y_i1 * (pw as usize) + src_x_i];
                                    let br = pixels[src_y_i1 * (pw as usize) + src_x_i1];
                                    let top = blend(tl, tr, fx);
                                    let bot = blend(bl, br, fx);
                                    fb[rs + x as usize] = blend(top, bot, fy);
                                }
                            }
                        } else {
                            let ox = (pw * fh - fw * ph) / (2 * ph);
                            for row_offset in 0..ch {
                                let y = cy + row_offset;
                                if y >= fb_height {
                                    continue;
                                }
                                let src_y_fp = (y as u64) * ph * precision / fh;
                                let src_y_i = (src_y_fp / precision) as usize;
                                let fy = (src_y_fp % precision) as u8;
                                let src_y_i = src_y_i.min(ph as usize - 1);
                                let src_y_i1 = (src_y_i + 1).min(ph as usize - 1);
                                let rs = (y as usize) * fb_w;
                                for col_offset in 0..cw {
                                    let x = cx + col_offset;
                                    if x >= fb_width {
                                        continue;
                                    }
                                    let src_x_fp = (x as u64 + ox) * ph * precision / fh;
                                    let src_x_i = (src_x_fp / precision) as usize;
                                    let fx = (src_x_fp % precision) as u8;
                                    let src_x_i = src_x_i.min(pw as usize - 1);
                                    let src_x_i1 = (src_x_i + 1).min(pw as usize - 1);
                                    let tl = pixels[src_y_i * (pw as usize) + src_x_i];
                                    let tr = pixels[src_y_i * (pw as usize) + src_x_i1];
                                    let bl = pixels[src_y_i1 * (pw as usize) + src_x_i];
                                    let br = pixels[src_y_i1 * (pw as usize) + src_x_i1];
                                    let top = blend(tl, tr, fx);
                                    let bot = blend(bl, br, fx);
                                    fb[rs + x as usize] = blend(top, bot, fy);
                                }
                            }
                        }
                    } else {
                        if fw * ph >= fh * pw {
                            let oy = (ph * fw - fh * pw) / (2 * pw);
                            for row_offset in 0..ch {
                                let y = cy + row_offset;
                                if y >= fb_height {
                                    continue;
                                }
                                let src_y =
                                    (((y as u64 + oy) * pw / fw) as usize).min(ph as usize - 1);
                                let rs = (y as usize) * fb_w;
                                let src_row_start = src_y * (pw as usize);
                                for col_offset in 0..cw {
                                    let x = cx + col_offset;
                                    if x >= fb_width {
                                        continue;
                                    }
                                    let src_x =
                                        ((x as u64 * pw / fw) as usize).min(pw as usize - 1);
                                    fb[rs + x as usize] = pixels[src_row_start + src_x];
                                }
                            }
                        } else {
                            let ox = (pw * fh - fw * ph) / (2 * ph);
                            for row_offset in 0..ch {
                                let y = cy + row_offset;
                                if y >= fb_height {
                                    continue;
                                }
                                let src_y = ((y as u64 * ph / fh) as usize).min(ph as usize - 1);
                                let rs = (y as usize) * fb_w;
                                let src_row_start = src_y * (pw as usize);
                                for col_offset in 0..cw {
                                    let x = cx + col_offset;
                                    if x >= fb_width {
                                        continue;
                                    }
                                    let src_x =
                                        (((x as u64 + ox) * ph / fh) as usize).min(pw as usize - 1);
                                    fb[rs + x as usize] = pixels[src_row_start + src_x];
                                }
                            }
                        }
                    }
                }
            } else {
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
            let grid_spacing = 64;
            let grid_thickness = 2;
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

fn blend_over(base: u32, top: u32, alpha_pct: u32) -> u32 {
    let a = alpha_pct.min(100);
    let ia = 100 - a;
    let r = (((top >> 16) & 0xFF) * a + ((base >> 16) & 0xFF) * ia) / 100;
    let g = (((top >> 8) & 0xFF) * a + ((base >> 8) & 0xFF) * ia) / 100;
    let b = ((top & 0xFF) * a + (base & 0xFF) * ia) / 100;
    (r << 16) | (g << 8) | b
}

fn blend(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255 - t;
    let r = (((a >> 16) & 0xFF) * it + ((b >> 16) & 0xFF) * t) / 255;
    let g = (((a >> 8) & 0xFF) * it + ((b >> 8) & 0xFF) * t) / 255;
    let b = ((a & 0xFF) * it + (b & 0xFF) * t) / 255;
    (r << 16) | (g << 8) | b
}

pub fn wallpaper_modes() -> &'static [(&'static str, WallpaperMode)] {
    &[
        ("solid", WallpaperMode::SolidColor),
        ("grid", WallpaperMode::GridPattern),
        ("gradient", WallpaperMode::Gradient),
        ("beach", WallpaperMode::Preset(0)),
        ("mountain", WallpaperMode::Preset(1)),
        ("city", WallpaperMode::Preset(2)),
        ("fullerene", WallpaperMode::Preset(3)),
        ("fullerene-sharp", WallpaperMode::Preset(4)),
    ]
}
