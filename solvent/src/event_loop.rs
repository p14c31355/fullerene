//! Event dispatch, timer processing, service ticking, and frame pacing.

use alloc::vec::Vec;
use lattice::shell_overlay::ShellState;
use resonance::{Event, InputEvent};
use spin::Mutex;

use crate::{
    CURSOR_TIMER_ID, FRAME_INTERVAL_MS, FRAME_TIMER_ID, NETWORK_SNAPSHOT, RENDERING_SUSPENDED,
    RUNTIME_CONTEXT, SERVICES, TSC_PER_MS,
};

pub static GLOBAL_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

static LAST_RENDER_TSC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static TICK_CORE_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);
static CURSOR_RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);
static LAST_USB_POLL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn process_pointer_motion_only() {
    let events = RUNTIME_CONTEXT
        .event_queue()
        .as_mut()
        .map(|queue| {
            let count = queue.len();
            (0..count).filter_map(|_| queue.pop()).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut retained = Vec::with_capacity(events.len());
    for event in events {
        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                crate::handlers::apply_pointer_motion(x, y);
            }
            other => retained.push(other),
        }
    }
    if let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut() {
        for event in retained.into_iter().rev() {
            queue.push_front(event);
        }
    }
}

struct TickCoreGuard;

impl TickCoreGuard {
    fn enter() -> Self {
        TICK_CORE_ACTIVE.store(true, core::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for TickCoreGuard {
    fn drop(&mut self) {
        TICK_CORE_ACTIVE.store(false, core::sync::atomic::Ordering::Release);
    }
}

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

/// Install the kernel callback used for cursor-only framebuffer updates.
///
/// Synchronous callers such as Nozzle cannot borrow a framebuffer directly,
/// so `runtime_tick_no_fb` uses this callback for the cheap cursor path.
pub fn set_cursor_render_fn(render_fn: fn()) {
    *CURSOR_RENDER_FN.lock() = Some(render_fn);
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
    let _tick_core = TickCoreGuard::enter();
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);

    // Drain the I2C-HID FIFO before consuming input.  The scheduler idle
    // loop services this separately (scheduler.rs), but while it is blocked
    // inside shell_main/nozzle the only entry point that runs is
    // runtime_tick_no_fb -> tick_core.  Without this call consume_input()
    // in poll_mouse_state always returns None and the touchpad cursor
    // freezes for the whole Nozzle session.
    nitrogen::i2c_hid::service_input();
    crate::poll_mouse_state();
    crate::poll_keyboard();
    crate::clock::update_clock();
    chrono_tick(now);

    // Skip service ticking while inside a WASM host callback.  Services
    // like WifiService can block for seconds on firmware MMIO, which
    // freezes the WASM caller (e.g. the MP4 viewer's decode loop) that
    // invoked `wait_for_ns` precisely to let the event loop run.
    let in_wasm = crate::IN_WASM_HOST_CALLBACK.load(core::sync::atomic::Ordering::Relaxed);
    if !in_wasm && !crate::HEADLESS_SMOKE_ACTIVE.load(core::sync::atomic::Ordering::Acquire) {
        // Callbacks may acquire runtime locks or register another service.
        let mut services = core::mem::take(&mut *SERVICES.lock());
        for service in &mut services {
            service.tick(now);
        }
        let mut registry = SERVICES.lock();
        services.append(&mut *registry);
        *registry = services;
    }

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
    crate::installer::service_install_request();
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

/// Lightweight HID input pump for the scheduler idle loop.
///
/// This mirrors the nested branch of [`runtime_tick_no_fb`]: drain the
/// I2C-HID FIFO, update mouse state, process pointer motion, and render
/// the cursor.  The scheduler calls this between long device phases
/// (storage, USB, Wi-Fi) so the cursor stays responsive on machines
/// whose touchpad is delivered over I2C-HID — the same responsiveness
/// the shell (Nozzle) gets from its tight `read_byte` → `runtime_tick_no_fb`
/// loop.
pub fn pump_hid_cursor() {
    let already_suspended = RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst);
    nitrogen::i2c_hid::service_input();
    crate::poll_mouse_state();
    crate::poll_keyboard();
    process_pointer_motion_only();
    let cursor_only = RUNTIME_CONTEXT
        .runtime()
        .as_ref()
        .is_some_and(|runtime| runtime.cursor_redraw_from.is_some());
    if cursor_only && !already_suspended {
        RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
        if let Some(render_fn) = *CURSOR_RENDER_FN.lock() {
            render_fn();
        }
    }
    RENDERING_SUSPENDED.store(already_suspended, core::sync::atomic::Ordering::SeqCst);
}

pub fn runtime_tick_no_fb() {
    let already_suspended = RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst);
    let tick_core_active = TICK_CORE_ACTIVE.load(core::sync::atomic::Ordering::Acquire);
    if already_suspended || tick_core_active {
        // The shell and the synchronous WASM viewer are both entered from
        // inside the normal event-loop tick.  In that case a nested
        // runtime_tick_no_fb used to be discarded completely.  That left a
        // launched Linux process without a scheduler handoff and left the
        // KLog Live surface stale until the synchronous caller returned.
        // Pump only input and the already-due compositor work here; do not
        // re-enter tick_core(), which could recursively launch another file
        // or shell while the outer tick is still active.
        nitrogen::i2c_hid::service_input();
        crate::poll_mouse_state();
        crate::poll_keyboard();
        process_pointer_motion_only();
        if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
            if runtime.klog_live_window.is_some() {
                runtime.klog_live_dirty = true;
                runtime.frame_due = true;
            }
        }
        let frame_tsc = TSC_PER_MS
            .load(core::sync::atomic::Ordering::Relaxed)
            .saturating_mul(FRAME_INTERVAL_MS);
        let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let (do_render, cursor_only) = RUNTIME_CONTEXT
            .runtime()
            .as_mut()
            .map(|runtime| {
                if !runtime.frame_due {
                    return (false, runtime.cursor_redraw_from.is_some());
                }
                // A video frame has its own presentation deadline in the WASM
                // viewer. Do not quantize it to the desktop's 17 ms refresh
                // throttle: that turns a 30 fps stream into alternating short
                // and long display intervals and is visible as judder.
                let video_frame_due = runtime.video_dirty_window.is_some();
                let last = LAST_RENDER_TSC.load(core::sync::atomic::Ordering::Relaxed);
                if !video_frame_due && now_tsc.wrapping_sub(last) < frame_tsc {
                    return (false, false);
                }
                LAST_RENDER_TSC.store(now_tsc, core::sync::atomic::Ordering::Relaxed);
                runtime.frame_due = false;
                (true, false)
            })
            .unwrap_or((false, false));
        if do_render || cursor_only {
            // Keep the outer tick marked as suspended while the nested pump
            // is idle, but release it around the renderer itself because the
            // renderer uses the same guard to reject recursive frames.
            RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
            let render_fn = if do_render {
                *RENDER_FN.lock()
            } else {
                *CURSOR_RENDER_FN.lock()
            };
            if let Some(render_fn) = render_fn {
                render_fn();
            }
        }
        RENDERING_SUSPENDED.store(already_suspended, core::sync::atomic::Ordering::SeqCst);
        return;
    }
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    tick_core(now);
    let (do_render, cursor_only) = RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .map(|runtime| {
            let due = runtime.frame_due;
            if due {
                let video_frame_due = runtime.video_dirty_window.is_some();
                let frame_tsc = TSC_PER_MS
                    .load(core::sync::atomic::Ordering::Relaxed)
                    .saturating_mul(FRAME_INTERVAL_MS);
                let last = LAST_RENDER_TSC.load(core::sync::atomic::Ordering::Relaxed);
                let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
                if !video_frame_due && now_tsc.wrapping_sub(last) < frame_tsc {
                    runtime.frame_due = true;
                    return (false, false);
                }
                LAST_RENDER_TSC.store(now_tsc, core::sync::atomic::Ordering::Relaxed);
                runtime.frame_due = false;
            }
            (due, !due && runtime.cursor_redraw_from.is_some())
        })
        .unwrap_or((false, false));
    // Release RENDERING_SUSPENDED before calling render_fn, otherwise
    // render() will see it as already-suspended and early-return.
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    let render_fn = if do_render {
        *RENDER_FN.lock()
    } else if cursor_only {
        *CURSOR_RENDER_FN.lock()
    } else {
        None
    };
    if let Some(render_fn) = render_fn {
        render_fn();
    }
}

/// Render an already-due frame without polling input, services, or events.
///
/// Scheduler diagnostics use this immediately before a context switch. The
/// normal tick would be too late when the switch enters a faulting or stalled
/// user transition, and calling the full tick here could recursively launch
/// work while the shell is yielding.
pub fn flush_frame_no_fb() {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let due = RUNTIME_CONTEXT.runtime().as_mut().is_some_and(|runtime| {
        let due = runtime.frame_due;
        runtime.frame_due = false;
        due
    });
    let render_fn = if due { *RENDER_FN.lock() } else { None };
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
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
