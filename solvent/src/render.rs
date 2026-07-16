//! Desktop rendering — compositor pass, brightness, and framebuffer blit.
//!
//! Extracted from `lib.rs` to reduce the size of the god-module.

use crate::{DISPLAY_BRIGHTNESS_X100, HEAP_EXTEND_RESERVE, RUNTIME_CONTEXT};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::cursor::Cursor;
use lattice::scene::DirtyRect;
use lattice::shell_overlay::{ShellState, render_app_grid, render_task_overview};
use spin::Mutex;

const MAX_FB_PIXELS: usize = 3840 * 2160; // upper bound for overflow checks

// ── Framebuffer target for the compositor ────────────────────

struct FramebufferTarget<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}
impl RenderTarget for FramebufferTarget<'_> {
    fn buffer(&mut self) -> &mut [u32] {
        self.pixels
    }
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ── Progress callback ────────────────────────────────────────

static RENDER_PROGRESS_FN: Mutex<Option<fn(&[u8])>> = Mutex::new(None);

pub fn set_render_progress_fn(f: fn(&[u8])) {
    *RENDER_PROGRESS_FN.lock() = Some(f);
}

fn render_progress(label: &[u8]) {
    if let Some(f) = *RENDER_PROGRESS_FN.lock() {
        f(label);
    }
}

fn clip_region(
    region: lattice::scene::DirtyRect,
    width: u32,
    height: u32,
) -> Option<lattice::scene::DirtyRect> {
    (region.x < width && region.y < height)
        .then(|| {
            lattice::scene::DirtyRect::new(
                region.x,
                region.y,
                region.width.min(width - region.x),
                region.height.min(height - region.y),
            )
        })
        .filter(|region| region.width != 0 && region.height != 0)
}

fn blit_region(
    back: &[u32],
    back_width: usize,
    framebuffer: &mut [u32],
    framebuffer_stride: usize,
    region: lattice::scene::DirtyRect,
) {
    for row in region.y as usize..(region.y + region.height) as usize {
        let src = row * back_width + region.x as usize;
        let dst = row * framebuffer_stride + region.x as usize;
        let width = region.width as usize;
        let (Some(src_end), Some(dst_end)) = (src.checked_add(width), dst.checked_add(width))
        else {
            return;
        };
        let Some(source) = back.get(src..src_end) else {
            return;
        };
        if dst_end > framebuffer.len() {
            return;
        }
        // SAFETY: the range checks above prove that each destination pixel is
        // inside the framebuffer. Volatile stores are required for GOP MMIO.
        let destination = unsafe { framebuffer.as_mut_ptr().add(dst) };
        for (column, &pixel) in source.iter().enumerate() {
            unsafe { core::ptr::write_volatile(destination.add(column), pixel) };
        }
    }
}

