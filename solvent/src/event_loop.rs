//! Event dispatch, timer processing, service ticking, and frame pacing.

use lattice::shell_overlay::ShellState;
use resonance::Event;
use spin::Mutex;

use crate::{
    CURSOR_TIMER_ID, FRAME_INTERVAL_MS, FRAME_TIMER_ID, NETWORK_SNAPSHOT, RENDERING_SUSPENDED,
    RUNTIME_CONTEXT, SERVICES, TSC_PER_MS,
};

pub static GLOBAL_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

static LAST_RENDER_TSC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);
static LAST_USB_POLL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn chrono_tick(now: u64) {
    let mut runtime = RUNTIME_CONTEXT.runtime();
    let runtime = match runtime.as_mut() {
        Some(runtime) => runtime,
        None => return,
    };
    runtime.chrono.tick(now);
    while let Some(timer) = runtime.chrono.pop_expired() {
        match timer.id {
            CURSOR_TIMER_ID => {
                runtime.cursor_visible = !runtime.cursor_visible;
                runtime.term_dirty = true;
            }
            FRAME_TIMER_ID if runtime.shell_state == ShellState::Desktop => {
                runtime.frame_due = true;
            }
            _ => {}
        }
    }
}

pub fn push_key_event(event: Event) {
    if let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut() {
        queue.push(event);
    }
}

pub fn process_events() {
    let mut dispatcher = RUNTIME_CONTEXT.dispatcher();
    let mut queue = RUNTIME_CONTEXT.event_queue();
    if let (Some(dispatcher), Some(queue)) = (dispatcher.as_mut(), queue.as_mut()) {
        dispatcher.dispatch_queue(queue);
    }
}

pub fn set_render_fn(render_fn: fn()) {
    *RENDER_FN.lock() = Some(render_fn);
}

fn service_explorer_navigation() {
    let path = RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .and_then(|runtime| runtime.explorer.as_mut()?.take_navigation_request());
    let Some(path) = path else { return };

    // Filesystem and hardware I/O must run without the runtime lock. Rendering
    // takes locks in the opposite direction and synchronous removable-media I/O
    // here previously deadlocked the desktop when a directory was opened.
    let callback = RUNTIME_CONTEXT.callback_snapshot().vfs_readdir;
    let result = callback
        .ok_or(genome::FsError::NotSupported)
        .and_then(|read| read(&path));
    match &result {
        Ok(entries) => nitrogen::debug_status!("Explorer", "ready: {} entries", entries.len()),
        Err(error) => nitrogen::debug_status!("Explorer", "readdir failed: {}", error),
    }

    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut()
        && let Some(explorer) = runtime.explorer.as_mut()
    {
        explorer.finish_navigation(path, result);
        runtime.explorer_dirty = true;
        runtime.frame_due = true;
    }
}

fn service_explorer_copy() {
    let pending = RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .and_then(|runtime| runtime.explorer.as_mut()?.take_pending_copy());
    let Some(pending) = pending else { return };

    // I/O must run without the runtime lock (same as service_explorer_navigation).
    let callback = RUNTIME_CONTEXT.callback_snapshot().vfs_copy;
    let result = callback
        .ok_or(genome::FsError::NotSupported)
        .and_then(|copy| copy(&pending.source, &pending.destination, pending.is_dir));
    match &result {
        Ok(()) => nitrogen::debug_status!("Explorer", "pasted {}", pending.destination),
        Err(error) => nitrogen::debug_status!("Explorer", "paste failed: {}", error),
    }

    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut()
        && let Some(explorer) = runtime.explorer.as_mut()
    {
        explorer.finish_paste(&pending.destination, result);
        runtime.explorer_dirty = true;
        runtime.frame_due = true;
    }
}

