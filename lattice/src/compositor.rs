use crate::cursor::Cursor;
use crate::scene::Scene;
use crate::window::Window;

pub trait RenderTarget {
    fn buffer(&mut self) -> &mut [u32];
    fn dimensions(&self) -> (u32, u32);
}

pub struct Compositor;

pub const TITLE_BAR_HEIGHT: u32 = 20;
pub const WINDOW_BORDER: u32 = 2;

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

// ── FPS overlay ─────────────────────────────────────────────
use core::sync::atomic::{AtomicU64, Ordering};

static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_FPS_TICK: AtomicU64 = AtomicU64::new(0);
static CURRENT_FPS_X100: AtomicU64 = AtomicU64::new(0);

pub fn notify_frame_presented(now_tick: u64) {
    let fc = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    let last = LAST_FPS_TICK.load(Ordering::Relaxed);
    if now_tick > last && (fc % 30 == 0) {
        let delta = now_tick.saturating_sub(last);
        if delta > 0 {
            let fps = 30u64.saturating_mul(100).saturating_div(delta);
            CURRENT_FPS_X100.store(fps, Ordering::Relaxed);
        }
        LAST_FPS_TICK.store(now_tick, Ordering::Relaxed);
    }
}

pub fn current_fps_x100() -> u64 { CURRENT_FPS_X100.load(Ordering::Relaxed) }

impl Compositor {
    /// Render the scene into the target.
    ///
    /// Returns the bounding box that was actually drawn (clipped dirty rect),
    /// so the caller can perform a partial blit instead of a full framebuffer copy.
    pub fn render(scene: &Scene<'_>, target: &mut dyn RenderTarget) -> (u32, u32, u32, u32) {
        let (fb_width, fb_height) = target.dimensions();
        let framebuffer = target.buffer();

        let (dx, dy, dw, dh) = if scene.dirty_rects.is_empty() {
            (0u32, 0u32, fb_width, fb_height)
        } else {
            let mut merged = scene.dirty_rects[0];
            for r in &scene.dirty_rects[1..] { merged.merge(r); }
            (merged.x, merged.y,
             merged.width.min(fb_width.saturating_sub(merged.x)),
             merged.height.min(fb_height.saturating_sub(merged.y)))
        };
        if dw == 0 || dh == 0 {
            return (0, 0, 0, 0);
        }

        let fb_w = fb_width as usize;
        for row in dy..dy + dh {
            let rs = (row as usize) * fb_w + (dx as usize);
            framebuffer[rs..rs + (dw as usize)].fill(scene.bg_color);
        }

        for window in scene.windows {
            Self::draw_window_clipped(framebuffer, fb_width, fb_height, window, dx, dy, dw, dh);
        }

        if let Some(c) = scene.cursor {
            if c.visible { Self::draw_cursor_clipped(framebuffer, fb_width, fb_height, c, dx, dy, dw, dh); }
        }

        // Draw debug overlay
        Self::draw_debug_overlay(framebuffer, fb_width, fb_height);

        // Draw taskbar (always visible, overlay at bottom)
        if let Some(tb) = scene.taskbar {
            let bar_y = fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT);
            let bar_rect = crate::scene::DirtyRect::new(0, bar_y, fb_width, crate::taskbar::TASKBAR_HEIGHT);
            let clip = crate::scene::DirtyRect::new(dx, dy, dw, dh);
            if bar_rect.intersects(&clip) {
                tb.render(framebuffer, fb_width, fb_height);
            }
        }

