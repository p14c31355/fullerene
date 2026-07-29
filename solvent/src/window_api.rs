//! Window lifecycle, redraw control, and file-launch integration.

use alloc::string::String;
use lattice::window::WindowId;

use crate::{FB_DIMS, RUNTIME_CONTEXT, RuntimeState, TERM_WIN_H, TERM_WIN_W};

pub(crate) static RENDERING_SUSPENDED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Path to launch, set by event handlers that hold the runtime lock.
/// The event loop picks this up after dropping the lock and calls
/// `launch_file` outside the locked section to avoid VFS deadlocks.
pub(crate) static PENDING_LAUNCH: spin::Mutex<Option<alloc::string::String>> =
    spin::Mutex::new(None);

pub fn write_terminal(text: &str) {
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.term_buf.put_str(text);
        runtime.term_dirty = true;
        runtime.frame_due = true;
    }
}

pub fn suspend_rendering() {
    RENDERING_SUSPENDED.store(true, core::sync::atomic::Ordering::SeqCst);
}

pub fn resume_rendering() {
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}

pub fn force_desktop_redraw() {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.desktop.force_full_redraw();
        runtime.frame_due = true;
    }
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}

pub fn create_window(
    title: impl Into<String>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<WindowId> {
    nitrogen::debug_status!("WASM", "window_api create enter");
    let result = RUNTIME_CONTEXT.runtime().as_mut().map(|runtime| {
        runtime
            .desktop
            .wm
            .create_titled_window(x, y, width, height, 0x000000, title)
    });
    nitrogen::debug_status!("WASM", "window_api create exit");
    result
}

pub fn with_window_surface<F, R>(id: WindowId, callback: F) -> Option<R>
where
    F: FnOnce(&mut [u32], u32, u32) -> R,
{
    nitrogen::debug_status!("WASM", "window_api surface enter");
    let result = RUNTIME_CONTEXT.runtime().as_mut().and_then(|runtime| {
        let window = runtime
            .desktop
            .wm
            .windows_mut()
            .iter_mut()
            .find(|window| window.id == id)?;
        if window.minimized {
            return None;
        }
        let (width, height) = (window.surface.width(), window.surface.height());
        Some(callback(window.surface.pixels_mut(), width, height))
    });
    nitrogen::debug_status!("WASM", "window_api surface exit");
    result
}

pub fn invalidate_window(id: WindowId) {
    nitrogen::debug_status!("WASM", "window_api invalidate enter");
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.desktop.invalidate_window(id);
        runtime.frame_due = true;
        runtime.term_dirty = true;
    }
    nitrogen::debug_status!("WASM", "window_api invalidate exit");
}

/// Open the live kernel-log viewer window from a non-GUI context (e.g. the
/// Nozzle shell).  This lets the user open Klog Live before launching a
/// command that may hang, so the timer-driven lock-free repaint keeps
/// displaying new `klog_fmt!` diagnostics even when the scheduler is stuck.
pub fn open_klog_live() -> bool {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    let opened = RUNTIME_CONTEXT.runtime().as_mut().map(|runtime| {
        crate::menu_actions::open_klog_live_window(runtime);
    });
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    opened.is_some()
}

/// Mark the live kernel-log window dirty from a synchronous diagnostic path.
/// Normally the event loop does this periodically, but a synchronous WASM
/// command prevents that loop from running while the command is blocked.
pub fn mark_klog_live_dirty() {
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut()
        && runtime.klog_live_window.is_some()
    {
        runtime.klog_live_dirty = true;
        runtime.frame_due = true;
    }
}

/// Lock-free state query for the timer-driven Klog Live repaint.
pub fn is_klog_live_active() -> bool {
    crate::runtime_context::KLOG_LIVE_ACTIVE.load(core::sync::atomic::Ordering::Acquire)
}

/// Return the existing Klog Live window's client-area geometry without taking
/// the runtime lock. The timer path uses this while the runtime may be stuck.
pub fn klog_live_surface_geometry() -> Option<(i32, i32, u32, u32)> {
    if !is_klog_live_active() {
        return None;
    }
    Some((
        crate::runtime_context::KLOG_LIVE_SURFACE_X.load(core::sync::atomic::Ordering::Relaxed),
        crate::runtime_context::KLOG_LIVE_SURFACE_Y.load(core::sync::atomic::Ordering::Relaxed),
        crate::runtime_context::KLOG_LIVE_SURFACE_WIDTH.load(core::sync::atomic::Ordering::Relaxed),
        crate::runtime_context::KLOG_LIVE_SURFACE_HEIGHT
            .load(core::sync::atomic::Ordering::Relaxed),
    ))
}

