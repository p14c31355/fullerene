use crate::cursor::Cursor;
use crate::window::Window;

/// Compositor state.
///
/// Currently stateless — just the pipeline.
/// Later we will add invalidation regions, damage tracking, etc.
pub struct Compositor;

impl Compositor {
    /// Composite all windows onto the framebuffer.
    ///
    /// This is the **only** place where pixel data enters the framebuffer.
    /// The compositor does NOT touch input logic, window management, or
    /// any other state.
    ///
    /// Rendering order (bottom → top):
    /// 1. Background fill (`bg_color`)
    /// 2. Each window in z‑order (back to front)
    /// 3. Software cursor
    pub fn composite(
        framebuffer: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        windows: &[Window],
        bg_color: u32,
        cursor: Option<&Cursor>,
    ) {
        // 1. Clear to background colour
        for pixel in framebuffer.iter_mut() {
            *pixel = bg_color;
        }

        // 2. Draw windows back to front
        for window in windows {
            Self::draw_window(framebuffer, fb_width, fb_height, window);
        }

        // 3. Draw software cursor
        if let Some(cursor) = cursor {
            if cursor.visible {
                Self::draw_cursor(framebuffer, fb_width, fb_height, cursor);
            }
        }
    }

    /// Draw the software cursor sprite.
    fn draw_cursor(framebuffer: &mut [u32], fb_width: u32, fb_height: u32, cursor: &Cursor) {
        let (pixels, cw, ch) = Cursor::shape();
        let cw = cw as i32;
        let ch = ch as i32;

        let dst_x = cursor.x - Cursor::HOTSPOT_X;
        let dst_y = cursor.y - Cursor::HOTSPOT_Y;

        // Clamp
        let src_x_start = 0i32.max(-dst_x);
        let src_y_start = 0i32.max(-dst_y);
        let src_x_end = cw.min(fb_width as i32 - dst_x);
        let src_y_end = ch.min(fb_height as i32 - dst_y);

        if src_x_start >= src_x_end || src_y_start >= src_y_end {
            return;
        }

        let cw_usize = cw as usize;

        for row in src_y_start..src_y_end {
            let sy = row as usize;
            let dy = (dst_y + row) as usize;

            let src_offset = sy * cw_usize + src_x_start as usize;
            let dst_offset = dy * (fb_width as usize) + (dst_x + src_x_start) as usize;
            let count = (src_x_end - src_x_start) as usize;

            let src_row = &pixels[src_offset..][..count];
            let dst_row = &mut framebuffer[dst_offset..][..count];

            // Blend: only copy non‑transparent (black = outline is fine, white = fill)
            for (s, d) in src_row.iter().zip(dst_row.iter_mut()) {
                if *s != 0 {  // non‑transparent pixel
                    *d = *s;
                }
            }
        }
    }

    /// Blit a single window's surface onto the framebuffer.
    fn draw_window(framebuffer: &mut [u32], fb_width: u32, fb_height: u32, window: &Window) {
        let src = &window.surface;

        // Source (surface) bounds
        let src_x_start = 0i32.max(-window.x);
        let src_y_start = 0i32.max(-window.y);
        let src_x_end = (src.width() as i32).min(fb_width as i32 - window.x);
        let src_y_end = (src.height() as i32).min(fb_height as i32 - window.y);

        if src_x_start >= src_x_end || src_y_start >= src_y_end {
            return; // completely clipped away
        }

        let dst_x = (window.x + src_x_start) as u32;
        let dst_y = (window.y + src_y_start) as u32;
        let copy_w = (src_x_end - src_x_start) as usize;
        let copy_h = (src_y_end - src_y_start) as usize;

        let src_pixels = src.pixels();

        for row in 0..copy_h {
            let sy = (src_y_start as usize) + row;
            let dy = (dst_y as usize) + row;

            let src_offset = sy * (src.width() as usize) + src_x_start as usize;
            let dst_offset = dy * (fb_width as usize) + dst_x as usize;

            let src_row = &src_pixels[src_offset..][..copy_w];
            let dst_row = &mut framebuffer[dst_offset..][..copy_w];
            dst_row.copy_from_slice(src_row);
        }
    }
}