        // Return the drawn bounding box for partial blit.
        let max_x = (dx + dw).min(fb_width);
        let max_y = (dy + dh).min(fb_height);
        (dx, dy, max_x - dx, max_y - dy)
    }

    fn draw_debug_overlay(fb: &mut [u32], fbw: u32, _fbh: u32) {
        let fps = current_fps_x100();
        if fps == 0 { return; }
        let text = alloc::format!("FPS: {}.{:02}", fps / 100, fps % 100);
        let x = fbw.saturating_sub(120);
        let y = 4u32;
        for (i, ch) in text.bytes().enumerate() {
            if ch < 32 || ch > 126 { continue; }
            for row in 0..12 {
                for col in 0..8 {
                    let px = x + (i as u32) * 8 + col;
                    let py = y + row;
                    if px < fbw && py < _fbh && crate::font::get_glyph_pixel(ch, row, col) {
                        fb[(py * fbw + px) as usize] = COLOR_ACCENT;
                    }
                }
            }
        }
    }

    fn draw_cursor_clipped(fb: &mut [u32], fbw: u32, fbh: u32, cur: &Cursor,
        cx: u32, cy: u32, cw: u32, ch: u32)
    {
        let pixels = Cursor::shape();
        let sz = Cursor::SIZE as i32;
        let dst_x = cur.x - Cursor::HOTSPOT_X;
        let dst_y = cur.y - Cursor::HOTSPOT_Y;
        let sx_s = 0i32.max(-dst_x);
        let sy_s = 0i32.max(-dst_y);
        let sx_e = sz.min(fbw as i32 - dst_x);
        let sy_e = sz.min(fbh as i32 - dst_y);
        if sx_s >= sx_e || sy_s >= sy_e { return; }
        let cex = (cx + cw) as i32;
        let cey = (cy + ch) as i32;
        for row in sy_s..sy_e {
            let dy = dst_y + row;
            if dy < cy as i32 || dy >= cey { continue; }
            for col in sx_s..sx_e {
                let dx = dst_x + col;
                if dx < cx as i32 || dx >= cex { continue; }
                let s = pixels[(row as usize) * (sz as usize) + col as usize];
                if s == 0 { continue; }
                let idx = (dy as usize) * (fbw as usize) + dx as usize;
                // Semi‑transparent blending
                let bg = fb[idx];
                let sa = ((s >> 24) & 0xFF) as u32;
                if sa == 0 { fb[idx] = s; continue; }
                let ia = 255 - sa;
                let r = (((s>>16)&0xFF)*sa + ((bg>>16)&0xFF)*ia)/255;
                let g = (((s>>8)&0xFF)*sa  + ((bg>>8)&0xFF)*ia)/255;
                let b = ((s&0xFF)*sa         + (bg&0xFF)*ia)/255;
                fb[idx] = (r<<16)|(g<<8)|b;
            }
        }
    }

    fn draw_window_clipped(fb: &mut [u32], fbw: u32, fbh: u32, win: &Window,
        cx: u32, cy: u32, cw: u32, ch: u32)
    {
        if win.title.is_some() { Self::draw_title_bar(fb, fbw, fbh, win, cx, cy, cw, ch); }
        let src = &win.surface;
        let to = if win.title.is_some() { TITLE_BAR_HEIGHT as i32 } else { 0 };
        let wdx = win.x; let wdy = win.y + to;
        let sxs = 0i32.max(-wdx); let sys = 0i32.max(-wdy);
        let sxe = (src.width() as i64).min((fbw as i64).saturating_sub(wdx as i64)).max(0) as i32;
        let sye = (src.height() as i64).min((fbh as i64).saturating_sub(wdy as i64)).max(0) as i32;
        if sxs >= sxe || sys >= sye { return; }
        let cex = (cx + cw) as i32; let cey = (cy + ch) as i32;
        let sp = src.pixels();
        for sr in sys..sye {
            let dr = (wdy + sr) as i32;
            if dr < cy as i32 || dr >= cey { continue; }
            let sb = (sr as usize)*(src.width() as usize);
            let db = (dr as usize)*(fbw as usize);
            for sc in sxs..sxe {
                let dc = wdx + sc;
                if dc < cx as i32 || dc >= cex { continue; }
                fb[db + dc as usize] = sp[sb + sc as usize];
            }
        }
    }

    fn draw_title_bar(fb: &mut [u32], fbw: u32, fbh: u32, win: &Window,
        cx: u32, cy: u32, cw: u32, ch: u32)
    {
        let title = win.title.as_ref().map(|t| t.as_str()).unwrap_or("");
        let bc = if win.focused { COLOR_BORDER_ACTIVE } else { COLOR_BORDER_INACTIVE };
        let tc = if win.focused { COLOR_TITLE_ACTIVE } else { COLOR_TITLE_INACTIVE };
        let ww = win.width + WINDOW_BORDER*2;
        let wh = win.height + TITLE_BAR_HEIGHT + WINDOW_BORDER*2;
        let cex = (cx+cw) as i32; let cey = (cy+ch) as i32;
        let fw = fbw as i32; let fh = fbh as i32;

        // Shadow
        if let Some(sh) = &win.shadow_surface {
            let shx = win.x + 2 - WINDOW_BORDER as i32;
            let shy = win.y + 2 - WINDOW_BORDER as i32;
            for sy in 0..sh.height() {
                let da = shy + sy as i32;
                if da<cy as i32||da>=cey||da>=fh { continue; }
                for sx in 0..sh.width() {
                    let dxa = shx + sx as i32;
                    if dxa<cx as i32||dxa>=cex||dxa>=fw { continue; }
                    let sp = sh.get_pixel(sx,sy).unwrap_or(0);
                    if sp&0xFF000000==0 { continue; }
                    let d = &mut fb[(da as usize)*(fbw as usize)+dxa as usize];
                    let bg = *d;
                    let a = ((sp>>24)&0xFF) as u32; let ia=255-a;
                    let r=(((sp>>16)&0xFF)*a+((bg>>16)&0xFF)*ia)/255;
                    let g=(((sp>>8)&0xFF)*a +((bg>>8)&0xFF)*ia)/255;
                    let b=((sp&0xFF)*a        +(bg&0xFF)*ia)/255;
                    *d = (r<<16)|(g<<8)|b;
                }
            }
        }

        // Border
        for row in 0..wh as i32 {
            let da = win.y - WINDOW_BORDER as i32 + row;
            if da<cy as i32||da>=cey||da>=fh { continue; }
            for col in 0..ww as i32 {
                let dxa = win.x - WINDOW_BORDER as i32 + col;
                if dxa<cx as i32||dxa>=cex||dxa>=fw { continue; }
                if row<WINDOW_BORDER as i32||row>=wh as i32 - WINDOW_BORDER as i32
                    ||col<WINDOW_BORDER as i32||col>=ww as i32 - WINDOW_BORDER as i32 {
                    fb[(da as usize)*(fbw as usize)+dxa as usize]=bc;
                }
            }
        }

        // Title bar bg
        for row in 0..TITLE_BAR_HEIGHT as i32 {
            let da = win.y + row;
            if da<cy as i32||da>=cey||da>=fh { continue; }
            for col in 0..win.width as i32 {
                let dxa = win.x + col;
                if dxa<cx as i32||dxa>=cex||dxa>=fw { continue; }
                fb[(da as usize)*(fbw as usize)+dxa as usize]=tc;
            }
        }

        // Close button (small red square)
        let close_x = win.x + win.width as i32 - 18;
        let close_y = win.y + 3;
        for r in 0..14i32 {
            for c in 0..14i32 {
                let da = close_y + r;
                let dxa = close_x + c;
                if da>=cey||dxa>=cex||da>=fh||dxa>=fw { continue; }
                fb[(da as usize)*(fbw as usize)+dxa as usize] = COLOR_DANGER;
            }
        }
        // White X on close button (smaller cross)
        for o in 0..8 {
            let dxa = close_x + 3 + o;
            let da1 = close_y + 3 + o;
            let da2 = close_y + 10 - o;
            if dxa<cex&&dxa<fw {
                if da1<cey&&da1<fh { fb[(da1 as usize)*(fbw as usize)+dxa as usize]=0xFFFFFF; }
                if da2<cey&&da2<fh { fb[(da2 as usize)*(fbw as usize)+dxa as usize]=0xFFFFFF; }
            }
        }

        // Title text
        let tx = win.x+4; let ty = win.y+4;
        for (i,ch) in title.bytes().enumerate() {
            if ch<32||ch>126 { continue; }
            let gx=tx+(i as i32)*8;
            for row in 0..12 {
                let da=ty+row;
                if da<cy as i32||da>=cey||da>=fh { continue; }
                for col in 0..8 {
                    let dxa=gx+col;
                    if dxa<cx as i32||dxa>=cex||dxa>=fw { continue; }
                    if crate::font::get_glyph_pixel(ch,row as u32,col as u32) {
                        fb[(da as usize)*(fbw as usize)+dxa as usize]=COLOR_TEXT;
                    }
                }
            }
        }
    }
}