pub fn close_window(id: WindowId) -> bool {
    let mut context = RUNTIME_CONTEXT.runtime();
    let Some(runtime) = context.as_mut() else {
        return false;
    };
    let closed = runtime.desktop.wm.close_window(id);
    if closed {
        runtime
            .process_terminals
            .retain(|terminal| terminal.window_id != id);
    }
    if closed && runtime.klog_live_window == Some(id) {
        runtime.klog_live_window = None;
        crate::runtime_context::clear_klog_live_surface();
        runtime.frame_due = true;
    }
    closed
}

pub fn framebuffer_dims() -> (u32, u32) {
    let (width, height, _) = *FB_DIMS.lock();
    (width, height)
}

/// Return dimensions for a capture constrained to the requested maximum.
///
/// WASM applications need room for both the pixel buffer and their encoder
/// state.  Keeping the limit here means the guest never has to allocate a
/// native-resolution RGBA buffer just to discover that capture is too large.
pub fn scaled_framebuffer_dims(max_width: u32, max_height: u32) -> (u32, u32) {
    let (width, height) = framebuffer_dims();
    if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
        return (0, 0);
    }
    let mut output_width = width.min(max_width);
    let mut output_height = height.min(max_height);
    if width > max_width {
        output_height = ((height as u64 * max_width as u64) / width as u64)
            .max(1)
            .min(max_height as u64) as u32;
    }
    if height > max_height {
        output_width = ((width as u64 * max_height as u64) / height as u64)
            .max(1)
            .min(max_width as u64) as u32;
    }
    (output_width, output_height)
}

/// Copy the compositor's clean RAM back buffer for a screen capture.
///
/// The cursor is intentionally omitted: the back buffer is the same stable,
/// cursor-free image used to restore the GOP framebuffer during cursor-only
/// updates. This also avoids reading from device-backed framebuffer memory.
pub fn capture_screen() -> Option<(u32, u32, alloc::vec::Vec<u8>)> {
    capture_screen_scaled(u32::MAX, u32::MAX)
}

/// Copy one RGBA chunk of a constrained screen capture without allocating the
/// complete image in the kernel or WASM guest.
pub fn capture_screen_chunk(
    max_width: u32,
    max_height: u32,
    offset: usize,
    pixels: &mut [u8],
) -> Option<(u32, u32)> {
    if pixels.is_empty() || offset % 4 != 0 || pixels.len() % 4 != 0 {
        return None;
    }
    let (width, height, _) = *FB_DIMS.lock();
    let (output_width, output_height) = scaled_framebuffer_dims(max_width, max_height);
    let output_pixels = (output_width as usize).checked_mul(output_height as usize)?;
    let total_bytes = output_pixels.checked_mul(4)?;
    let end = offset.checked_add(pixels.len())?;
    if end > total_bytes {
        return None;
    }
    nitrogen::debug_status!(
        "CAPTURE",
        "chunk enter offset={} bytes={} output={}x{}",
        offset,
        pixels.len(),
        output_width,
        output_height
    );
    let back_guard = crate::BACK_BUFFER.try_lock()?;
    let (current_width, current_height, _) = *FB_DIMS.try_lock()?;
    if (current_width, current_height) != (width, height) {
        return None;
    }
    let back = back_guard.as_ref()?;
    let source_width = width as usize;
    let source_height = height as usize;
    let source_pixels = source_width.checked_mul(source_height)?;
    if width == 0 || height == 0 || back.len() < source_pixels {
        return None;
    }
    let first_pixel = offset / 4;
    for (chunk_pixel, output) in pixels.chunks_exact_mut(4).enumerate() {
        let output_pixel = first_pixel + chunk_pixel;
        let output_row = output_pixel / output_width as usize;
        let output_column = output_pixel % output_width as usize;
        let source_row = output_row * source_height / output_height as usize;
        let source_column = output_column * source_width / output_width as usize;
        let pixel = back.as_slice()[source_row * source_width + source_column];
        output[0] = ((pixel >> 16) & 0xFF) as u8;
        output[1] = ((pixel >> 8) & 0xFF) as u8;
        output[2] = (pixel & 0xFF) as u8;
        output[3] = 0xFF;
    }
    nitrogen::debug_status!(
        "CAPTURE",
        "chunk exit offset={} bytes={}",
        offset,
        pixels.len()
    );
    Some((output_width, output_height))
}

