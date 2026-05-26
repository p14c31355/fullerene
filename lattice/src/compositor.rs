use crate::cursor::Cursor;
use crate::scene::Scene;
use crate::window::Window;

/// A minimal pixel target — just a buffer + dimensions.
///
/// The compositor writes pixels here and does **not** own presentation
/// timing, vsync, or swapchain logic.  Those belong in the kernel/runtime.
pub trait RenderTarget {
    fn buffer(&mut self) -> &mut [u32];
    fn dimensions(&self) -> (u32, u32);
}

/// Compositor — stateless, pure rendering.
///
/// The compositor accepts a `Scene` snapshot and a `RenderTarget`.
/// It does NOT own or manage:
/// - window state (WM's job)
/// - cursor position (input layer's job)
/// - presentation timing (kernel's job)
///
/// Supports partial redraw via `Scene::dirty_rects`: when populated,
/// only dirty areas are cleared and re‑composited.  An empty dirty rect
/// list triggers a full‑screen redraw (legacy fallback).
pub struct Compositor;

/// Title bar height in pixels.
pub const TITLE_BAR_HEIGHT: u32 = 20;
/// Border width around windows.
pub const WINDOW_BORDER: u32 = 2;

/// Active (focused) window border colour (soft blue).
const ACTIVE_BORDER_COLOR: u32 = 0x4A90D9;
/// Inactive window border colour (dark grey).
const INACTIVE_BORDER_COLOR: u32 = 0x555555;
/// Title bar background for focused windows.
const TITLE_BAR_ACTIVE: u32 = 0x3A7BD5;
/// Title bar background for unfocused windows.
const TITLE_BAR_INACTIVE: u32 = 0x444444;

impl Compositor {
    /// Composite `scene` onto `target`.
    ///
    /// Rendering order (bottom → top):
    /// 1. Background fill (`scene.bg_color`)
    /// 2. Each window in z‑order (back to front) — including title bar
    /// 3. Software cursor (if visible)
    ///
    /// When `scene.dirty_rects` is empty, the entire framebuffer is
    /// redrawn.  Otherwise only the union of dirty rects is updated.
    pub fn render(scene: &Scene<'_>, target: &mut dyn RenderTarget) {
        let (fb_width, fb_height) = target.dimensions();
        let framebuffer = target.buffer();

        // Determine effective draw region: if dirty rects are present, use
        // their bounding union; otherwise full framebuffer.
        let (dx, dy, dw, dh) = if scene.dirty_rects.is_empty() {
            (0u32, 0u32, fb_width, fb_height)
        } else {
            let mut merged = scene.dirty_rects[0];
            for r in &scene.dirty_rects[1..] {
                merged.merge(r);
            }
            (
                merged.x,
                merged.y,
                merged.width.min(fb_width.saturating_sub(merged.x)),
                merged.height.min(fb_height.saturating_sub(merged.y)),
            )
        };

        if dw == 0 || dh == 0 {
            return;
        }

        // 1. Restore background colour in dirty region
        // We do this by filling row-by-row only within the dirty rect.
        let fb_w = fb_width as usize;
        for row in dy..dy + dh {
            let row_start = (row as usize) * fb_w + (dx as usize);
            let row_end = row_start + (dw as usize);
            framebuffer[row_start..row_end].fill(scene.bg_color);
        }

        // 2. Draw windows back to front, clipped to dirty region
        for window in scene.windows {
            Self::draw_window_clipped(framebuffer, fb_width, fb_height, window, dx, dy, dw, dh);
        }

        // 3. Draw software cursor (only within dirty region)
        if let Some(cursor) = scene.cursor {
            if cursor.visible {
                Self::draw_cursor_clipped(framebuffer, fb_width, fb_height, cursor, dx, dy, dw, dh);
            }
        }
    }

    // ── Cursor drawing (with dirty‑rect clipping) ───────────────

