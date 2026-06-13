//! Graphics subsystem — thin bridge to [`crate::contexts::FramebufferContext`].
//!
//! # Initialisation order
//!
//! 1. `efi_main_stage2` (boot phase) creates the `FramebufferContext` and calls
//!    `init_from_kernel_args` while `args_ptr` is still valid.  This sets
//!    `renderer` before any world‑switch or page‑table rebuild can corrupt
//!    the pointer.
//! 2. `init_common` → `init_graphics()` is called later.  If `renderer` is
//!    already present (step 1), it is used as‑is.  Otherwise the function
//!    falls back to VGA text mode.
use crate::contexts::framebuffer::{get_framebuffer, with_framebuffer_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init_graphics() {
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    // Ensure FramebufferContext slot exists.
    if get_framebuffer().lock().is_none() {
        crate::contexts::framebuffer::init_framebuffer();
    }

    // Build the GOP renderer from parameters stored by
    // `efi_main_stage2` (store_raw_params).  The raw integers were
    // captured before the world‑switch so they survive pointer
    // corruption in `landing_zone_logic`.
    let built = with_framebuffer_mut(|fb| fb.build_renderer_from_stored()).unwrap_or(false);
    if built {
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] GOP renderer built from stored params.\n"
        ));
        return;
    }

    // No GOP renderer → VGA text mode fallback.
    petroleum::serial::serial_log(format_args!(
        "[init_gfx] No GOP renderer available, falling back to VGA text mode.\n"
    ));
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let vga_phys = petroleum::page_table::constants::VGA_MEMORY_START;
    let vga_virt = vga_phys + off;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().unwrap();
        let _ = mm.safe_map_page(
            vga_virt as usize,
            vga_phys as usize,
            x86_64::structures::paging::PageTableFlags::NO_CACHE
                | x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE,
        );
    }
    let mut vga = petroleum::graphics::text::VgaBuffer::with_address(vga_virt as usize);
    vga.enable();
    petroleum::graphics::Console::clear(&mut vga);
    let _ = core::fmt::write(&mut vga, format_args!("fullerene kernel — VGA text mode\n"));
    with_framebuffer_mut(|fb| fb.vga_console = Some(vga));
}

pub fn flush_gpu() {
    with_framebuffer_mut(|fb| fb.flush());
}
pub fn print_to_console(s: &str) {
    with_framebuffer_mut(|fb| fb.write_str(s));
    flush_gpu();
}
pub fn print_fmt(args: core::fmt::Arguments) {
    with_framebuffer_mut(|fb| fb.write_fmt(args));
    flush_gpu();
}
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}