use crate::cursor::Cursor;
use crate::scene::{DirtyRect, OverlayRect, Scene};
use crate::window::Window;

pub trait RenderTarget {
    fn buffer(&mut self) -> &mut [u32];
    fn dimensions(&self) -> (u32, u32);
}

pub struct Compositor;

pub const TITLE_BAR_HEIGHT: u32 = 20;
pub const WINDOW_BORDER: u32 = 2;

// UI padding constants
pub const WINDOW_PADDING: u32 = 4;
pub const TASKBAR_PADDING: u32 = 4;
pub const BUTTON_PADDING: u32 = 2;

// ── Fullerene Color Palette ──────────────────────────────────
pub const COLOR_BG: u32 = 0x1a1a2e;
pub const COLOR_SURFACE: u32 = 0x16213e;
pub const COLOR_PRIMARY: u32 = 0x4A90D9;
pub const COLOR_ACTIVE: u32 = 0x3A7BD5;
pub const COLOR_TEXT: u32 = 0xE0E0E0;
pub const COLOR_MUTED: u32 = 0x888888;
pub const COLOR_BORDER_ACTIVE: u32 = 0x4A90D9;
pub const COLOR_BORDER_INACTIVE: u32 = 0x555555;
pub const COLOR_TITLE_ACTIVE: u32 = 0x3A7BD5;
pub const COLOR_TITLE_INACTIVE: u32 = 0x444444;
pub const COLOR_ACCENT: u32 = 0xE6A817;
pub const COLOR_DANGER: u32 = 0xD94A4A;

// ── Dim lookup table ────────────────────────────────────────
/// Pre‑computed dim table: `(v * 2) / 5` for each 0..=255 channel value.
static DIM_TABLE: [u32; 256] = {
    let mut tbl = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        tbl[i as usize] = (i * 2) / 5;
        i += 1;
    }
    tbl
};

/// Apply dim (~40% luminance) to a colour using the pre‑computed table.
#[inline]
pub(crate) fn dim_color(color: u32) -> u32 {
    let r = DIM_TABLE[((color >> 16) & 0xFF) as usize];
    let g = DIM_TABLE[((color >> 8) & 0xFF) as usize];
    let b = DIM_TABLE[(color & 0xFF) as usize];
    (r << 16) | (g << 8) | b
}