fn cursor_region(position: (i32, i32), width: u32, height: u32) -> Option<DirtyRect> {
    let size = Cursor::SIZE as i32;
    let left = position.0 - Cursor::HOTSPOT_X;
    let top = position.1 - Cursor::HOTSPOT_Y;
    let right = left.saturating_add(size);
    let bottom = top.saturating_add(size);
    let x0 = left.max(0).min(width as i32);
    let y0 = top.max(0).min(height as i32);
    let x1 = right.max(0).min(width as i32);
    let y1 = bottom.max(0).min(height as i32);
    (x0 < x1 && y0 < y1)
        .then(|| DirtyRect::new(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

fn draw_cursor_strided(
    framebuffer: &mut [u32],
    width: u32,
    height: u32,
    stride: usize,
    cursor: &Cursor,
) {
    if !cursor.visible {
        return;
    }
    let left = cursor.x - Cursor::HOTSPOT_X;
    let top = cursor.y - Cursor::HOTSPOT_Y;
    let size = Cursor::SIZE as i32;
    for row in 0..size {
        let y = top + row;
        if y < 0 || y >= height as i32 {
            continue;
        }
        for column in 0..size {
            let x = left + column;
            if x < 0 || x >= width as i32 {
                continue;
            }
            let pixel = Cursor::shape()[(row * size + column) as usize];
            if pixel == 0 {
                continue;
            }
            let Some(index) = (y as usize)
                .checked_mul(stride)
                .and_then(|row_start| row_start.checked_add(x as usize))
            else {
                return;
            };
            if index >= framebuffer.len() {
                return;
            }
            // SAFETY: `index` was checked against the guard-owned slice.
            // Volatile stores are required for GOP MMIO.
            unsafe { core::ptr::write_volatile(framebuffer.as_mut_ptr().add(index), pixel) };
        }
    }
}

fn update_cursor_pixels(
    framebuffer: &mut petroleum::graphics::FramebufferGuard,
    back: &[u32],
    previous: (i32, i32),
    cursor: &Cursor,
) -> bool {
    let width = framebuffer.width();
    let height = framebuffer.height();
    let stride = framebuffer.stride() as usize;
    let Some(back_len) = (width as usize).checked_mul(height as usize) else {
        return false;
    };
    let Some(framebuffer_len) = stride.checked_mul(height as usize) else {
        return false;
    };
    if stride < width as usize
        || back.len() < back_len
        || framebuffer.pixels().len() < framebuffer_len
    {
        return false;
    }

    let pixels = framebuffer.pixels_mut();
    if let Some(region) = cursor_region(previous, width, height) {
        blit_region(back, width as usize, pixels, stride, region);
    }
    if let Some(region) = cursor_region((cursor.x, cursor.y), width, height) {
        blit_region(back, width as usize, pixels, stride, region);
    }
    draw_cursor_strided(pixels, width, height, stride, cursor);
    true
}

/// Apply a pending cursor-only update while the kernel owns the framebuffer.
///
/// The old and new cursor rectangles are restored from the clean RAM back
/// buffer. No framebuffer address or mutable slice is retained after the
/// `FramebufferGuard` borrow ends, and the GOP framebuffer is never read.
pub fn render_cursor_fast(framebuffer: &mut petroleum::graphics::FramebufferGuard) {
    if crate::RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    struct SuspendGuard;
    impl Drop for SuspendGuard {
        fn drop(&mut self) {
            crate::RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
        }
    }
    let _suspend = SuspendGuard;

    let mut runtime = RUNTIME_CONTEXT.runtime();
    let Some(runtime) = runtime.as_mut() else {
        return;
    };
    let Some(previous) = runtime.cursor_redraw_from.take() else {
        return;
    };
    let cursor = runtime.desktop.cursor.clone();
    let back = crate::BACK_BUFFER.lock();
    let updated = back
        .as_deref()
        .is_some_and(|back| update_cursor_pixels(framebuffer, back, previous, &cursor));
    if !updated {
        runtime.cursor_redraw_from = Some(previous);
        runtime.frame_due = true;
    }
}

// ── Main render function ─────────────────────────────────────

pub fn render(fb: &mut petroleum::graphics::FramebufferGuard) {
    if crate::RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    struct SuspendGuard;
    impl Drop for SuspendGuard {
        fn drop(&mut self) {
            crate::RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
        }
    }
    let _guard = SuspendGuard;

    let mut rt_lock = RUNTIME_CONTEXT.runtime();
    let rt = match rt_lock.as_mut() {
        Some(r) => r,
        None => return,
    };
    let pending_cursor_origin = rt.cursor_redraw_from;

    static PREV_SHELL_STATE: Mutex<ShellState> = Mutex::new(ShellState::Desktop);
    static PREV_TRANSITION: Mutex<bool> = Mutex::new(false);
    {
        let prev = *PREV_SHELL_STATE.lock();
        if rt.shell_state != prev {
            rt.desktop.force_full_redraw();
            *PREV_SHELL_STATE.lock() = rt.shell_state;
            *PREV_TRANSITION.lock() = true;
        }
    }

    crate::terminal::render_terminal(rt, rt.term_window);

    if !rt.editor_dirty {
        if let Some(editor_id) = rt.editor_window {
            if let Some(w) = rt.desktop.wm.windows().iter().find(|w| w.id == editor_id) {
                const GLYPH_W: u32 = 8;
                const GLYPH_H: u32 = 16;
                if w.surface.width() != (w.width / GLYPH_W).max(1) * GLYPH_W
                    || w.surface.height() != (w.height / GLYPH_H).max(1) * GLYPH_H
                {
                    rt.editor_dirty = true;
                }
            }
        }
    }

    render_progress(b"RENDER: pre-update");

    if rt.editor_dirty {
        crate::editor_bridge::render_editor(rt);
    }
    if rt.explorer_dirty {
        crate::render_explorer(rt);
    }
    if rt.settings_dirty {
        crate::settings_bridge::render_settings(rt);
    }

    let debug_msgs = nitrogen::debug::drain();
    let debug_changed = if !debug_msgs.is_empty() {
        let changed = rt.desktop.taskbar.debug_msgs != debug_msgs;
        rt.desktop.taskbar.debug_msgs = debug_msgs;
        changed
    } else {
        false
    };
    let tb_changed = rt.desktop.update_taskbar();
    render_progress(b"RENDER: got fb dims");
    let fb_width = fb.width();
    let fb_height = fb.height();
    let fb_stride_pixels = fb.stride();
    let fb_pixels = fb.pixels_mut();
    let fb_pixels_len = fb_pixels.len();
    *crate::FB_DIMS.lock() = (fb_width, fb_height, fb_stride_pixels);

    // A full render may supersede several queued mouse moves. Include the
    // earliest position still visible on the scanout so it cannot ghost.
    if let Some(previous) = pending_cursor_origin
        && let Some(region) = cursor_region(previous, fb_width, fb_height)
    {
        rt.desktop.push_dirty_rect(region);
    }

    let bar_h = lattice::taskbar::TASKBAR_HEIGHT;
    if rt.clock_changed || tb_changed || debug_changed {
        rt.desktop.push_dirty_rect(lattice::scene::DirtyRect::new(
            0,
            fb_height.saturating_sub(bar_h),
            fb_width,
            bar_h,
        ));
    }
    if rt.clock_changed {
        if lattice::top_panel::is_top_panel_enabled() {
            rt.desktop.push_dirty_rect(lattice::scene::DirtyRect::new(
                0,
                0,
                fb_width,
                lattice::top_panel::TOP_PANEL_HEIGHT,
            ));
        }
    }
    rt.clock_changed = false;

    rt.desktop.prepare_frame(fb_width, fb_height);
    let fb_stride = fb_stride_pixels as usize;
    let fb_len = fb_stride.saturating_mul(fb_height as usize);
    let back_len = (fb_width as usize).saturating_mul(fb_height as usize);
    if fb_len > MAX_FB_PIXELS || back_len > MAX_FB_PIXELS || fb_pixels_len < fb_len {
        render_progress(b"RENDER: skip (fb too large or invalid)");
        return;
    }
    rt.back_len = back_len;

    let has_dirty = rt.desktop.has_pending_dirty_rects();
    if has_dirty {
        render_progress(b"RENDER: has dirty");
        {
            let back_needed = back_len.saturating_mul(4);
            let reserve = HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed);
            if back_needed > reserve {
                let additional = back_needed.saturating_sub(reserve).next_multiple_of(4096);
                match RUNTIME_CONTEXT.callbacks().heap_extend {
                    Some(f) if f(additional).is_ok() => {
                        HEAP_EXTEND_RESERVE
                            .fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                    }
                    _ => return,
                }
            }
        }
        render_progress(b"RENDER: heap ok");

        let was_transition = {
            let mut prev = PREV_TRANSITION.lock();
            core::mem::replace(&mut *prev, false)
        };
        {
            render_progress(b"RENDER: alloc backbuf");
            let mut back_opt = crate::BACK_BUFFER.lock();
            let back = back_opt.get_or_insert_with(|| alloc::vec![0u32; back_len]);
            if back.len() < back_len {
                back.resize(back_len, 0);
            }
            let mut back_target = FramebufferTarget {
                pixels: &mut back[..back_len],
                width: fb_width,
                height: fb_height,
            };
            render_progress(b"RENDER: compositor");
            let mut scene = rt.desktop.scene();
            // Keep the RAM back buffer cursor-free. Cursor-only updates can
            // then restore clean pixels without reading from GOP memory.
            let cursor = scene.cursor.take();
            let (bx, by, bw, bh) = Compositor::render(&scene, &mut back_target);
            render_progress(b"RENDER: compositor done");
            let brightness = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
            if brightness < 100 && bw > 0 && bh > 0 {
                let back_w = fb_width as usize;
                let rows: core::ops::Range<usize> = if was_transition {
                    0..fb_height as usize
                } else {
                    (by as usize)..((by + bh) as usize)
                };
                let cols: core::ops::Range<usize> = if was_transition {
                    0..fb_width as usize
                } else {
                    (bx as usize)..((bx + bw) as usize)
                };
                for row in rows {
                    for col in cols.clone() {
                        let idx = row * back_w + col;
                        if idx < back_len {
                            back[idx] =
                                lattice::compositor::apply_brightness(back[idx], brightness);
                        }
                    }
                }
            }

            render_progress(b"RENDER: system layers");
            match rt.shell_state {
                ShellState::TaskOverview => {
                    render_task_overview(back, fb_width, fb_height, fb_width, scene.windows)
                }
                ShellState::AppGrid => render_app_grid(back, fb_width, fb_height, fb_width),
                ShellState::TimeZoneSelector => {
                    let offset = crate::clock::TIMEZONE_OFFSET_HOURS
                        .load(core::sync::atomic::Ordering::Relaxed);
                    lattice::shell_overlay::render_timezone_selector(
                        back, fb_width, fb_height, fb_width, offset,
                    );
                }
                ShellState::Desktop => {}
            }
            if rt.shell_state == ShellState::Desktop && lattice::top_panel::is_top_panel_enabled() {
                rt.desktop
                    .top_panel
                    .render(back, fb_width, fb_height, fb_width);
            }
            if was_transition {
                render_progress(b"RENDER: copy to fb");
                blit_region(
                    back,
                    fb_width as usize,
                    fb_pixels,
                    fb_stride,
                    lattice::scene::DirtyRect::full(fb_width, fb_height),
                );
            } else if bw > 0 && bh > 0 {
                render_progress(b"RENDER: copy dirty regions");
                for &region in scene.dirty_rects {
                    if let Some(region) = clip_region(region, fb_width, fb_height) {
                        blit_region(back, fb_width as usize, fb_pixels, fb_stride, region);
                    }
                }
            }
            if let Some(cursor) = cursor {
                draw_cursor_strided(fb_pixels, fb_width, fb_height, fb_stride, cursor);
            }
        }
    }
    rt.cursor_redraw_from = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_regions_to_visible_framebuffer() {
        assert_eq!(
            clip_region(lattice::scene::DirtyRect::new(2, 1, 4, 3), 5, 3),
            Some(lattice::scene::DirtyRect::new(2, 1, 3, 2))
        );
        assert_eq!(
            clip_region(lattice::scene::DirtyRect::new(5, 0, 1, 1), 5, 3),
            None
        );
    }

    #[test]
    fn blits_only_the_requested_region_and_preserves_stride_padding() {
        let back = [1, 2, 3, 4, 5, 6];
        let mut framebuffer = [0; 8];

        blit_region(
            &back,
            3,
            &mut framebuffer,
            4,
            lattice::scene::DirtyRect::new(1, 0, 2, 2),
        );

        assert_eq!(framebuffer, [0, 2, 3, 0, 0, 5, 6, 0]);
    }

    #[test]
    fn cursor_fast_path_restores_old_pixels_and_preserves_padding() {
        const WIDTH: usize = 32;
        const HEIGHT: usize = 24;
        const STRIDE: usize = 36;
        const PADDING: u32 = 0xdead_beef;
        let back: alloc::vec::Vec<u32> =
            (0..WIDTH * HEIGHT).map(|pixel| pixel as u32 + 1).collect();
        let mut pixels = alloc::vec![PADDING; STRIDE * HEIGHT];
        for row in 0..HEIGHT {
            pixels[row * STRIDE..row * STRIDE + WIDTH]
                .copy_from_slice(&back[row * WIDTH..(row + 1) * WIDTH]);
        }

        let previous = Cursor::new(3, 3);
        draw_cursor_strided(&mut pixels, WIDTH as u32, HEIGHT as u32, STRIDE, &previous);
        let current = Cursor::new(22, 12);
        {
            let mut guard = petroleum::graphics::FramebufferGuard::try_new(
                &mut pixels,
                WIDTH as u32,
                HEIGHT as u32,
                STRIDE as u32,
            )
            .unwrap();
            assert!(update_cursor_pixels(
                &mut guard,
                &back,
                (previous.x, previous.y),
                &current,
            ));
        }

        let old_origin = 2 * STRIDE + 2;
        assert_eq!(pixels[old_origin], back[2 * WIDTH + 2]);
        let new_origin = 11 * STRIDE + 21;
        assert_eq!(pixels[new_origin], 0xff00_0000);
        assert!((0..HEIGHT).all(|row| {
            pixels[row * STRIDE + WIDTH..(row + 1) * STRIDE]
                .iter()
                .all(|&pixel| pixel == PADDING)
        }));
    }

    #[test]
    fn cursor_fast_path_clips_at_the_visible_edge() {
        const WIDTH: usize = 8;
        const HEIGHT: usize = 8;
        const STRIDE: usize = 10;
        let back = alloc::vec![0x1234_5678; WIDTH * HEIGHT];
        let mut pixels = alloc::vec![0xa5a5_a5a5; STRIDE * HEIGHT];
        let cursor = Cursor::new(0, 0);
        {
            let mut guard = petroleum::graphics::FramebufferGuard::try_new(
                &mut pixels,
                WIDTH as u32,
                HEIGHT as u32,
                STRIDE as u32,
            )
            .unwrap();
            assert!(update_cursor_pixels(
                &mut guard,
                &back,
                (-100, -100),
                &cursor,
            ));
        }

        assert_eq!(pixels[0], 0xffff_ffff);
        assert!((0..HEIGHT).all(|row| {
            pixels[row * STRIDE + WIDTH..(row + 1) * STRIDE]
                .iter()
                .all(|&pixel| pixel == 0xa5a5_a5a5)
        }));
    }

    #[test]
    fn cursor_fast_path_rejects_an_incomplete_back_buffer() {
        let mut pixels = [0xfeed_face; 16];
        let original = pixels;
        let cursor = Cursor::new(2, 2);
        let mut guard =
            petroleum::graphics::FramebufferGuard::try_new(&mut pixels, 4, 4, 4).unwrap();

        assert!(!update_cursor_pixels(&mut guard, &[0; 15], (1, 1), &cursor,));
        assert_eq!(guard.pixels(), &original);
    }
}