    /// Draw the software cursor sprite, clipped to [dx, dx+dw) × [dy, dy+dh).
    fn draw_cursor_clipped(
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        cursor: &Cursor,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    ) {
        let pixels = Cursor::shape();
        let cw = Cursor::SIZE as i32;
        let ch = Cursor::SIZE as i32;

        let dst_x = cursor.x - Cursor::HOTSPOT_X;
        let dst_y = cursor.y - Cursor::HOTSPOT_Y;

        // Clamp to screen
        let src_x_start = 0i32.max(-dst_x);
        let src_y_start = 0i32.max(-dst_y);
        let src_x_end = cw.min(fb_width as i32 - dst_x);
        let src_y_end = ch.min(fb_height as i32 - dst_y);

        if src_x_start >= src_x_end || src_y_start >= src_y_end {
            return;
        }

        let cw_usize = cw as usize;
        let clip_end_x = (clip_x + clip_w) as i32;
        let clip_end_y = (clip_y + clip_h) as i32;

        for row in src_y_start..src_y_end {
            let sy = row as usize;
            let dy = dst_y + row;

            // Skip rows outside dirty rect vertically
            if dy < clip_y as i32 || dy >= clip_end_y {
                continue;
            }

            let src_row_base = sy * cw_usize;
            let dst_row_base = dy as usize * (fb_width as usize);

            for col in src_x_start..src_x_end {
                let dx_abs = dst_x + col;
                if dx_abs < clip_x as i32 || dx_abs >= clip_end_x {
                    continue;
                }
                let s = pixels[src_row_base + col as usize];
                if s != 0 {
                    framebuffer[dst_row_base + dx_abs as usize] = s;
                }
            }
        }
    }

    /// Back‑compat helper — full‑screen cursor draw (used by tests).
    fn draw_cursor(framebuffer: &mut [u32], fb_width: u32, fb_height: u32, cursor: &Cursor) {
        Self::draw_cursor_clipped(framebuffer, fb_width, fb_height, cursor, 0, 0, fb_width, fb_height);
    }

    // ── Window drawing (with dirty‑rect clipping) ───────────────

    /// Draw a single window clipped to [clip_x, clip_x+clip_w) × [clip_y, clip_y+clip_h).
    fn draw_window_clipped(
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        window: &Window,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    ) {
        // Draw title bar and border decorations
        if window.title.is_some() {
            Self::draw_title_bar(
                framebuffer,
                fb_width,
                fb_height,
                window,
                clip_x,
                clip_y,
                clip_w,
                clip_h,
            );
        }

        let src = &window.surface;
        let title_offset = if window.title.is_some() { TITLE_BAR_HEIGHT as i32 } else { 0i32 };

        // Surface content is below the title bar
        let win_draw_x = window.x;
        let win_draw_y = window.y + title_offset;

        // Source bounds
        let src_x_start = 0i32.max(-win_draw_x);
        let src_y_start = 0i32.max(-win_draw_y);
        let src_x_end = (src.width() as i64)
            .min((fb_width as i64).saturating_sub(win_draw_x as i64))
            .max(0) as i32;
        let src_y_end = (src.height() as i64)
            .min((fb_height as i64).saturating_sub(win_draw_y as i64))
            .max(0) as i32;

        if src_x_start >= src_x_end || src_y_start >= src_y_end {
            return;
        }

        let clip_end_x = (clip_x + clip_w) as i32;
        let clip_end_y = (clip_y + clip_h) as i32;
        let src_pixels = src.pixels();

        for src_row in src_y_start..src_y_end {
            let dst_row = (win_draw_y + src_row) as i32;
            if dst_row < clip_y as i32 || dst_row >= clip_end_y {
                continue;
            }

            let src_base = (src_row as usize) * (src.width() as usize);
            let dst_base = (dst_row as usize) * (fb_width as usize);

            for src_col in src_x_start..src_x_end {
                let dst_col = win_draw_x + src_col;
                if dst_col < clip_x as i32 || dst_col >= clip_end_x {
                    continue;
                }
                framebuffer[dst_base + dst_col as usize] =
                    src_pixels[src_base + src_col as usize];
            }
        }
    }

    // ── Title bar / window decoration ────────────────────────────

