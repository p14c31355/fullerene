//! Painter abstraction — the sole way to draw into a framebuffer.
//!
//! Provides clipped drawing primitives (rect, rounded rect, text, shadow,
//! surface blit) so upper layers never touch raw pixels directly.

use crate::font::{self, render_text_bitmap, render_text_ttf};
use crate::surface::Surface;
use crate::theme::ThemeColors;

/// A software painter operating on a `u32` RGBA framebuffer.
pub struct Painter<'a> {
    pub fb: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    clip_x: u32,
    clip_y: u32,
    clip_w: u32,
    clip_h: u32,
}

impl<'a> Painter<'a> {
    pub fn new(fb: &'a mut [u32], width: u32, height: u32) -> Self {
        Self {
            fb,
            width,
            height,
            stride: width,
            clip_x: 0,
            clip_y: 0,
            clip_w: width,
            clip_h: height,
        }
    }

    pub fn new_with_stride(fb: &'a mut [u32], width: u32, height: u32, stride: u32) -> Self {
        Self {
            fb,
            width,
            height,
            stride,
            clip_x: 0,
            clip_y: 0,
            clip_w: width,
            clip_h: height,
        }
    }

    /// Set an additional clipping rectangle (in addition to framebuffer bounds).
    pub fn clip_rect(&mut self, x: i32, y: i32, w: u32, h: u32) {
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        let xe = x.saturating_add(w).min(self.width);
        let ye = y.saturating_add(h).min(self.height);
        let cx = self.clip_x.max(x);
        let cy = self.clip_y.max(y);
        let cw = self
            .clip_x
            .saturating_add(self.clip_w)
            .min(xe)
            .saturating_sub(cx);
        let ch = self
            .clip_y
            .saturating_add(self.clip_h)
            .min(ye)
            .saturating_sub(cy);
        self.clip_x = cx;
        self.clip_y = cy;
        self.clip_w = cw;
        self.clip_h = ch;
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.stride as usize) + (x as usize)
    }

    #[inline]
    fn contains(&self, x: u32, y: u32) -> bool {
        x < self.width
            && y < self.height
            && x >= self.clip_x
            && x - self.clip_x < self.clip_w
            && y >= self.clip_y
            && y - self.clip_y < self.clip_h
    }

    /// Clip a rectangle to framebuffer bounds and the painter clip rect, returning `(x, y, w, h)` or `None`.
    fn clip(&self, x: i32, y: i32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
        let w = if x < 0 {
            w.saturating_sub((-x) as u32)
        } else {
            w
        };
        let h = if y < 0 {
            h.saturating_sub((-y) as u32)
        } else {
            h
        };
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        let mut w = (w as u64).min((self.width as u64).saturating_sub(x as u64)) as u32;
        let mut h = (h as u64).min((self.height as u64).saturating_sub(y as u64)) as u32;
        if w == 0 || h == 0 {
            return None;
        }
        // Intersect with painter clip rect
        let cxe = self.clip_x.saturating_add(self.clip_w);
        let cye = self.clip_y.saturating_add(self.clip_h);
        let xe = x.saturating_add(w).min(cxe);
        let ye = y.saturating_add(h).min(cye);
        let x = x.max(self.clip_x);
        let y = y.max(self.clip_y);
        w = xe.saturating_sub(x);
        h = ye.saturating_sub(y);
        if w == 0 || h == 0 {
            None
        } else {
            Some((x, y, w, h))
        }
    }

    // ── Fill ─────────────────────────────────────────────────

    /// Fill a rectangle with a solid colour.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        let (cx, cy, cw, ch) = match self.clip(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        let stride = self.stride as usize;
        for row in cy..cy + ch {
            let start = (row as usize) * stride + cx as usize;
            let end = start + cw as usize;
            self.fb[start..end].fill(color);
        }
    }

    /// Fill a rounded rectangle.
    pub fn rounded_rect(&mut self, x: i32, y: i32, w: u32, h: u32, r: u32, color: u32) {
        if r == 0 {
            return self.fill_rect(x, y, w, h, color);
        }
        let r = r.min(w / 2).min(h / 2) as i32;
        // Fill center
        self.fill_rect(x, y + r, w, h.saturating_sub(r as u32 * 2), color);
        // Fill top/middle/bottom bars between corners
        self.fill_rect(x + r, y, w.saturating_sub(r as u32 * 2), h, color);
        // Four 4x4-supersampled corners. The old integer circle test left
        // stair-stepped one-pixel wedges on every window and button.
        self.fill_rounded_corner(x, y, x + r, y + r, r, color);
        self.fill_rounded_corner(x + w as i32 - r, y, x + w as i32 - r, y + r, r, color);
        self.fill_rounded_corner(x, y + h as i32 - r, x + r, y + h as i32 - r, r, color);
        self.fill_rounded_corner(
            x + w as i32 - r,
            y + h as i32 - r,
            x + w as i32 - r,
            y + h as i32 - r,
            r,
            color,
        );
    }

    /// Draw only the antialiased outline of a rounded rectangle. This keeps
    /// the client surface intact while making the outer frame continuous.
    pub fn rounded_rect_outline(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        r: u32,
        thickness: u32,
        color: u32,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2) as i32;
        let t = thickness.max(1).min(r.max(1) as u32) as i32;
        if r == 0 {
            self.fill_rect(x, y, w, t as u32, color);
            self.fill_rect(x, y + h as i32 - t, w, t as u32, color);
            self.fill_rect(x, y, t as u32, h, color);
            self.fill_rect(x + w as i32 - t, y, t as u32, h, color);
            return;
        }

        self.fill_rect(x + r, y, w.saturating_sub(r as u32 * 2), t as u32, color);
        self.fill_rect(
            x + r,
            y + h as i32 - t,
            w.saturating_sub(r as u32 * 2),
            t as u32,
            color,
        );
        self.fill_rect(x, y + r, t as u32, h.saturating_sub(r as u32 * 2), color);
        self.fill_rect(
            x + w as i32 - t,
            y + r,
            t as u32,
            h.saturating_sub(r as u32 * 2),
            color,
        );

        self.outline_rounded_corner(x, y, x + r, y + r, r, t, color);
        self.outline_rounded_corner(x + w as i32 - r, y, x + w as i32 - r, y + r, r, t, color);
        self.outline_rounded_corner(x, y + h as i32 - r, x + r, y + h as i32 - r, r, t, color);
        self.outline_rounded_corner(
            x + w as i32 - r,
            y + h as i32 - r,
            x + w as i32 - r,
            y + h as i32 - r,
            r,
            t,
            color,
        );
    }

    fn fill_rounded_corner(
        &mut self,
        x: i32,
        y: i32,
        center_x: i32,
        center_y: i32,
        radius: i32,
        color: u32,
    ) {
        for py in y..y + radius {
            for px in x..x + radius {
                if px < 0 || py < 0 {
                    continue;
                }
                let coverage = self.circle_coverage(px, py, center_x, center_y, radius);
                if coverage == 16 {
                    self.set_pixel(px as u32, py as u32, color);
                } else if coverage > 0 {
                    self.blend_pixel(
                        px as u32,
                        py as u32,
                        (color & 0x00FF_FFFF) | ((coverage * 255 / 16) << 24),
                    );
                }
            }
        }
    }

    fn outline_rounded_corner(
        &mut self,
        x: i32,
        y: i32,
        center_x: i32,
        center_y: i32,
        radius: i32,
        thickness: i32,
        color: u32,
    ) {
        for py in y..y + radius {
            for px in x..x + radius {
                if px < 0 || py < 0 {
                    continue;
                }
                let coverage = self.circle_coverage(px, py, center_x, center_y, radius);
                let inner_radius = (radius - thickness).max(0);
                let inner = self.circle_coverage(px, py, center_x, center_y, inner_radius);
                let coverage = coverage.saturating_sub(inner);
                if coverage == 16 {
                    self.set_pixel(px as u32, py as u32, color);
                } else if coverage > 0 {
                    self.blend_pixel(
                        px as u32,
                        py as u32,
                        (color & 0x00FF_FFFF) | ((coverage * 255 / 16) << 24),
                    );
                }
            }
        }
    }

    fn circle_coverage(&self, px: i32, py: i32, center_x: i32, center_y: i32, radius: i32) -> u32 {
        if radius <= 0 {
            return 0;
        }
        let mut inside = 0u32;
        let radius = radius * 8;
        let center_x = center_x * 8;
        let center_y = center_y * 8;
        for sample_y in [1i32, 3, 5, 7] {
            for sample_x in [1i32, 3, 5, 7] {
                let dx = px * 8 + sample_x - center_x;
                let dy = py * 8 + sample_y - center_y;
                if dx * dx + dy * dy <= radius * radius {
                    inside += 1;
                }
            }
        }
        inside
    }

    // ── Pixel ────────────────────────────────────────────────

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        if self.contains(x, y) {
            self.fb[self.idx(x, y)] = color;
        }
    }

    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x < self.width && y < self.height {
            Some(self.fb[self.idx(x, y)])
        } else {
            None
        }
    }

    // ── Alpha-blend ──────────────────────────────────────────

    /// Alpha-blend a source RGBA pixel over the destination.
    pub fn blend_pixel(&mut self, x: u32, y: u32, src: u32) {
        if !self.contains(x, y) {
            return;
        }
        let idx = self.idx(x, y);
        let a = (src >> 24) & 0xFF;
        if a == 255 {
            self.fb[idx] = src;
            return;
        }
        if a == 0 {
            return;
        }
        let bg = self.fb[idx];
        let ia = 255 - a;
        let r = (((src >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
        let g = (((src >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
        let b = ((src & 0xFF) * a + (bg & 0xFF) * ia) / 255;
        self.fb[idx] = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
    }

    // ── Blit Surface ─────────────────────────────────────────

    /// Copy a Surface onto the framebuffer at (dx, dy) with optional alpha blend.
    pub fn blit_surface(&mut self, src: &Surface, dx: i32, dy: i32) {
        let sw = src.width() as i32;
        let sh = src.height() as i32;
        let sx_s = 0i32.max(-dx);
        let sy_s = 0i32.max(-dy);
        let sx_e = sw
            .min(self.width as i32 - dx)
            .min(self.clip_x.saturating_add(self.clip_w).min(self.width) as i32 - dx);
        let sy_e = sh
            .min(self.height as i32 - dy)
            .min(self.clip_y.saturating_add(self.clip_h).min(self.height) as i32 - dy);
        let sx_s = sx_s.max(self.clip_x as i32 - dx);
        let sy_s = sy_s.max(self.clip_y as i32 - dy);
        if sx_s >= sx_e || sy_s >= sy_e {
            return;
        }
        let ddx = (dx + sx_s) as u32;
        let ddy = (dy + sy_s) as u32;
        for row in sy_s..sy_e {
            let src_row = &src.pixels()[(row as usize) * (sw as usize)..];
            let dst_start = self.idx(ddx, ddy + (row - sy_s) as u32);
            let dst_slice = &mut self.fb[dst_start..dst_start + (sx_e - sx_s) as usize];
            for (i, &p) in src_row[sx_s as usize..sx_e as usize].iter().enumerate() {
                let a = (p >> 24) & 0xFF;
                if a == 0 {
                    // Fully transparent: preserve destination
                    continue;
                } else if a == 255 {
                    // Fully opaque: direct replacement
                    dst_slice[i] = p;
                } else {
                    // Partially transparent: blend with background
                    let bg = dst_slice[i];
                    let ia = 255 - a;
                    let r = (((p >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
                    let g = (((p >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
                    let b = ((p & 0xFF) * a + (bg & 0xFF) * ia) / 255;
                    dst_slice[i] = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
                }
            }
        }
    }

    // ── Shadow ───────────────────────────────────────────────

    /// Draw a drop shadow with smooth quadratic falloff (no FP).
    pub fn draw_shadow(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        _radius: u32,
        offset: i32,
        blur: u32,
        color: u32,
    ) {
        let sx = x + offset;
        let sy = y + offset;
        let b = blur as i32;
        let sw = w as i32 + b * 2;
        let sh = h as i32 + b * 2;
        if (sw as u64) * (sh as u64) > self.width as u64 * self.height as u64 * 2 {
            return;
        }
        let b_sq = (blur * blur) as u64;
        let max_alpha = 70u32;
        for dy in 0..sh {
            let ay = sy - b + dy;
            if ay < 0 || ay >= self.height as i32 {
                continue;
            }
            let yd = if dy < b {
                (b - dy) as u64
            } else if dy >= sh - b {
                (dy - (sh - b) + 1) as u64
            } else {
                0
            };
            for dx in 0..sw {
                let ax = sx - b + dx;
                if ax < 0 || ax >= self.width as i32 {
                    continue;
                }
                let xd = if dx < b {
                    (b - dx) as u64
                } else if dx >= sw - b {
                    (dx - (sw - b) + 1) as u64
                } else {
                    0
                };
                // The shadow is an exterior effect. The previous formula
                // also blended the shadow across the entire window interior,
                // which became visible once frames were drawn after client
                // surfaces to keep rounded corners connected.
                if xd == 0 && yd == 0 {
                    continue;
                }
                let dist_sq = xd * xd + yd * yd;
                if dist_sq >= b_sq {
                    continue;
                }
                // Quadratic falloff: max alpha at center, 0 at edge
                let alpha = if b_sq == 0 {
                    max_alpha as u64
                } else {
                    (max_alpha as u64 * (b_sq - dist_sq)) / b_sq
                };
                if alpha > 0 {
                    let src_c = (color & 0x00FF_FFFF) | ((alpha as u32) << 24);
                    self.blend_pixel(ax as u32, ay as u32, src_c);
                }
            }
        }
    }

    // ── Text ─────────────────────────────────────────────────

    /// Draw text using TTF-based antialiased rendering (falls back to bitmap).
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, color: u32, size: f32) {
        let ttf = font::get_ttf_font();
        if let Some(font) = ttf {
            let _ = render_text_ttf(
                self.fb,
                self.width,
                self.height,
                self.stride,
                x,
                y,
                text,
                color,
                size,
                font,
            );
        } else {
            render_text_bitmap(
                self.fb,
                self.width,
                self.height,
                self.stride,
                x,
                y,
                text,
                color,
            );
        }
    }

    /// Draw text using the legacy bitmap font (always works, no TTF needed).
    pub fn draw_text_bitmap(&mut self, x: i32, y: i32, text: &str, color: u32) {
        render_text_bitmap(
            self.fb,
            self.width,
            self.height,
            self.stride,
            x,
            y,
            text,
            color,
        );
    }

    // ── Window border helpers ────────────────────────────────

    /// Draw a window border with rounded corners.
    pub fn draw_window_border(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        radius: u32,
        title_h: u32,
        active: bool,
        theme: &ThemeColors,
    ) {
        let border_color = if active {
            theme.border_active
        } else {
            theme.border_inactive
        };
        let title_color = if active {
            theme.title_active
        } else {
            theme.title_inactive
        };
        self.rounded_rect_outline(x, y, w, h, radius, 1, border_color);
        let inner_radius = radius.saturating_sub(1);
        self.fill_rect(
            x + inner_radius as i32,
            y,
            w.saturating_sub(inner_radius * 2),
            title_h + 2,
            title_color,
        );
        self.fill_rect(
            x + 1,
            y + inner_radius as i32,
            w.saturating_sub(2),
            (title_h + 2).saturating_sub(inner_radius),
            title_color,
        );
        self.fill_rect(x, y + title_h as i32 + 2, w, 1, border_color);
    }
}