/// Copy the compositor back buffer, optionally downsampling it to fit within
/// `max_width` x `max_height`.
pub fn capture_screen_scaled(
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32, alloc::vec::Vec<u8>)> {
    const MAX_CAPTURE_RGBA_BYTES: usize = 32 * 1024 * 1024;
    nitrogen::debug_status!("CAPTURE", "dimensions enter");
    let (width, height, _framebuffer_stride) = *FB_DIMS.lock();
    // A synchronous WASM command can run while another CPU is rendering.
    // Never spin forever waiting for the compositor's back-buffer lock: a
    // failed capture is recoverable, whereas a blocked shell is not.
    let width_usize = width as usize;
    let height_usize = height as usize;
    let pixel_count = width_usize.checked_mul(height_usize)?;
    let (output_width, output_height) = scaled_framebuffer_dims(max_width, max_height);
    let output_width_usize = output_width as usize;
    let output_height_usize = output_height as usize;
    nitrogen::debug_status!(
        "CAPTURE",
        "dimensions exit {}x{} output={}x{} pixels={}",
        width,
        height,
        output_width,
        output_height,
        pixel_count
    );
    {
        nitrogen::debug_status!("CAPTURE", "back buffer probe enter");
        let back_guard = crate::BACK_BUFFER.try_lock()?;
        let back = back_guard.as_ref()?;
        // BACK_BUFFER is a tightly packed width*height image, even when the
        // GOP framebuffer has padding at the end of each physical scanline.
        if width == 0
            || height == 0
            || back.len() < pixel_count
            || pixel_count > MAX_CAPTURE_RGBA_BYTES / 4
        {
            nitrogen::debug_status!("CAPTURE", "back buffer probe rejected");
            return None;
        }
        nitrogen::debug_status!("CAPTURE", "back buffer probe exit len={}", back.len());
    }

    // Allocate before taking the back-buffer guard so the allocator cannot
    // become part of the lock ordering.
    nitrogen::debug_status!("CAPTURE", "host allocation enter bytes={}", pixel_count * 4);
    let output_pixel_count = output_width_usize.checked_mul(output_height_usize)?;
    let output_bytes = output_pixel_count.checked_mul(4)?;
    let mut pixels = alloc::vec::Vec::with_capacity(output_bytes);
    nitrogen::debug_status!("CAPTURE", "host allocation exit");
    let back_guard = crate::BACK_BUFFER.try_lock()?;
    // Render updates FB_DIMS before taking BACK_BUFFER, so use try_lock here
    // to avoid acquiring the two locks in the opposite order and deadlocking
    // a concurrent render.
    let (current_width, current_height, _) = *FB_DIMS.try_lock()?;
    if (current_width, current_height) != (width, height) {
        return None;
    }
    let back = back_guard.as_ref()?;
    let width_usize = current_width as usize;
    let height_usize = current_height as usize;
    let current_pixel_count = width_usize.checked_mul(height_usize)?;
    if current_width == 0
        || current_height == 0
        || back.len() < current_pixel_count
        || current_pixel_count > MAX_CAPTURE_RGBA_BYTES / 4
    {
        nitrogen::debug_status!("CAPTURE", "copy rejected after lock");
        return None;
    }
    nitrogen::debug_status!(
        "CAPTURE",
        "copy enter {}x{} -> {}x{}",
        current_width,
        current_height,
        output_width,
        output_height
    );
    for row in 0..output_height_usize {
        let source_row = row * height_usize / output_height_usize;
        let start = source_row * width_usize;
        for column in 0..output_width_usize {
            let source_column = column * width_usize / output_width_usize;
            let pixel = back.as_slice()[start + source_column];
            pixels.push(((pixel >> 16) & 0xFF) as u8);
            pixels.push(((pixel >> 8) & 0xFF) as u8);
            pixels.push((pixel & 0xFF) as u8);
            pixels.push(0xFF);
        }
    }
    nitrogen::debug_status!("CAPTURE", "copy exit bytes={}", pixels.len());
    Some((output_width, output_height, pixels))
}

pub fn ensure_terminal_window() -> Option<WindowId> {
    let mut runtime = RUNTIME_CONTEXT.runtime();
    let runtime = runtime.as_mut()?;
    if let Some(id) = runtime.term_window
        && runtime
            .desktop
            .wm
            .windows()
            .iter()
            .any(|window| window.id == id)
    {
        return Some(id);
    }
    let id = runtime
        .desktop
        .wm
        .create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    runtime.term_window = Some(id);
    runtime.desktop.force_full_redraw();
    runtime.frame_due = true;
    runtime.term_dirty = true;
    Some(id)
}

pub fn ensure_editor_window() -> Option<WindowId> {
    RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .and_then(crate::editor_bridge::ensure_editor_window)
}

pub(crate) fn render_explorer(runtime: &mut RuntimeState) {
    let explorer = match runtime.explorer.as_mut() {
        Some(explorer) => explorer,
        None => return,
    };
    let explorer_id = match explorer.window_id {
        Some(id) => id,
        None => return,
    };
    let window = match runtime
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|window| window.id == explorer_id)
    {
        Some(window) => window,
        None => {
            runtime.explorer = None;
            runtime.explorer_dirty = false;
            return;
        }
    };
    crate::explorer::render_explorer(explorer, &mut window.surface);
    runtime.desktop.invalidate_window(explorer_id);
    runtime.explorer_dirty = false;
}

pub fn launch_file(path: &str) {
    crate::viewer::open(path);
}