/// Alpha-blend a source pixel over a destination pixel, writing the result.
/// Returns the blending was performed (useful for callers to `continue` in
/// tight loops).
macro_rules! alpha_blend {
    ($dst:expr, $src:expr) => {{
        let s = $src;
        let a = ((s >> 24) & 0xFF) as u32;
        if a == 255 {
            $dst = s;
            false // fully opaque — no further blending needed
        } else if a > 0 {
            let bg = $dst;
            let ia = 255 - a;
            let r = (((s >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
            let g = (((s >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
            let b = ((s & 0xFF) * a + (bg & 0xFF) * ia) / 255;
            $dst = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
            false
        } else {
            true // fully transparent — caller should `continue`
        }
    }};
}

// ── Title bar button caches ─────────────────────────────────
/// Pre‑rendered 14×14 close button (red background + white X).
static CLOSE_BUTTON_CACHE: [u32; 14 * 14] = build_close_button();
/// Pre‑rendered 14×14 maximize button (green background + white square).
static MAXIMIZE_BUTTON_CACHE: [u32; 14 * 14] = build_maximize_button();
/// Pre‑rendered 14×14 minimize button (amber background + white line).
static MINIMIZE_BUTTON_CACHE: [u32; 14 * 14] = build_minimize_button();

/// Fill a 14x14 buffer with a solid colour.
const fn fill_14x14(buf: &mut [u32; 14 * 14], color: u32) {
    let mut i = 0;
    while i < 14 * 14 {
        buf[i] = color;
        i += 1;
    }
}

const fn build_close_button() -> [u32; 14 * 14] {
    let mut buf = [0u32; 14 * 14];
    fill_14x14(&mut buf, COLOR_DANGER);
    let mut o = 0;
    while o < 8 {
        buf[(3 + o) * 14 + (3 + o)] = 0xFFFFFF;
        buf[(10 - o) * 14 + (3 + o)] = 0xFFFFFF;
        o += 1;
    }
    buf
}

const fn build_maximize_button() -> [u32; 14 * 14] {
    let mut buf = [0u32; 14 * 14];
    fill_14x14(&mut buf, 0x338833);
    let mut r = 3;
    while r < 11 {
        let mut c = 3;
        while c < 11 {
            if r == 3 || r == 10 || c == 3 || c == 10 {
                buf[r * 14 + c] = 0xFFFFFF;
            }
            c += 1;
        }
        r += 1;
    }
    buf
}

const fn build_minimize_button() -> [u32; 14 * 14] {
    let mut buf = [0u32; 14 * 14];
    fill_14x14(&mut buf, COLOR_ACCENT);
    let mut c = 3;
    while c < 11 {
        buf[10 * 14 + c] = 0xFFFFFF;
        c += 1;
    }
    buf
}

/// Blit a 14×14 cached button onto the framebuffer at (bx, by).
#[inline]
fn blit_button(fb: &mut [u32], fbw: u32, cache: &[u32; 14 * 14], bx: i32, by: i32) {
    let fb_w = fbw as usize;
    let fb_len = fb.len();
    let fbh = (fb_len / fb_w) as i32;
    if fbh == 0 {
        return;
    }
    let fbw_i32 = fbw as i32;
    for row in 0..14 {
        let da = by + row;
        if da < 0 || da >= fbh {
            continue;
        }
        let row_base = (da as usize) * fb_w;
        for col in 0..14 {
            let dxa = bx + col;
            if dxa < 0 || dxa >= fbw_i32 {
                continue;
            }
            let idx = row_base + dxa as usize;
            if idx < fb_len {
                fb[idx] = cache[(row as usize) * 14 + col as usize];
            }
        }
    }
}

fn draw_cursor_impl(
    fb: &mut [u32],
    fbw: u32,
    fbh: u32,
    cur: &Cursor,
    cx: u32,
    cy: u32,
    cw: u32,
    ch: u32,
) {
    let pixels = Cursor::shape();
    let sz = Cursor::SIZE as i32;
    let dst_x = cur.x - Cursor::HOTSPOT_X;
    let dst_y = cur.y - Cursor::HOTSPOT_Y;
    let sx_s = 0i32.max(-dst_x);
    let sy_s = 0i32.max(-dst_y);
    let sx_e = sz.min(fbw as i32 - dst_x);
    let sy_e = sz.min(fbh as i32 - dst_y);
    if sx_s >= sx_e || sy_s >= sy_e {
        return;
    }
    let cex = (cx + cw) as i32;
    let cey = (cy + ch) as i32;
    for row in sy_s..sy_e {
        let dy = dst_y + row;
        if dy < cy as i32 || dy >= cey {
            continue;
        }
        for col in sx_s..sx_e {
            let dx = dst_x + col;
            if dx < cx as i32 || dx >= cex {
                continue;
            }
            let s = pixels[(row as usize) * (sz as usize) + col as usize];
            if s == 0 {
                continue;
            }
            alpha_blend!(fb[(dy as usize) * (fbw as usize) + dx as usize], s);
        }
    }
}

// ── FPS overlay ─────────────────────────────────────────────
use core::sync::atomic::{AtomicU64, Ordering};

static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_FPS_TICK: AtomicU64 = AtomicU64::new(0);
static CURRENT_FPS_X100: AtomicU64 = AtomicU64::new(0);

/// Total draw calls per frame (atomic for async access).
static DRAW_CALLS: AtomicU64 = AtomicU64::new(0);
/// Estimated time spent in render (ticks).
static RENDER_TICKS: AtomicU64 = AtomicU64::new(0);

// ── Inline formatting helpers (no heap) ────────────────────

/// Write a byte slice into `buf` at `pos`. Returns the number of bytes written.
fn write_str(buf: &mut [u8; 32], pos: &mut usize, s: &[u8]) -> usize {
    let n = s.len().min(buf.len().saturating_sub(*pos));
    buf[*pos..*pos + n].copy_from_slice(&s[..n]);
    *pos += n;
    n
}

/// Write a single byte into the buffer.
fn write_byte(buf: &mut [u8; 32], pos: &mut usize, b: u8) -> usize {
    if *pos < buf.len() {
        buf[*pos] = b;
        *pos += 1;
        1
    } else {
        0
    }
}

/// Write a u64 as decimal, padded to at least `min_digits` (0 = natural width).
fn write_u64_fixed(buf: &mut [u8; 32], pos: &mut usize, mut v: u64, min_digits: usize) -> usize {
    let mut tmp = [0u8; 20];
    let mut i = 0usize;
    if v == 0 {
        tmp[i] = b'0';
        i += 1;
    }
    while v > 0 {
        tmp[i] = b'0' + (v % 10) as u8;
        i += 1;
        v /= 10;
    }
    // Pad to min_digits
    while i < min_digits {
        tmp[i] = b'0';
        i += 1;
    }
    // tmp has digits reversed — write in correct order
    let start = *pos;
    for j in (0..i).rev() {
        write_byte(buf, pos, tmp[j]);
    }
    *pos - start
}

pub fn notify_frame_presented(now_tick: u64) {
    let fc = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    let last = LAST_FPS_TICK.load(Ordering::Relaxed);
    // Update FPS every FRAMES_PER_UPDATE frames, but also enforce a minimum
    // time between updates so low-framerate environments don't show stale data.
    const FRAMES_PER_UPDATE: u64 = 30;
    if now_tick > last && (fc as u64 % FRAMES_PER_UPDATE == 0) {
        let ticks_since = now_tick.saturating_sub(last);
        if ticks_since > 0 {
            let fps = FRAMES_PER_UPDATE
                .saturating_mul(100)
                .saturating_div(ticks_since);
            CURRENT_FPS_X100.store(fps, Ordering::Relaxed);
        }
        LAST_FPS_TICK.store(now_tick, Ordering::Relaxed);
    }
}

pub fn current_fps_x100() -> u64 {
    CURRENT_FPS_X100.load(Ordering::Relaxed)
}

/// Return the number of draw calls in the last rendered frame.
pub fn draw_calls_last_frame() -> u64 {
    DRAW_CALLS.load(Ordering::Relaxed)
}

/// Return the estimated render time in ticks for the last frame.
pub fn render_ticks_last_frame() -> u64 {
    RENDER_TICKS.load(Ordering::Relaxed)
}

fn inc_draw_calls() {
    DRAW_CALLS.fetch_add(1, Ordering::Relaxed);
}

impl Compositor {
    /// Render the scene into the target using layered rendering.
    ///
    /// Layer order (back to front):
    /// 1. Desktop background
    /// 2. Windows (z-ordered, last = topmost)
    /// 3. Overlays (menus, tooltips)
    /// 4. System UI (cursor, FPS debug overlay, taskbar)
    ///
    /// Returns the bounding box that was actually drawn (clipped dirty rect),
    /// so the caller can perform a partial blit instead of a full framebuffer copy.
    pub fn render(scene: &Scene<'_>, target: &mut dyn RenderTarget) -> (u32, u32, u32, u32) {
        // Reset draw-call counter
        DRAW_CALLS.store(0, Ordering::Relaxed);

        let (fb_width, fb_height) = target.dimensions();
        let framebuffer = target.buffer();

        // When there are no dirty rects, render the full framebuffer.
        let merged = if scene.dirty_rects.is_empty() {
            DirtyRect::full(fb_width, fb_height)
        } else {
            let mut m = scene.dirty_rects[0];
            for r in &scene.dirty_rects[1..] {
                m.merge(r);
            }
            m
        };
        let dx = merged.x;
        let dy = merged.y;
        let dw = merged.width.min(fb_width.saturating_sub(merged.x));
        let dh = merged.height.min(fb_height.saturating_sub(merged.y));
        if dw == 0 || dh == 0 {
            return (0, 0, 0, 0);
        }

        // ── Layer 0: Desktop background (wallpaper) + icons ───
        crate::wallpaper::render_wallpaper(framebuffer, fb_width, fb_height, dx, dy, dw, dh);

        // Draw desktop icons on the background, behind windows
        if let Some(icons) = scene.desktop_icons {
            icons.render(framebuffer, fb_width, fb_height, dx, dy, dw, dh);
        }

        // ── Layer 1: Windows ─────────────────────────────
        for window in scene.windows {
            // Skip minimized windows
            if window.minimized {
                continue;
            }
            Self::draw_window_clipped(framebuffer, fb_width, fb_height, window, dx, dy, dw, dh);
        }
        inc_draw_calls();

        // ── Layer 2: Overlays ────────────────────────────
        if !scene.overlays.is_empty() {
            for ov in scene.overlays {
                Self::draw_overlay_clipped(framebuffer, fb_width, fb_height, ov, dx, dy, dw, dh);
            }
            inc_draw_calls();
        }

        // Draw menu text on top of overlay rectangles
        if let Some(menu) = scene.active_menu {
            menu.render_text(framebuffer, fb_width, fb_height, fb_width);
            inc_draw_calls();
        }

        // ── Layer 3: System UI ───────────────────────────
        // Taskbar
        if let Some(tb) = scene.taskbar {
            let bar_y = fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT);
            let bar_rect = DirtyRect::new(0, bar_y, fb_width, crate::taskbar::TASKBAR_HEIGHT);
            let clip = DirtyRect::new(dx, dy, dw, dh);
            if bar_rect.intersects(&clip) {
                tb.render(framebuffer, fb_width, fb_height);
            }
            inc_draw_calls();
        }

        // Cursor — drawn in the back‑buffer so the compositor owns
        // cursor rendering exclusively.  The dirty rect for the old and
        // new cursor positions is already pushed by prepare_frame(), so
        // the compositor redraws both the restored old area and the new
        // cursor position in a single pass.
        if let Some(c) = scene.cursor {
            if c.visible {
                Self::draw_cursor_clipped(framebuffer, fb_width, fb_height, c, dx, dy, dw, dh);
                inc_draw_calls();
            }
        }

        // Debug overlay (FPS + draw calls)
        Self::draw_debug_overlay(framebuffer, fb_width, fb_height);
        inc_draw_calls();

        // Return the drawn bounding box for partial blit.
        let max_x = (dx + dw).min(fb_width);
        let max_y = (dy + dh).min(fb_height);
        (dx, dy, max_x - dx, max_y - dy)
    }

    // ── Overlay drawing ────────────────────────────────────

    fn draw_overlay_clipped(
        fb: &mut [u32],
        fbw: u32,
        fbh: u32,
        ov: &OverlayRect,
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) {
        let ox = ov.x as i32;
        let oy = ov.y as i32;
        let ow = ov.width as i32;
        let oh = ov.height as i32;
        let cex = (cx + cw) as i32;
        let cey = (cy + ch) as i32;
        for row in 0..oh {
            let da = oy + row;
            if da < cy as i32 || da >= cey || da >= fbh as i32 {
                continue;
            }
            for col in 0..ow {
                let dxa = ox + col;
                if dxa < cx as i32 || dxa >= cex || dxa >= fbw as i32 {
                    continue;
                }
                // Border (1px)
                let is_border = row == 0 || row == oh - 1 || col == 0 || col == ow - 1;
                let color = if is_border {
                    COLOR_BORDER_ACTIVE
                } else {
                    ov.color
                };
                let idx = (da as usize) * (fbw as usize) + dxa as usize;
                fb[idx] = color;
            }
        }
    }

    fn draw_debug_overlay(fb: &mut [u32], fbw: u32, _fbh: u32) {
        let fps = current_fps_x100();
        if fps == 0 {
            return;
        }
        let dc = draw_calls_last_frame();
        // Inline formatting to avoid heap allocation
        let mut buf = [0u8; 32];
        let mut pos = 0usize;
        let _ = write_str(&mut buf, &mut pos, b"FPS:");
        write_u64_fixed(&mut buf, &mut pos, fps / 100, 1);
        let _ = write_byte(&mut buf, &mut pos, b'.');
        write_u64_fixed(&mut buf, &mut pos, fps % 100, 2);
        let _ = write_str(&mut buf, &mut pos, b" DC:");
        write_u64_fixed(&mut buf, &mut pos, dc, 0);
        let text = &buf[..pos.min(32)];

        let x = fbw.saturating_sub(150);
        let y = 4u32;
        for (i, &ch) in text.iter().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            let gl = crate::font::glyph_fast(ch);
            for row in 0..12 {
                let py = y + row;
                for col in 0..8 {
                    let px = x + (i as u32) * 8 + col;
                    if px < fbw && py < _fbh && gl.pixel(row, col) {
                        fb[(py * fbw + px) as usize] = COLOR_ACCENT;
                    }
                }
            }
        }
    }

    // ── Cursor ────────────────────────────────────────────

    pub fn draw_cursor_direct(fb: &mut [u32], fbw: u32, fbh: u32, cur: &Cursor) {
        draw_cursor_impl(fb, fbw, fbh, cur, 0, 0, fbw, fbh);
    }
    fn draw_cursor_clipped(
        fb: &mut [u32],
        fbw: u32,
        fbh: u32,
        cur: &Cursor,
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) {
        draw_cursor_impl(fb, fbw, fbh, cur, cx, cy, cw, ch);
    }

    // ── Window drawing ────────────────────────────────────

    fn draw_window_clipped(
        fb: &mut [u32],
        fbw: u32,
        fbh: u32,
        win: &Window,
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) {
        if win.title.is_some() {
            Self::draw_title_bar(fb, fbw, fbh, win, cx, cy, cw, ch);
        }
        let src = &win.surface;
        let to = if win.title.is_some() {
            TITLE_BAR_HEIGHT as i32
        } else {
            0
        };
        let wdx = win.x;
        let wdy = win.y + to;
        // Draw the surface (client area).  The window may be larger
        // than the surface (e.g. after tiling).  Surface pixels are
        // drawn once; any remaining area is filled with the surface's
        // background colour.
        let sw = src.width() as i32;
        let sh = src.height() as i32;
        let bg_fallback = src.get_pixel(0, 0).unwrap_or(0x000000);
        let sxs = 0i32.max(-wdx);
        let sys = 0i32.max(-wdy);
        let sxe = (win.width as i64)
            .min((fbw as i64).saturating_sub(wdx as i64))
            .max(0) as i32;
        let sye = (win.height as i64)
            .min((fbh as i64).saturating_sub(wdy as i64))
            .max(0) as i32;
        if sxs >= sxe || sys >= sye {
            return;
        }
        let cex = (cx + cw) as i32;
        let cey = (cy + ch) as i32;
        let sp = src.pixels();
        for sr in sys..sye {
            let dr = (wdy + sr) as i32;
            if dr < cy as i32 || dr >= cey {
                continue;
            }
            let db = (dr as usize) * (fbw as usize);
            let in_surface_row = sr < sh;
            let sb = if in_surface_row {
                (sr as usize) * (sw as usize)
            } else {
                0
            };
            for sc in sxs..sxe {
                let dc = wdx + sc;
                if dc < cx as i32 || dc >= cex {
                    continue;
                }
                let color = if in_surface_row && sc < sw {
                    sp[sb + sc as usize]
                } else {
                    bg_fallback
                };
                fb[db + dc as usize] = if win.focused { color } else { dim_color(color) };
            }
        }
    }

    // ── Title bar drawing (with padding) ──────────────────

    fn draw_title_bar(
        fb: &mut [u32],
        fbw: u32,
        fbh: u32,
        win: &Window,
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) {
        let title = win.title.as_ref().map(|t| t.as_str()).unwrap_or("");
        let bc = if win.focused {
            COLOR_BORDER_ACTIVE
        } else {
            COLOR_BORDER_INACTIVE
        };
        let tc = if win.focused {
            COLOR_TITLE_ACTIVE
        } else {
            COLOR_TITLE_INACTIVE
        };
        let ww = win.width + WINDOW_BORDER * 2;
        let wh = win.height + TITLE_BAR_HEIGHT + WINDOW_BORDER * 2;
        let cex = (cx + cw) as i32;
        let cey = (cy + ch) as i32;
        let fw = fbw as i32;
        let fh = fbh as i32;

        // Shadow
        if let Some(sh) = &win.shadow_surface {
            let shx = win.x + 2 - WINDOW_BORDER as i32;
            let shy = win.y + 2 - WINDOW_BORDER as i32;
            for sy in 0..sh.height() {
                let da = shy + sy as i32;
                if da < cy as i32 || da >= cey || da >= fh {
                    continue;
                }
                for sx in 0..sh.width() {
                    let dxa = shx + sx as i32;
                    if dxa < cx as i32 || dxa >= cex || dxa >= fw {
                        continue;
                    }
                    let sp = sh.get_pixel(sx, sy).unwrap_or(0);
                    if sp & 0xFF000000 == 0 {
                        continue;
                    }
                    let d = &mut fb[(da as usize) * (fbw as usize) + dxa as usize];
                    let bg = *d;
                    let a = ((sp >> 24) & 0xFF) as u32;
                    let ia = 255 - a;
                    let r = (((sp >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
                    let g = (((sp >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
                    let b = ((sp & 0xFF) * a + (bg & 0xFF) * ia) / 255;
                    *d = (r << 16) | (g << 8) | b;
                }
            }
        }

        // Border
        for row in 0..wh as i32 {
            let da = win.y - WINDOW_BORDER as i32 + row;
            if da < cy as i32 || da >= cey || da >= fh {
                continue;
            }
            for col in 0..ww as i32 {
                let dxa = win.x - WINDOW_BORDER as i32 + col;
                if dxa < cx as i32 || dxa >= cex || dxa >= fw {
                    continue;
                }
                if row < WINDOW_BORDER as i32
                    || row >= wh as i32 - WINDOW_BORDER as i32
                    || col < WINDOW_BORDER as i32
                    || col >= ww as i32 - WINDOW_BORDER as i32
                {
                    fb[(da as usize) * (fbw as usize) + dxa as usize] = bc;
                }
            }
        }

        // Title bar bg
        for row in 0..TITLE_BAR_HEIGHT as i32 {
            let da = win.y + row;
            if da < cy as i32 || da >= cey || da >= fh {
                continue;
            }
            for col in 0..win.width as i32 {
                let dxa = win.x + col;
                if dxa < cx as i32 || dxa >= cex || dxa >= fw {
                    continue;
                }
                fb[(da as usize) * (fbw as usize) + dxa as usize] = tc;
            }
        }

        // ── Title bar buttons (close / maximize / minimize) ──
        // Use pre‑rendered cached textures for fast blitting.

        blit_button(
            fb,
            fbw,
            &CLOSE_BUTTON_CACHE,
            win.x + win.width as i32 - 18,
            win.y + 3,
        );
        blit_button(
            fb,
            fbw,
            &MAXIMIZE_BUTTON_CACHE,
            win.x + win.width as i32 - 38,
            win.y + 3,
        );
        blit_button(
            fb,
            fbw,
            &MINIMIZE_BUTTON_CACHE,
            win.x + win.width as i32 - 58,
            win.y + 3,
        );

        // Title text (with padding from left)
        // Uses glyph_fast to avoid per-pixel Mutex lock on font lookup
        let tx = win.x + WINDOW_PADDING as i32;
        let ty = win.y + WINDOW_PADDING as i32;
        for (i, ch) in title.bytes().enumerate() {
            if ch < 32 || ch > 126 {
                continue;
            }
            let gl = crate::font::glyph_fast(ch);
            let gx = tx + (i as i32) * 8;
            for row in 0..12 {
                let da = ty + row;
                if da < cy as i32 || da >= cey || da >= fh {
                    continue;
                }
                for col in 0..8 {
                    let dxa = gx + col;
                    if dxa < cx as i32 || dxa >= cex || dxa >= fw {
                        continue;
                    }
                    if gl.pixel(row as u32, col as u32) {
                        fb[(da as usize) * (fbw as usize) + dxa as usize] = COLOR_TEXT;
                    }
                }
            }
        }
    }
}