    /// Draw a window's title bar, border, and shadow within the clip rect.
    fn draw_title_bar(
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        window: &Window,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    ) {
        let title = window.title.as_ref().map(|t| t.as_str()).unwrap_or("");
        let focused = window.focused;

        let bar_color = if focused { TITLE_BAR_ACTIVE } else { TITLE_BAR_INACTIVE };
        let border_color = if focused { ACTIVE_BORDER_COLOR } else { INACTIVE_BORDER_COLOR };

        let win_w = window.width + WINDOW_BORDER * 2;
        let win_h = window.height + TITLE_BAR_HEIGHT + WINDOW_BORDER * 2;

        let clip_end_x = (clip_x + clip_w) as i32;
        let clip_end_y = (clip_y + clip_h) as i32;
        let fb_w = fb_width as i32;

        // Draw shadow (2 px offset, semi‑transparent dark)
        {
            if let Some(shadow) = &window.shadow_surface {
                let sh_x = window.x + 2 - WINDOW_BORDER as i32;
                let sh_y = window.y + 2 - WINDOW_BORDER as i32;
                for sy in 0..shadow.height() {
                    let dy_abs = sh_y + sy as i32;
                    if dy_abs < clip_y as i32 || dy_abs >= clip_end_y || dy_abs >= fb_height as i32 {
                        continue;
                    }
                    for sx in 0..shadow.width() {
                        let dx_abs = sh_x + sx as i32;
                        if dx_abs < clip_x as i32 || dx_abs >= clip_end_x || dx_abs >= fb_w {
                            continue;
                        }
                        let sp = shadow.get_pixel(sx, sy).unwrap_or(0);
                        if sp & 0xFF000000 != 0 {
                            let dst = &mut framebuffer[(dy_abs as usize) * (fb_width as usize) + dx_abs as usize];
                            // Blend shadow with background
                            let bg = *dst;
                            let alpha = ((sp >> 24) & 0xFF) as u32;
                            let inv = 255 - alpha;
                            let r = (((sp >> 16) & 0xFF) * alpha + ((bg >> 16) & 0xFF) * inv) / 255;
                            let g = (((sp >> 8) & 0xFF) * alpha + ((bg >> 8) & 0xFF) * inv) / 255;
                            let b = (((sp) & 0xFF) * alpha + ((bg) & 0xFF) * inv) / 255;
                            *dst = (r << 16) | (g << 8) | b;
                        }
                    }
                }
            }
        }

        // Draw border
        for row in 0..win_h as i32 {
            let dy_abs = window.y - WINDOW_BORDER as i32 + row;
            if dy_abs < clip_y as i32 || dy_abs >= clip_end_y || dy_abs >= fb_height as i32 {
                continue;
            }
            for col in 0..win_w as i32 {
                let dx_abs = window.x - WINDOW_BORDER as i32 + col;
                if dx_abs < clip_x as i32 || dx_abs >= clip_end_x || dx_abs >= fb_w {
                    continue;
                }
                let is_border = row < WINDOW_BORDER as i32
                    || row >= (win_h - WINDOW_BORDER) as i32
                    || col < WINDOW_BORDER as i32
                    || col >= (win_w - WINDOW_BORDER) as i32;
                if is_border {
                    framebuffer[(dy_abs as usize) * (fb_width as usize) + dx_abs as usize] = border_color;
                }
            }
        }

        // Draw title bar background
        let bar_x = window.x;
        let bar_y = window.y;
        for row in 0..TITLE_BAR_HEIGHT as i32 {
            let dy_abs = bar_y + row;
            if dy_abs < clip_y as i32 || dy_abs >= clip_end_y || dy_abs >= fb_height as i32 {
                continue;
            }
            for col in 0..window.width as i32 {
                let dx_abs = bar_x + col;
                if dx_abs < clip_x as i32 || dx_abs >= clip_end_x || dx_abs >= fb_w {
                    continue;
                }
                framebuffer[(dy_abs as usize) * (fb_width as usize) + dx_abs as usize] = bar_color;
            }
        }

        // Draw title text (using simple bitmap font)
        Self::draw_title_text(framebuffer, fb_width, fb_height, window, title, clip_x, clip_y, clip_w, clip_h);
    }

    /// Draw title bar text (simple bitmap font, glyphs 32–126).
    fn draw_title_text(
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        window: &Window,
        text: &str,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    ) {
        let clip_end_x = (clip_x + clip_w) as i32;
        let clip_end_y = (clip_y + clip_h) as i32;
        let fb_w = fb_width as i32;

        let text_x = window.x + 4; // padding from left
        let text_y = window.y + 4; // vertical centering in title bar (approximate)

        for (i, ch) in text.bytes().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            let glyph_x = text_x + (i as i32) * 8;
            let glyph_y = text_y;

            for row in 0..12 {
                let dy_abs = glyph_y + row;
                if dy_abs < clip_y as i32 || dy_abs >= clip_end_y || dy_abs >= fb_height as i32 {
                    continue;
                }
                for col in 0..8 {
                    let dx_abs = glyph_x + col;
                    if dx_abs < clip_x as i32 || dx_abs >= clip_end_x || dx_abs >= fb_w {
                        continue;
                    }
                    if crate::font::get_glyph_pixel(ch, row as u32, col as u32) {
                        framebuffer[(dy_abs as usize) * (fb_width as usize) + dx_abs as usize] = 0xFFFFFF;
                    }
                }
            }
        }
    }

    /// Full‑screen window draw (back‑compat for tests).
    fn draw_window(framebuffer: &mut [u32], fb_width: u32, fb_height: u32, window: &Window) {
        Self::draw_window_clipped(framebuffer, fb_width, fb_height, window, 0, 0, fb_width, fb_height);
    }
}
