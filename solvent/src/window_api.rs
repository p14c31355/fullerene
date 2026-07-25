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
