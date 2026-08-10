use crate::cursor::Cursor;
use crate::scene::{DirtyRect, OverlayRect, Scene};
use crate::window::Window;

/// Global window corner radius (0 = square, 8 = rounded).
/// Set by the settings UI; read by the compositor.
pub static WINDOW_CORNER_RADIUS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(8);

pub trait RenderTarget {
    fn buffer(&mut self) -> &mut [u32];
    fn dimensions(&self) -> (u32, u32);
}

pub struct Compositor;

pub const TITLE_BAR_HEIGHT: u32 = 28;
pub const WINDOW_BORDER: u32 = 1;

// UI padding constants
pub const WINDOW_PADDING: u32 = 8;
pub const TASKBAR_PADDING: u32 = 6;
pub const BUTTON_PADDING: u32 = 4;

// ── Fullerene Color Palette ──────────────────────────────────
pub const COLOR_BG: u32 = 0x1B1B1D;
pub const COLOR_SURFACE: u32 = 0x242426;
pub const COLOR_PRIMARY: u32 = 0x3584E4;
pub const COLOR_ACTIVE: u32 = 0x2A7DE0;
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

/// Apply software brightness to a colour.
///
/// `brightness_x100` is the brightness value × 100 (range 10..100, default 100).
/// Each channel is multiplied by `brightness_x100 / 100`.
#[inline]
pub fn apply_brightness(color: u32, brightness_x100: u32) -> u32 {
    if brightness_x100 >= 100 {
        return color; // no change at 100% or above
    }
    let r = ((color >> 16) & 0xFF) * brightness_x100 / 100;
    let g = ((color >> 8) & 0xFF) * brightness_x100 / 100;
    let b = (color & 0xFF) * brightness_x100 / 100;
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

/// Blend the cursor into a RAM-backed render target within `clip`.
pub fn render_cursor(fb: &mut [u32], fbw: u32, fbh: u32, cur: &Cursor, clip: DirtyRect) {
    let DirtyRect {
        x: cx,
        y: cy,
        width: cw,
        height: ch,
    } = clip;
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
    // `fetch_add` returns the pre-increment count, so use `fc + 1` so the
    // update lands on frames 30, 60, 90, … rather than 1, 31, 61, …
    const FRAMES_PER_UPDATE: u64 = 30;
    if now_tick > last && (fc + 1) % FRAMES_PER_UPDATE == 0 {
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
        Self::render_internal(scene, target, false)
    }

    /// Render an incremental video update over an already-composited target.
    /// The background and desktop icon layers are retained in the RAM-backed
    /// target, avoiding an expensive wallpaper repaint for every video frame.
    pub fn render_preserving_background(
        scene: &Scene<'_>,
        target: &mut dyn RenderTarget,
    ) -> (u32, u32, u32, u32) {
        Self::render_internal(scene, target, true)
    }

    fn render_internal(
        scene: &Scene<'_>,
        target: &mut dyn RenderTarget,
        preserve_background: bool,
    ) -> (u32, u32, u32, u32) {
        // Reset draw-call counter
        DRAW_CALLS.store(0, Ordering::Relaxed);

        let (fb_width, fb_height) = target.dimensions();
        let framebuffer = target.buffer();

        // Render each dirty region independently. Merging all dirty regions
        // into one bounding box makes a pair of small updates (for example,
        // the old and new cursor positions) repaint every pixel between them.
        // The framebuffer is persistent, so each region can be composited
        // independently without changing the resulting image.
        let mut drawn: Option<DirtyRect> = None;
        if scene.dirty_rects.is_empty() {
            let region = DirtyRect::full(fb_width, fb_height);
            Self::render_region(
                scene,
                framebuffer,
                fb_width,
                fb_height,
                region,
                preserve_background,
            );
            drawn = Some(region);
        } else {
            for &dirty in scene.dirty_rects {
                let Some(region) = Self::clip_region(dirty, fb_width, fb_height) else {
                    continue;
                };
                Self::render_region(
                    scene,
                    framebuffer,
                    fb_width,
                    fb_height,
                    region,
                    preserve_background,
                );
                if let Some(bounds) = drawn.as_mut() {
                    bounds.merge(&region);
                } else {
                    drawn = Some(region);
                }
            }
        }

        drawn.map_or((0, 0, 0, 0), |region| {
            (region.x, region.y, region.width, region.height)
        })
    }

    #[inline]
    fn clip_region(region: DirtyRect, fb_width: u32, fb_height: u32) -> Option<DirtyRect> {
        if region.x >= fb_width || region.y >= fb_height {
            return None;
        }
        let width = region.width.min(fb_width - region.x);
        let height = region.height.min(fb_height - region.y);
        (width > 0 && height > 0).then_some(DirtyRect::new(region.x, region.y, width, height))
    }

    fn render_region(
        scene: &Scene<'_>,
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        region: DirtyRect,
        preserve_background: bool,
    ) {
        let dx = region.x;
        let dy = region.y;
        let dw = region.width;
        let dh = region.height;

        // ── Layer 0: Desktop background (wallpaper) + icons ───
        if !preserve_background {
            if scene.layered {
                crate::wallpaper::render_wallpaper(
                    framebuffer,
                    fb_width,
                    fb_height,
                    dx,
                    dy,
                    dw,
                    dh,
                );
            } else {
                for row in dy..dy + dh {
                    let start = (row * fb_width + dx) as usize;
                    framebuffer[start..start + dw as usize].fill(scene.bg_color);
                }
            }

            // Draw desktop icons on the background, behind windows
            if let Some(icons) = scene.desktop_icons {
                icons.render(framebuffer, fb_width, fb_height, dx, dy, dw, dh);
            }
        }

        // ── Layer 1: Windows ─────────────────────────────
        for window in scene.windows {
            if !window.minimized {
                Self::draw_window_clipped(
                    framebuffer,
                    fb_width,
                    fb_height,
                    window,
                    dx,
                    dy,
                    dw,
                    dh,
                    preserve_background,
                );
            }
        }
        inc_draw_calls();

        // ── Layer 2: Overlays ────────────────────────────
        if !scene.overlays.is_empty() {
            for ov in scene.overlays {
                Self::draw_overlay_clipped(framebuffer, fb_width, fb_height, ov, dx, dy, dw, dh);
            }
            inc_draw_calls();
        }

        if let Some(menu) = scene.active_menu {
            let menu_rect = DirtyRect::new(menu.x, menu.y, menu.width, menu.height);
            if menu_rect.intersects(&region) {
                let mut painter = crate::painter::Painter::new(framebuffer, fb_width, fb_height);
                painter.clip_rect(
                    region.x as i32,
                    region.y as i32,
                    region.width,
                    region.height,
                );
                crate::style::style_for(crate::style::variant()).draw_menu(&mut painter, menu);
                inc_draw_calls();
            }
        }

        if scene.network_menu_open {
            let menu_height =
                4 + (scene.net_aps.len() as u32 + 1) * crate::network_menu::NET_MENU_ITEM_HEIGHT;
            let menu_rect = DirtyRect::new(
                scene.net_menu_x,
                scene.net_menu_y,
                crate::network_menu::NET_MENU_WIDTH,
                menu_height,
            );
            if menu_rect.intersects(&region) {
                crate::network_menu::render_network_menu(
                    framebuffer,
                    fb_width,
                    fb_height,
                    scene.net_menu_x,
                    scene.net_menu_y,
                    scene.net_aps,
                    scene.net_status,
                    scene.net_selected_idx,
                );
                inc_draw_calls();
            }
        }

        if scene.pwd_dialog_open {
            let dialog_rect = DirtyRect::new(
                scene.pwd_dialog_x,
                scene.pwd_dialog_y,
                crate::network_menu::PWD_DIALOG_W,
                crate::network_menu::PWD_DIALOG_H,
            );
            if dialog_rect.intersects(&region) {
                crate::network_menu::render_password_dialog(
                    framebuffer,
                    fb_width,
                    fb_height,
                    scene.pwd_dialog_x,
                    scene.pwd_dialog_y,
                    scene.pwd_ssid,
                    scene.pwd_password,
                    scene.pwd_cursor,
                );
                inc_draw_calls();
            }
        }

        // ── Layer 3: System UI ───────────────────────────
        if let Some(tb) = scene.taskbar {
            let bar_height = crate::style::taskbar_height();
            let bar_y = fb_height.saturating_sub(bar_height);
            let bar_rect = DirtyRect::new(0, bar_y, fb_width, bar_height);
            if bar_rect.intersects(&region) {
                tb.render(framebuffer, fb_width, fb_height);
            }
            inc_draw_calls();
        }

        if let Some(c) = scene.cursor
            && c.visible
        {
            render_cursor(framebuffer, fb_width, fb_height, c, region);
            inc_draw_calls();
        }

        // Clip the debug text too; otherwise every small region writes into
        // the top-left area of the persistent back buffer unnecessarily.
        Self::draw_debug_overlay(framebuffer, fb_width, fb_height, region);
        inc_draw_calls();
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
        let border_active = crate::theme::current_colors().border_active;
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
                let color = if is_border { border_active } else { ov.color };
                let idx = (da as usize) * (fbw as usize) + dxa as usize;
                fb[idx] = color;
            }
        }
    }

    fn draw_debug_overlay(fb: &mut [u32], fbw: u32, fbh: u32, clip: DirtyRect) {
        let fps = current_fps_x100();
        if fps == 0 {
            return;
        }
        let dc = draw_calls_last_frame();
        let accent = crate::theme::current_colors().accent;
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
        let text_str = core::str::from_utf8(text).unwrap_or("FPS:?");

        let x = (fbw.saturating_sub(150)) as i32;
        // Painter text rendering currently clips to the framebuffer but not
        // to its painter clip rectangle. Avoid invoking the relatively
        // expensive font renderer unless this region can touch the overlay.
        if clip.x as i32 >= fbw as i32
            || clip.y >= 20
            || (clip.x + clip.width) as i32 <= x
            || clip.y + clip.height <= 4
        {
            return;
        }
        let mut p = crate::painter::Painter::new(fb, fbw, fbh);
        p.draw_text(x, 4, text_str, accent, 13.0);
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
        preserve_background: bool,
    ) {
        let src = &win.surface;
        let title_h = crate::style::title_bar_height() as i32;
        let radius = crate::style::window_radius().saturating_sub(1) as i32;
        let to = if win.title.is_some() { title_h } else { 0 };
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
            Self::draw_window_frame_clipped(fb, fbw, fbh, win, cx, cy, cw, ch);
            return;
        }
        let cex = (cx + cw) as i32;
        let cey = (cy + ch) as i32;
        let sp = src.pixels();

        // Video frames normally update a focused window without changing its
        // geometry. Copy the bulk of the client surface row-wise and leave
        // only the small rounded-corner strip to the general clipped path.
        // This avoids per-pixel bounds and corner checks for ~99% of a video
        // frame while preserving the normal path for all desktop redraws.
        let fast_client_rows = if preserve_background
            && win.focused
            && wdx >= 0
            && wdy >= 0
            && sw >= win.width as i32
            && sh >= win.height as i32
            && win.title.is_some()
            && radius > 0
        {
            let x0 = (cx as i32).max(wdx) as u32;
            let x1 = cex.min(wdx + win.width as i32).min(fbw as i32) as u32;
            let y0 = (cy as i32).max(wdy) as u32;
            let client_bottom = wdy + win.height as i32;
            let fast_bottom = client_bottom.saturating_sub(radius);
            let y1 = cey.min(fast_bottom).min(fbh as i32) as u32;
            if x0 < x1 && y0 < y1 {
                let copy_width = (x1 - x0) as usize;
                let source_x = (x0 as i32 - wdx) as usize;
                for y in y0..y1 {
                    let source_start = (y as i32 - wdy) as usize * sw as usize + source_x;
                    let dest_start = y as usize * fbw as usize + x0 as usize;
                    fb[dest_start..dest_start + copy_width]
                        .copy_from_slice(&sp[source_start..source_start + copy_width]);
                }
                Some((y0 as i32, y1 as i32))
            } else {
                None
            }
        } else {
            None
        };

        // A maximized shell is an opaque, focused surface that usually
        // covers almost the entire work area. The general path performs
        // several bounds checks and a focus colour branch for every pixel;
        // copy complete rows directly in this common case.
        if win.focused
            && wdx >= 0
            && wdy >= 0
            && sw >= win.width as i32
            && sh >= win.height as i32
            && (win.title.is_none() || radius == 0)
        {
            let x0 = (cx as i32).max(wdx) as u32;
            let x1 = cex.min(wdx + win.width as i32).min(fbw as i32) as u32;
            let y0 = (cy as i32).max(wdy) as u32;
            let y1 = cey.min(wdy + win.height as i32).min(fbh as i32) as u32;
            if x0 < x1 && y0 < y1 {
                let copy_width = (x1 - x0) as usize;
                let source_x = (x0 as i32 - wdx) as usize;
                for y in y0..y1 {
                    let source_start = (y as i32 - wdy) as usize * sw as usize + source_x;
                    let dest_start = y as usize * fbw as usize + x0 as usize;
                    fb[dest_start..dest_start + copy_width]
                        .copy_from_slice(&sp[source_start..source_start + copy_width]);
                }
                Self::draw_window_frame_clipped(fb, fbw, fbh, win, cx, cy, cw, ch);
                return;
            }
        }

        for sr in sys..sye {
            let dr = (wdy + sr) as i32;
            if dr < cy as i32 || dr >= cey {
                continue;
            }
            if fast_client_rows.is_some_and(|(y0, y1)| dr >= y0 && dr < y1) {
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
                if !Self::client_pixel_inside_frame(win, dc, dr, title_h, radius) {
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
        Self::draw_window_frame_clipped(fb, fbw, fbh, win, cx, cy, cw, ch);
    }

    fn client_pixel_inside_frame(win: &Window, x: i32, y: i32, title_h: i32, radius: i32) -> bool {
        if win.title.is_none() {
            return true;
        }
        if radius == 0 {
            return true;
        }
        let right = win.x + win.width as i32;
        let bottom = win.y + title_h + win.height as i32;
        if y < bottom - radius || (x >= win.x + radius && x < right - radius) {
            return true;
        }
        let (center_x, center_y) = if x < win.x + radius {
            (win.x + radius, bottom - radius)
        } else {
            (right - radius, bottom - radius)
        };
        let dx = x - center_x;
        let dy = y - center_y;
        dx * dx + dy * dy <= radius * radius
    }

    fn draw_window_frame_clipped(
        fb: &mut [u32],
        fbw: u32,
        fbh: u32,
        win: &Window,
        cx: u32,
        cy: u32,
        cw: u32,
        ch: u32,
    ) {
        if win.title.is_none() {
            return;
        }
        let mut painter = crate::painter::Painter::new(fb, fbw, fbh);
        painter.clip_rect(cx as i32, cy as i32, cw, ch);
        crate::style::style_for(crate::style::variant()).draw_window_frame(
            &mut painter,
            win,
            crate::common::WindowVisualState {
                focused: win.focused,
                maximized: win.maximized,
            },
        );
    }
}
