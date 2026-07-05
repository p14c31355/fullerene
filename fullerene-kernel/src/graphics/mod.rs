//! Graphics subsystem.
//!
//! # Architecture
//!
//! ```text
//! discovery.rs   FramebufferDiscovery  (hardware probe)
//!      ↓
//! mod.rs         init_graphics()        (orchestration)
//!      ↓
//! contexts/
//!   framebuffer.rs  FramebufferContext  (GOP / VGA backend)
//! ```
//!
//! # Initialisation order
//!
//! 1. `efi_main_stage2` stores GOP parameters in `.data` globals
//! 2. `init_common` → `init_graphics()` uses `FramebufferDiscovery`
//!    then `FramebufferContext::build_renderer_from_stored()`

pub mod discovery;

use crate::contexts::kernel::{get_kernel, with_kernel_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init_graphics() {
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] entry\n");
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] already initialized, returning\n");
        return;
    }

    // Ensure KernelContext exists.
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] getting kernel lock\n");
    {
        let kernel_lock = get_kernel();
        let kg = kernel_lock.lock();
        if kg.is_none() {
            petroleum::write_serial_bytes(
                0x3F8,
                0x3FD,
                b"[init_gfx] kernel is None, initializing\n",
            );
            drop(kg);
            crate::contexts::kernel::init_kernel();
        }
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel lock released\n");

    // ── Discover framebuffer parameters ─────────────────────────
    let probe = discovery::FramebufferDiscovery::discover();

    if let Some(ref p) = probe {
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] discovered {}x{} stride={}\n",
            p.width, p.height, p.stride
        ));
        with_kernel_mut(|k| {
            k.framebuffer
                .store_raw_params(p.phys, p.width, p.height, p.stride, 32, p.pixel_format);
        });
    } else {
        petroleum::write_serial_bytes(
            0x3F8,
            0x3FD,
            b"[init_gfx] no probe result, trying KernelContext fallback\n",
        );
    }

    // ── Build renderer ──────────────────────────────────────────
    petroleum::write_serial_bytes(
        0x3F8,
        0x3FD,
        b"[init_gfx] calling build_renderer_from_stored\n",
    );
    let built = with_kernel_mut(|k| k.framebuffer.build_renderer_from_stored()).unwrap_or(false);
    if built {
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] GOP renderer built (identity mapping)\n"
        ));
        return;
    }

    petroleum::serial::serial_log(format_args!(
        "[init_gfx] No valid GOP framebuffer; remaining headless.\n"
    ));
}

pub fn flush_gpu() {
    with_kernel_mut(|k| k.framebuffer.flush());
}
pub fn print_to_console(s: &str) {
    with_kernel_mut(|k| k.framebuffer.write_str(s));
    flush_gpu();
}
pub fn print_fmt(args: core::fmt::Arguments) {
    with_kernel_mut(|k| k.framebuffer.write_fmt(args));
    flush_gpu();
}
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}
