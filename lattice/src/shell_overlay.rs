//! Shell overlay rendering — Task Overview & App Grid.
//!
//! Renders the GNOME-style Activities overlay directly onto the framebuffer.
//! Consists of:
//! - Semi‑transparent black backdrop
//! - Window thumbnails (TaskOverview)
//! - App launcher grid (AppGrid)
//!
//! All rendering is done in software — no GPU / 3D acceleration required.

use crate::compositor::{COLOR_TEXT, COLOR_PRIMARY};
use crate::window::Window;

/// Shell state — determines which overlay to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellState {
    Desktop,
    TaskOverview,
    AppGrid,
    TimeZoneSelector,
}

/// Render the Task Overview overlay on top of the current framebuffer.
///
/// `windows` provides the list of open windows for thumbnail rendering.
/// `fb` is the current framebuffer (already containing rendered desktop).
pub fn render_task_overview(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    windows: &[Window],
) {
    let fb_w = fbw as usize;

    // ── Semi‑transparent black backdrop ──────────────────
    // Blend 60% opaque black over the entire framebuffer.
    for row in 0..fbh {
        let off = (row as usize) * fb_w;
        for col in 0..fbw as usize {
            let bg = fb[off + col];
            let r = ((bg >> 16) & 0xFF) as u32;
            let g = ((bg >> 8) & 0xFF) as u32;
            let b = (bg & 0xFF) as u32;
            // Blend with black (0x000000) at 60% opacity.
            let r2 = (r * 2) / 5;
            let g2 = (g * 2) / 5;
            let b2 = (b * 2) / 5;
            fb[off + col] = (r2 << 16) | (g2 << 8) | b2;
        }
    }

    // ── Window thumbnails ─────────────────────────────────
    let thumb_w = 160u32;
    let thumb_h = 120u32;
    let pad = 20u32;
    let title_h = 22u32;

    let columns = (fbw / (thumb_w + pad)).max(1);
    let label_y_base = 40u32;

    for (i, window) in windows.iter().enumerate() {
        if window.minimized {
            continue; // skip minimized windows in overview
        }
        let col = (i as u32) % columns;
        let row = (i as u32) / columns;
        let tx = pad + col * (thumb_w + pad);
        let ty = label_y_base + row * (thumb_h + title_h + pad);

        // Skip if out of bounds
        if tx + thumb_w > fbw || ty + thumb_h + title_h > fbh {
            continue;
        }

        // Thumbnail background (window surface scaled down)
        let src = &window.surface;
        let sw = src.width().max(1);
        let sh = src.height().max(1);
        let sp = src.pixels();

        for dy in 0..thumb_h {
            let sy = (dy as u64 * sh as u64 / thumb_h as u64) as u32;
            for dx in 0..thumb_w {
                let sx = (dx as u64 * sw as u64 / thumb_w as u64) as u32;
                let color = sp.get((sy * sw + sx) as usize).copied().unwrap_or(0x333344);
                let idx = ((ty + dy) as usize) * fb_w + (tx + dx) as usize;
                if idx < fb.len() {
                    // Draw border
                    let is_border = dy == 0 || dy == thumb_h - 1 || dx == 0 || dx == thumb_w - 1;
                    fb[idx] = if is_border { COLOR_PRIMARY } else { color };
                }
            }
        }

        // Window title below thumbnail
        let title = window.title.as_ref().map(|t| t.as_str()).unwrap_or("Window");
        let label_x = tx + 2;
        let label_y = ty + thumb_h + 3;
        for (j, ch) in title.bytes().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            for gry in 0..12 {
                let py = label_y + gry;
                if py >= fbh {
                    continue;
                }
                for grx in 0..8 {
                    let px = label_x + (j as u32) * 8 + grx;
                    if px >= fbw {
                        continue;
                    }
                    if crate::font::get_glyph_pixel(ch, gry, grx) {
                        let idx = (py as usize) * fb_w + px as usize;
                        if idx < fb.len() {
                            fb[idx] = COLOR_TEXT;
                        }
                    }
                }
            }
        }
    }

    // ── "Activities" label at top ──────────────────────────
    render_label(fb, fbw, fbh, "Task Overview", (fbw / 2).saturating_sub(52), 10);
}

