//! Graphics subsystem — thin bridge to [`crate::contexts::FramebufferContext`].
//!
//! # Initialisation order
//!
//! 1. `efi_main_stage2` (boot phase) reads GOP parameters from the
//!    still-valid `args_ptr` and calls `store_raw_params` on the
//!    `FRAMEBUFFER` static.
//! 2. `init_common` → `init_graphics()` copies the stored params from
//!    the `FRAMEBUFFER` static into `KernelContext.framebuffer` and
//!    calls `build_renderer_from_stored()`.
use crate::contexts::kernel::{get_kernel, with_kernel_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// KernelArgs virtual address preserved in `.data` section to survive
/// page-table rebuilds that corrupt kernel `.bss`.
/// This is the higher-half VA passed by the bootloader's init_and_jump.
#[unsafe(link_section = ".data")]
pub static mut STORED_ARGS_VA: u64 = 0;

/// Store the virtual address of KernelArgs.
/// Called from `efi_main_stage2` before the world switch.
pub fn store_args_va(va: u64) {
    unsafe {
        STORED_ARGS_VA = va;
    }
    petroleum::serial::_print(format_args!("[store_args] va=0x{va:x}\n",));
}

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
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel lock acquired\n");
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

    // ── Copy framebuffer params from boot-phase FRAMEBUFFER static ──
    // efi_main_stage2 saved correct GOP values into the standalone
    // FRAMEBUFFER static (via define_context!) while args_ptr was valid.
    // KernelContext.framebuffer is a *different* instance created later
    // by init_kernel(), so it starts with all-zero defaults.
    //
    // Copy the stored values now so that build_renderer_from_stored()
    // uses the real panel dimensions.
    let copied = crate::contexts::framebuffer::with_framebuffer(|src| {
        if src.fb_phys >= 0x100000
            && src.fb_width_px > 0
            && src.fb_width_px <= 16384
            && src.fb_height_px > 0
            && src.fb_height_px <= 16384
            && src.fb_stride_bytes > 0
            && src.bpp == 32
        {
            Some((
                src.fb_phys,
                src.fb_width_px,
                src.fb_height_px,
                src.fb_stride_bytes,
                src.fb_pixel_format,
            ))
        } else {
            None
        }
    })
    .flatten();

    if let Some((phys, w, h, stride, pixel_format)) = copied {
        petroleum::write_serial_bytes(
            0x3F8,
            0x3FD,
            b"[init_gfx] copying boot-phase FB params to KernelContext\n",
        );
        with_kernel_mut(|k| {
            k.framebuffer
                .store_raw_params(phys, w, h, stride, 32, pixel_format);
        });
    } else {
        petroleum::write_serial_bytes(
            0x3F8,
            0x3FD,
            b"[init_gfx] FRAMEBUFFER static has no valid params\n",
        );
    }

    // ── Build renderer from stored params ──────────────────────
    petroleum::write_serial_bytes(
        0x3F8,
        0x3FD,
        b"[init_gfx] calling build_renderer_from_stored\n",
    );
    let built = with_kernel_mut(|k| k.framebuffer.build_renderer_from_stored()).unwrap_or(false);
    petroleum::write_serial_bytes(
        0x3F8,
        0x3FD,
        b"[init_gfx] build_renderer_from_stored returned\n",
    );

    if built {
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] GOP renderer built (identity mapping)\n"
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
    if let Some(mem) = crate::contexts::memory::get_memory().lock().as_mut() {
        let _ = mem.map_page(
            vga_virt as usize,
            vga_phys as usize,
            x86_64::structures::paging::PageTableFlags::NO_CACHE
                | x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE,
        );
    } else {
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
    with_kernel_mut(|k| k.framebuffer.vga_console = Some(vga));
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