pub fn tick_core(now: u64) {
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);

    crate::poll_mouse_state();
    crate::poll_keyboard();
    crate::clock::update_clock();
    chrono_tick(now);

    // Callbacks may acquire runtime locks or register another service.
    let mut services = core::mem::take(&mut *SERVICES.lock());
    for service in &mut services {
        service.tick(now);
    }
    let mut registry = SERVICES.lock();
    services.append(&mut *registry);
    *registry = services;

    if now.is_multiple_of(20) {
        let snapshot = NETWORK_SNAPSHOT.lock();
        let access_points = snapshot.aps.clone();
        let status = snapshot.status.clone();
        drop(snapshot);
        if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut()
            && runtime.desktop.update_ap_list(access_points, status)
        {
            runtime.frame_due = true;
        }
    }

    crate::viewers::tick_rle_playback();

    process_events();
    // File launch may have been queued by event handlers that ran inside
    // the runtime lock.  Process it now, outside the lock, so that VFS I/O
    // (called inside launch_file) cannot deadlock with the compositor.
    if let Some(path) = crate::window_api::PENDING_LAUNCH.lock().take() {
        crate::launch_file(&path);
    }
    // Auto-refresh the live kernel log viewer if open, every ~0.8s (50 ticks).
    if now % 50 == 0 {
        if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
            if runtime.klog_live_window.is_some() {
                runtime.klog_live_dirty = true;
                runtime.frame_due = true;
            }
        }
    }

    // NOTE: periodic kernel log write to SD card is DISABLED because the
    // SD card SPI driver can hang on writes, which defeats the purpose of
    // saving a crash log.  Use the shell command `klog > /mnt/klog.txt`
    // or the debug menu to export logs manually.
    // Deferred settings persistence (VFS write must happen outside the
    // runtime lock to avoid deadlocks with filesystem I/O).
    if crate::settings_bridge::PERSIST_PENDING.swap(false, core::sync::atomic::Ordering::Relaxed) {
        if let Some(save) = crate::RUNTIME_CONTEXT.callback_snapshot().settings_save {
            save();
        }
    }
    service_explorer_navigation();
    service_explorer_copy();
    if RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let pending = runtime.shell_launch_pending;
        runtime.shell_launch_pending = false;
        pending
    }) {
        crate::ensure_terminal_window();
        crate::launch_shell();
    }
    if RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let pending = runtime.editor_launch_pending;
        runtime.editor_launch_pending = false;
        pending
    }) {
        crate::ensure_editor_window();
    }
}

pub fn runtime_tick_no_fb() {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    tick_core(now);
    let do_render = RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let due = runtime.frame_due;
        if due {
            let frame_tsc = TSC_PER_MS
                .load(core::sync::atomic::Ordering::Relaxed)
                .saturating_mul(FRAME_INTERVAL_MS);
            let last = LAST_RENDER_TSC.load(core::sync::atomic::Ordering::Relaxed);
            let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
            if now_tsc.wrapping_sub(last) < frame_tsc {
                runtime.frame_due = true;
                return false;
            }
            LAST_RENDER_TSC.store(now_tsc, core::sync::atomic::Ordering::Relaxed);
            runtime.frame_due = false;
        }
        due
    });
    // Release RENDERING_SUSPENDED before calling render_fn, otherwise
    // render() will see it as already-suspended and early-return.
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    let render_fn = if do_render { *RENDER_FN.lock() } else { None };
    if let Some(render_fn) = render_fn {
        render_fn();
    }
}

pub fn consume_frame_due() -> bool {
    RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let due = runtime.frame_due;
        runtime.frame_due = false;
        due
    })
}

/// Return whether a cursor-only update is waiting for a framebuffer guard.
pub fn cursor_update_due() -> bool {
    RUNTIME_CONTEXT
        .runtime()
        .as_ref()
        .is_some_and(|runtime| runtime.cursor_redraw_from.is_some())
}

pub fn runtime_tick(now: u64, framebuffer: &mut petroleum::graphics::FramebufferGuard) {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    tick_core(now);

    let tick = GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed);
    if tick.wrapping_sub(LAST_USB_POLL.load(core::sync::atomic::Ordering::Relaxed)) >= 100 {
        LAST_USB_POLL.store(tick, core::sync::atomic::Ordering::Relaxed);
        let poll_usb = RUNTIME_CONTEXT.callback_snapshot().usb_poll;
        if let Some(poll_usb) = poll_usb
            && poll_usb()
            && let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut()
            && let Some(explorer) = runtime.explorer.as_mut()
        {
            explorer.refresh_sidebar();
            runtime.explorer_dirty = true;
            runtime.frame_due = true;
        }
    }

    let do_render = RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let due = runtime.frame_due;
        runtime.frame_due = false;
        due
    });
    // Release RENDERING_SUSPENDED before calling render(), otherwise
    // render() will see it as already-suspended and early-return.
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    if do_render {
        crate::render(framebuffer);
    } else if cursor_update_due() {
        crate::render_cursor_fast(framebuffer);
    }
}