/// Render the App Grid overlay.
pub fn render_app_grid(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
) {
    let fb_w = fbw as usize;

    // ── Semi‑transparent black backdrop ──────────────────
    for row in 0..fbh {
        let off = (row as usize) * fb_w;
        for col in 0..fbw as usize {
            let bg = fb[off + col];
            let r = ((bg >> 16) & 0xFF) as u32;
            let g = ((bg >> 8) & 0xFF) as u32;
            let b = (bg & 0xFF) as u32;
            let r2 = (r * 2) / 5;
            let g2 = (g * 2) / 5;
            let b2 = (b * 2) / 5;
            fb[off + col] = (r2 << 16) | (g2 << 8) | b2;
        }
    }

    // ── App launcher grid ─────────────────────────────────
    #[derive(Clone, Copy)]
    struct AppEntry {
        label: &'static str,
        color: u32,
    }

    let apps: &[AppEntry] = &[
        AppEntry { label: "Terminal", color: 0x333344 },
        AppEntry { label: "Clock", color: 0x333344 },
        AppEntry { label: "Settings", color: 0x333344 },
        AppEntry { label: "File Mgr", color: 0x333344 },
        AppEntry { label: "About", color: 0x333344 },
    ];

    let icon_size = 64u32;
    let pad = 24u32;
    let label_h = 18u32;
    let columns = (fbw / (icon_size + pad)).max(1);
    let start_y = 60u32;

    for (i, app) in apps.iter().enumerate() {
        let col = (i as u32) % columns;
        let row = (i as u32) / columns;
        let ax = pad + col * (icon_size + pad);
        let ay = start_y + row * (icon_size + label_h + pad);

        if ax + icon_size > fbw || ay + icon_size + label_h > fbh {
            continue;
        }

        // App icon (coloured square)
        for dy in 0..icon_size {
            let py = ay + dy;
            for dx in 0..icon_size {
                let px = ax + dx;
                let is_border = dy == 0
                    || dy == icon_size - 1
                    || dx == 0
                    || dx == icon_size - 1;
                let color = if is_border {
                    COLOR_PRIMARY
                } else {
                    app.color
                };
                let idx = (py as usize) * fb_w + px as usize;
                if idx < fb.len() {
                    fb[idx] = color;
                }
            }
        }

        // App label below icon
        let label_x = ax + 2;
        let label_y = ay + icon_size + 2;
        for (j, ch) in app.label.bytes().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            for gry in 0..12 {
                let py = label_y + gry;
                if py >= fbh {
                    continue;
                }
                for grx in 0..8 {
                    let px = label_x + (j as u32) * 8 + grx;
                    if px >= fbw {
                        continue;
                    }
                    if crate::font::get_glyph_pixel(ch, gry, grx) {
                        let idx = (py as usize) * fb_w + px as usize;
                        if idx < fb.len() {
                            fb[idx] = COLOR_TEXT;
                        }
                    }
                }
            }
        }
    }

    // Label
    render_label(fb, fbw, fbh, "Applications", (fbw / 2).saturating_sub(54), 10);
}

/// Render the timezone selector overlay.
pub fn render_timezone_selector(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    current_offset: i8,
) {
    let fb_w = fbw as usize;

    // ── Semi‑transparent black backdrop ──────────────────
    for row in 0..fbh {
        let off = (row as usize) * fb_w;
        for col in 0..fbw as usize {
            let bg = fb[off + col];
            let r = ((bg >> 16) & 0xFF) as u32;
            let g = ((bg >> 8) & 0xFF) as u32;
            let b = (bg & 0xFF) as u32;
            let r2 = (r * 2) / 5;
            let g2 = (g * 2) / 5;
            let b2 = (b * 2) / 5;
            fb[off + col] = (r2 << 16) | (g2 << 8) | b2;
        }
    }

    // ── Timezone entries ─────────────────────────────────
    let timezones: &[(&str, i8)] = &[
        ("UTC-12:00", -12),
        ("UTC-08:00  PST", -8),
        ("UTC-05:00  EST", -5),
        ("UTC+00:00  GMT", 0),
        ("UTC+01:00  CET", 1),
        ("UTC+03:00  MSK", 3),
        ("UTC+05:30  IST", 5),
        ("UTC+08:00  CST", 8),
        ("UTC+09:00  JST", 9),
        ("UTC+10:00  AEST", 10),
        ("UTC+12:00  NZST", 12),
    ];

    let entry_h = 24u32;
    let pad = 6u32;
    let start_y = 40u32;
    let max_label_chars = 16u32;  // "UTC-12:00  PST" = 14 chars
    let entry_w = max_label_chars * 8 + 16;  // 8px per char + padding

    for (i, (label, offset)) in timezones.iter().enumerate() {
        let ex = (fbw - entry_w) / 2;
        let ey = start_y + (i as u32) * (entry_h + pad);

        if ey + entry_h > fbh {
            continue;
        }

        // Highlight current timezone
        let bg_color = if *offset == current_offset {
            crate::compositor::COLOR_ACTIVE
        } else {
            0x333344u32
        };

        // Entry background
        for row in 0..entry_h {
            let py = ey + row;
            let rs = (py as usize) * fb_w + (ex as usize);
            fb[rs..rs + entry_w as usize].fill(bg_color);
        }

        // Entry label
        let lx = ex + 4;
        let ly = ey + 6;
        for (j, ch) in label.bytes().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            for gry in 0..12 {
                let py = ly + gry;
                if py >= fbh {
                    continue;
                }
                for grx in 0..8 {
                    let px = lx + (j as u32) * 8 + grx;
                    if px >= fbw {
                        continue;
                    }
                    if crate::font::get_glyph_pixel(ch, gry, grx) {
                        let idx = (py as usize) * fb_w + px as usize;
                        if idx < fb.len() {
                            fb[idx] = COLOR_TEXT;
                        }
                    }
                }
            }
        }
    }

    // Title
    render_label(fb, fbw, fbh, "Select Timezone", fbw / 2 - 60, 10);
}

/// Render a text label centred horizontally.
fn render_label(fb: &mut [u32], fbw: u32, _fbh: u32, text: &str, x: u32, y: u32) {
    let fb_w = fbw as usize;
    for (i, ch) in text.bytes().enumerate() {
        if ch < 32 || ch > 126 {
            continue;
        }
        for row in 0..14 {
            let py = y + row;
            for col in 0..8 {
                let px = x + (i as u32) * 8 + col;
                if px < fbw && crate::font::get_glyph_pixel(ch, row, col) {
                    let idx = (py as usize) * fb_w + px as usize;
                    if idx < fb.len() {
                        fb[idx] = COLOR_PRIMARY;
                    }
                }
            }
        }
    }
}