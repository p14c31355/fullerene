//! GUI subsystem — bridged to [`solvent`] runtime.
//!
//! This file serves as a thin bridge layer between kernel framebuffer
//! management and the Solvent runtime. All GUI/rendering logic lives
//! in `solvent`; this module only provides framebuffer access and
//! GPU present/flush, which are kernel-owned responsibilities.
//!
//! # Architecture
//!
//! ```text
//! Kernel (framebuffer memory, GPU present)
//!     ↓
//! gui.rs (framebuffer access, present/flush)
//!     ↓
//! Solvent (desktop state, compositor, events, timers)
//!     ↓
//! Lattice / Nozzle / Resonance / ChronoLine
//! ```

use petroleum::graphics::Renderer;
use solvent;

// Re-export solvent types used by other kernel modules
pub use solvent::{
    LatticeTerminal, MOUSE_STATE, MouseState,
    chrono_tick, is_initialized, poll_mouse_state,
    process_events, push_key_event, write_terminal,
};

/// Initialise the GUI subsystem via Solvent runtime.
pub fn init() {
    solvent::init();
}

/// Render the desktop onto the primary framebuffer.
///
/// Bridged from solvent, providing kernel-owned framebuffer access.
pub fn render() {
    // Render via solvent with framebuffer access from kernel
    solvent::render(get_framebuffer_slice);

    // Signal present & flush GPU (kernel-owned resource management)
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        renderer.present();
    }
    drop(renderer_lock);
    crate::graphics::flush_gpu();
}

/// Perform one tick of the runtime loop with kernel framebuffer access.
///
/// This wraps `solvent::runtime_tick` with the kernel framebuffer callback.
pub fn runtime_tick(now: u64) {
    solvent::runtime_tick(now, get_framebuffer_slice);

    // Signal present & flush GPU
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        renderer.present();
    }
    drop(renderer_lock);
    crate::graphics::flush_gpu();
}

// ── Framebuffer access (kernel-internal) ─────────────────────

/// Get a mutable slice of the framebuffer pixels and its dimensions.
fn get_framebuffer_slice() -> Option<(&'static mut [u32], u32, u32)> {
    let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    let renderer = renderer_lock.as_ref()?;
    let info = renderer.get_info();

    let fb_ptr = info.address as *mut u32;
    let fb_len = (info.width as usize) * (info.height as usize);

    let fb_pixels = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    Some((fb_pixels, info.width, info.height))
}