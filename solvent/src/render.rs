//! Desktop rendering — compositor pass, brightness, cursor save/restore.
//!
//! Extracted from `lib.rs` to reduce the size of the god-module.

use crate::{DISPLAY_BRIGHTNESS_X100, HEAP_EXTEND_RESERVE, RUNTIME, SOLVENT_CALLBACKS};
use alloc::vec::Vec;
use lattice::compositor::{Compositor, RenderTarget};
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

// ── Cursor helpers ────────────────────────────────────────────

pub fn cursor_save_background(
    cursor: &lattice::cursor::Cursor,
    buf: &mut [u32; lattice::cursor::Cursor::SIZE as usize
             * lattice::cursor::Cursor::SIZE as usize],
    save_x: &mut i32,
    save_y: &mut i32,
    save_valid: &mut bool,
    fb: &[u32],
    fb_stride: u32,
    fb_width: u32,
    fb_height: u32,
) {
    if !cursor.visible {
        return;
    }
    let cur_sz = lattice::cursor::Cursor::SIZE as i32;
    let cx = cursor.x - lattice::cursor::Cursor::HOTSPOT_X;
    let cy = cursor.y - lattice::cursor::Cursor::HOTSPOT_Y;
    let stride_i = fb_stride as i32;
    let fbw_i = fb_width as i32;
    let fbh_i = fb_height as i32;
    let fb_len = (fb_stride as usize).saturating_mul(fb_height as usize);
    for row in 0..cur_sz {
        let sy = cy + row;
        for col in 0..cur_sz {
            let val = if sy >= 0 && sy < fbh_i {
                let sx = cx + col;
                if sx >= 0 && sx < fbw_i {
                    let idx = (sy * stride_i + sx) as usize;
                    if idx < fb_len { fb[idx] } else { 0 }
                } else {
                    0
                }
            } else {
                0
            };
            buf[(row * cur_sz + col) as usize] = val;
        }
    }
    *save_x = cx;
    *save_y = cy;
    *save_valid = true;
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

    let mut rt_lock = RUNTIME.lock();
    let rt = match rt_lock.as_mut() {
        Some(r) => r,
        None => return,
    };

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
    let back_len = (fb_width as usize) * (fb_height as usize);
    if fb_len > MAX_FB_PIXELS || back_len > MAX_FB_PIXELS {
        render_progress(b"RENDER: skip (fb too large)");
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
                let additional = back_needed
                    .saturating_sub(reserve)
                    .next_multiple_of(4096);
                match SOLVENT_CALLBACKS.lock().heap_extend {
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
            let scene = rt.desktop.scene();
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
            if was_transition || (bw > 0 && bh > 0) {
                let back_w = fb_width as usize;
                render_progress(b"RENDER: copy to fb");
                let fb_base = fb_pixels.as_mut_ptr();
                let fb_stride_u = fb_stride;
                if was_transition {
                    for row in 0..fb_height as usize {
                        let src_off = row * back_w;
                        let dst_off = row * fb_stride_u;
                        for col in 0..back_w {
                            unsafe {
                                core::ptr::write_volatile(
                                    fb_base.add(dst_off + col),
                                    back[src_off + col],
                                );
                            }
                        }
                    }
                } else {
                    for row in 0..bh {
                        let src_off = ((by + row) as usize) * back_w + (bx as usize);
                        let dst_off = ((by + row) as usize) * fb_stride_u + (bx as usize);
                        for col in 0..bw as usize {
                            unsafe {
                                core::ptr::write_volatile(
                                    fb_base.add(dst_off + col),
                                    back[src_off + col],
                                );
                            }
                        }
                    }
                }
            }
        }

        match rt.shell_state {
            ShellState::TaskOverview => render_task_overview(
                fb_pixels,
                fb_width,
                fb_height,
                fb_stride_pixels,
                rt.desktop.wm.windows(),
            ),
            ShellState::AppGrid => {
                render_app_grid(fb_pixels, fb_width, fb_height, fb_stride_pixels)
            }
            ShellState::TimeZoneSelector => {
                let offset =
                    crate::clock::TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);
                lattice::shell_overlay::render_timezone_selector(
                    fb_pixels,
                    fb_width,
                    fb_height,
                    fb_stride_pixels,
                    offset,
                );
            }
            ShellState::Desktop => {}
        }

        if rt.shell_state == ShellState::Desktop && lattice::top_panel::is_top_panel_enabled() {
            rt.desktop
                .top_panel
                .render(fb_pixels, fb_width, fb_height, fb_stride_pixels);
        }

        // Cursor handling: restore old cursor, save background, draw new cursor
        let cur_sz = lattice::cursor::Cursor::SIZE as i32;
        if rt.cursor_save_valid {
            let sx = rt.cursor_save_x;
            let sy = rt.cursor_save_y;
            let fbw_i = fb_width as i32;
            let fbh_i = fb_height as i32;
            for row in 0..cur_sz {
                let dy = sy + row;
                if dy < 0 || dy >= fbh_i {
                    continue;
                }
                for col in 0..cur_sz {
                    let dx = sx + col;
                    if dx < 0 || dx >= fbw_i {
                        continue;
                    }
                    let idx = (dy as usize) * fb_stride + (dx as usize);
                    if idx < fb_pixels_len {
                        fb_pixels[idx] = rt.cursor_save_buf[(row * cur_sz + col) as usize];
                    }
                }
            }
            rt.cursor_save_valid = false;
        }

        if rt.desktop.cursor.visible {
            cursor_save_background(
                &rt.desktop.cursor,
                &mut rt.cursor_save_buf,
                &mut rt.cursor_save_x,
                &mut rt.cursor_save_y,
                &mut rt.cursor_save_valid,
                fb_pixels,
                fb_stride_pixels,
                fb_width,
                fb_height,
            );
            Compositor::draw_cursor_direct(
                fb_pixels,
                fb_stride_pixels,
                fb_height,
                &rt.desktop.cursor,
            );
        }
    }

    if rt.usb_poll_pending {
        rt.usb_poll_pending = false;
        drop(rt_lock);
        let poll_fn = {
            let cb_guard = SOLVENT_CALLBACKS.lock();
            cb_guard.usb_poll
        };
        if let Some(f) = poll_fn {
            let _ = f();
        }
        if let Some(ref mut rt) = *RUNTIME.lock() {
            if let Some(ref mut explorer) = rt.explorer {
                explorer.refresh_sidebar();
                rt.explorer_dirty = true;
                rt.frame_due = true;
            }
        }
    }
}