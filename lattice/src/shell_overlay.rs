//! Shell overlay rendering — Task Overview & App Grid.
//!
//! Renders the GNOME-style Activities overlay directly onto the framebuffer.
//! Consists of:
//! - Semi‑transparent black backdrop
//! - Window thumbnails (TaskOverview)
//! - App launcher grid (AppGrid)
//!
//! All rendering is done in software — no GPU / 3D acceleration required.

use crate::compositor::{COLOR_PRIMARY, COLOR_TEXT, dim_color};
use crate::painter::Painter;
use crate::window::Window;

/// Shell state — determines which overlay to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellState {
    Desktop,
    TaskOverview,
    AppGrid,
    TimeZoneSelector,
}

/// Apply semi‑transparent black (~60% dim) to the full framebuffer.
fn dim_backdrop(fb: &mut [u32], fbw: u32, fbh: u32, stride: usize) {
    for row in 0..fbh as usize {
        let off = row * stride;
        for col in 0..fbw as usize {
            fb[off + col] = dim_color(fb[off + col]);
        }
    }
}

/// Render text at `(x, y)` using the Painter's TTF renderer (bitmap fallback).
fn render_text(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    _stride: usize,
    text: &str,
    x: u32,
    y: u32,
    color: u32,
) {
    let mut p = Painter::new(fb, fbw, fbh);
    p.draw_text(x as i32, y as i32, text, color, 13.0);
}

/// Render the Task Overview overlay on top of the current framebuffer.
pub fn render_task_overview(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    fb_stride: u32,
    windows: &[Window],
) {
    let stride = fb_stride as usize;
    if stride < fbw as usize || fb.len() < stride.checked_mul(fbh as usize).unwrap_or(usize::MAX) {
        return;
    }
    dim_backdrop(fb, fbw, fbh, stride);

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
                let idx = ((ty + dy) as usize) * stride + (tx + dx) as usize;
                if idx < fb.len() {
                    // Draw border
                    let is_border = dy == 0 || dy == thumb_h - 1 || dx == 0 || dx == thumb_w - 1;
                    fb[idx] = if is_border { COLOR_PRIMARY } else { color };
                }
            }
        }

        // Window title below thumbnail
        let title = window
            .title
            .as_ref()
            .map(|t| t.as_str())
            .unwrap_or("Window");
        render_text(
            fb,
            fbw,
            fbh,
            stride,
            title,
            tx + 2,
            ty + thumb_h + 3,
            COLOR_TEXT,
        );
    }

    // ── "Activities" label at top ──────────────────────────
    render_label(
        fb,
        fbw,
        fbh,
        fb_stride,
        "Task Overview",
        (fbw / 2).saturating_sub(52),
        10,
    );
}

/// Render the App Grid overlay.
pub fn render_app_grid(fb: &mut [u32], fbw: u32, fbh: u32, fb_stride: u32) {
    let stride = fb_stride as usize;
    if stride < fbw as usize || fb.len() < stride.checked_mul(fbh as usize).unwrap_or(usize::MAX) {
        return;
    }
    dim_backdrop(fb, fbw, fbh, stride);

    if crate::style::kind() == crate::common::ShellKind::Prism {
        render_prism_start_panel(fb, fbw, fbh, stride);
        return;
    }

    // ── App launcher grid ─────────────────────────────────
    struct AppEntry {
        label: &'static str,
        icon: &'static crate::icon::SvgIcon,
    }

    let apps: &[AppEntry] = &[
        AppEntry {
            label: "Shell",
            icon: &crate::icon::ICON_SHELL,
        },
        AppEntry {
            label: "Terminal",
            icon: &crate::icon::ICON_TERMINAL,
        },
        AppEntry {
            label: "Editor",
            icon: &crate::icon::ICON_EDITOR,
        },
        AppEntry {
            label: "Clock",
            icon: &crate::icon::ICON_CLOCK,
        },
        AppEntry {
            label: "Settings",
            icon: &crate::icon::ICON_SETTINGS,
        },
        AppEntry {
            label: "File Mgr",
            icon: &crate::icon::ICON_FILES,
        },
        AppEntry {
            label: "About",
            icon: &crate::icon::ICON_ABOUT,
        },
    ];

    let icon_size = 64u32;
    let pad = 24u32;
    let label_h = 18u32;
    let columns = (fbw / (icon_size + pad)).max(1);
    let start_y = 60u32;

    for (i, app) in apps.iter().enumerate() {
        let col = (i as u32) % columns;
        let row = (i as u32) / columns;
        let ax = (pad + col * (icon_size + pad)) as i32;
        let ay = (start_y + row * (icon_size + label_h + pad)) as i32;

        if ax + icon_size as i32 > fbw as i32 || ay + (icon_size + label_h) as i32 > fbh as i32 {
            continue;
        }

        // SVG icon (direct framebuffer blit, no heap allocation)
        app.icon.blit_into(fb, fbw, stride, ax, ay);

        // App label below icon
        render_text(
            fb,
            fbw,
            fbh,
            stride,
            app.label,
            (ax + 2) as u32,
            (ay + icon_size as i32 + 2) as u32,
            COLOR_TEXT,
        );
    }

    // Label
    render_label(
        fb,
        fbw,
        fbh,
        fb_stride,
        "Applications",
        (fbw / 2).saturating_sub(54),
        10,
    );
}

/// Windows-like Start panel used by Prism. It intentionally uses the same
/// seven applications as the overlay input handler, so the visual target and
/// the hit-test geometry cannot drift apart.
fn render_prism_start_panel(fb: &mut [u32], fbw: u32, fbh: u32, stride: usize) {
    let panel_w = 520u32.min(fbw.saturating_sub(48));
    let panel_h = 500u32.min(fbh.saturating_sub(80));
    let panel_x = fbw.saturating_sub(panel_w) / 2;
    let panel_y = fbh.saturating_sub(panel_h) / 2 + 18;
    let colors = crate::style::current().palette;
    let mut painter = Painter::new(fb, fbw, fbh);
    painter.draw_shadow(
        panel_x as i32,
        panel_y as i32,
        panel_w,
        panel_h,
        18,
        2,
        12,
        colors.window_shadow,
    );
    painter.rounded_rect(
        panel_x as i32,
        panel_y as i32,
        panel_w,
        panel_h,
        18,
        0xFFFFFF,
    );
    painter.rounded_rect(
        panel_x as i32 + 1,
        panel_y as i32 + 1,
        panel_w.saturating_sub(2),
        panel_h.saturating_sub(2),
        17,
        0xFFFFFF,
    );
    painter.rounded_rect(
        panel_x as i32 + 24,
        panel_y as i32 + 22,
        panel_w.saturating_sub(48),
        36,
        8,
        0xF2F4F7,
    );
    painter.draw_text(
        panel_x as i32 + 38,
        panel_y as i32 + 33,
        "Search apps, settings, and documents",
        0x667085,
        13.0,
    );
    painter.draw_text(
        panel_x as i32 + 28,
        panel_y as i32 + 82,
        "Pinned",
        0x1F2937,
        16.0,
    );

    let apps: &[(&str, &crate::icon::SvgIcon)] = &[
        ("Shell", &crate::icon::ICON_SHELL),
        ("Terminal", &crate::icon::ICON_TERMINAL),
        ("Editor", &crate::icon::ICON_EDITOR),
        ("Clock", &crate::icon::ICON_CLOCK),
        ("Settings", &crate::icon::ICON_SETTINGS),
        ("File Mgr", &crate::icon::ICON_FILES),
        ("About", &crate::icon::ICON_ABOUT),
    ];
    for (index, (label, icon)) in apps.iter().enumerate() {
        let Some((x, y, _, _)) = app_grid_item_rect(index, fbw, fbh) else {
            continue;
        };
        icon.blit_scaled_into(painter.fb, fbw, stride, x + 12, y + 4, 48);
        painter.draw_text(x + 8, y + 58, label, 0x344054, 12.0);
    }
    painter.draw_text(
        panel_x as i32 + 28,
        panel_y as i32 + panel_h as i32 - 30,
        "All apps  >",
        colors.primary,
        13.0,
    );
}

/// Return the clickable rectangle for an AppGrid entry in the active shell.
pub fn app_grid_item_rect(index: usize, fbw: u32, fbh: u32) -> Option<(i32, i32, u32, u32)> {
    if crate::style::kind() == crate::common::ShellKind::Prism {
        if index >= 7 {
            return None;
        }
        let panel_w = 520u32.min(fbw.saturating_sub(48));
        let panel_h = 500u32.min(fbh.saturating_sub(80));
        let panel_x = fbw.saturating_sub(panel_w) / 2;
        let panel_y = fbh.saturating_sub(panel_h) / 2 + 18;
        return Some((
            panel_x as i32 + 24 + (index % 5) as i32 * 96,
            panel_y as i32 + 106 + (index / 5) as i32 * 100,
            72,
            80,
        ));
    }

    let icon_size = 64u32;
    let pad = 24u32;
    let columns = (fbw / (icon_size + pad)).max(1);
    let col = index as u32 % columns;
    let row = index as u32 / columns;
    Some((
        (pad + col * (icon_size + pad)) as i32,
        (60 + row * (icon_size + 18 + pad)) as i32,
        icon_size,
        icon_size + 18,
    ))
}

/// Render the timezone selector overlay.
pub fn render_timezone_selector(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    fb_stride: u32,
    current_offset: i8,
) {
    let stride = fb_stride as usize;
    if stride < fbw as usize || fb.len() < stride.checked_mul(fbh as usize).unwrap_or(usize::MAX) {
        return;
    }
    dim_backdrop(fb, fbw, fbh, stride);

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
    let max_label_chars = 16u32; // "UTC-12:00  PST" = 14 chars
    let entry_w = max_label_chars * 8 + 16; // 8px per char + padding

    for (i, (label, offset)) in timezones.iter().enumerate() {
        let ex = fbw.saturating_sub(entry_w) / 2;
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
            let rs = (py as usize) * stride + (ex as usize);
            let start = rs;
            let end = start.saturating_add(entry_w as usize).min(fb.len());
            if start < end {
                fb[start..end].fill(bg_color);
            }
        }

        // Entry label
        render_text(fb, fbw, fbh, stride, label, ex + 4, ey + 6, COLOR_TEXT);
    }

    // Title
    render_label(
        fb,
        fbw,
        fbh,
        fb_stride,
        "Select Timezone",
        (fbw / 2).saturating_sub(60),
        10,
    );
}

/// Render a text label centred horizontally using Painter TTF.
fn render_label(fb: &mut [u32], fbw: u32, fbh: u32, _fb_stride: u32, text: &str, x: u32, y: u32) {
    let mut p = Painter::new(fb, fbw, fbh);
    p.draw_text(x as i32, y as i32, text, COLOR_PRIMARY, 15.0);
}
