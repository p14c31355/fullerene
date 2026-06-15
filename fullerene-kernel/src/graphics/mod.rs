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
use crate::contexts::kernel::{get_kernel, with_kernel, with_kernel_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// KernelArgs virtual address preserved in `.data` section to survive
/// page-table rebuilds that corrupt kernel `.bss`.
/// This is the higher-half VA passed by the bootloader's init_and_jump.
#[unsafe(link_section = ".data")]
pub static mut STORED_ARGS_VA: u64 = 0;

/// Store the virtual address of KernelArgs so init_graphics can
/// read GOP parameters directly from the bootloader's allocation.
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

    // Ensure FramebufferContext slot exists (KernelContext owns it now).
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
        // Drop kg here to release the lock before with_kernel_mut
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel lock released\n");

    // ── Detect framebuffer: try KernelArgs via STORED_ARGS_VA, then PCI BAR0 ──
    // STORED_ARGS_VA (.data) holds the higher-half VA of KernelArgs, set by
    // efi_main_stage2 before the world-switch.  The bootloader's init_and_jump
    // identity-maps kernel_args_page, and shallow clone_page_table preserves it.
    let mut fb_params: Option<(u64, u32, u32, u32)> = None;
    let args_va = unsafe { STORED_ARGS_VA };
    if args_va >= 0xFFFF_8000_0000_0000 {
        let args = unsafe { &*(args_va as *const petroleum::assembly::KernelArgs) };
        let mut buf = [0u8; 64];
        let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] KernelArgs fb=0x");
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        if args.fb_address >= 0x100000
            && args.fb_width > 0
            && args.fb_width <= 16384
            && args.fb_height > 0
            && args.fb_height <= 16384
            && args.fb_bpp == 32
        {
            let stride = if args.fb_stride > 0 {
                args.fb_stride * 4
            } else {
                args.fb_width * 4
            };
            fb_params = Some((args.fb_address, args.fb_width, args.fb_height, stride));
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] KernelArgs valid\n");
        }
    }
    if fb_params.is_none() {
        petroleum::write_serial_bytes(
            0x3F8,
            0x3FD,
            b"[init_gfx] KernelArgs invalid, scanning PCI\n",
        );
        fb_params = with_kernel(|k| {
            for dev in k.pci.devices.iter() {
                let vendor =
                    nitrogen::pci::PciConfigSpace::read_config_word(dev.bus, dev.device, 0, 0);
                if vendor == 0xFFFF || vendor == 0x0000 {
                    continue;
                }
                let class =
                    nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0B);
                let subclass =
                    nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0A);
                if class == 0x03 && subclass == 0x00 {
                    let bar0 = nitrogen::pci::PciConfigSpace::read_config_dword(
                        dev.bus, dev.device, 0, 0x10,
                    );
                    let fb_phys = (bar0 & 0xFFFFFFF0) as u64;
                    if fb_phys >= 0x100000 {
                        let w = 1280u32;
                        let h = 800u32;
                        return Some((fb_phys, w, h, w * 4));
                    }
                }
            }
            None
        })
        .flatten();
    }
    if let Some((fb_phys, w, h, stride)) = fb_params {
        let pixel_format =
            petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor;
        with_kernel_mut(|k| {
            k.framebuffer
                .store_raw_params(fb_phys, w, h, stride, 32, pixel_format);
        });
    }
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
        // The framebuffer is already identity-mapped by the bootloader
        // via a 2MB/1GB huge page.  Do NOT call `map_framebuffer_vm()`
        // here because it tries to split the huge page into 4KB WC pages,
        // which breaks the entire mapping on InsydeH2O firmware.
        //
        // See README.md § "Real Hardware Compatibility" item 3.
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] GOP renderer built (boot-phase huge-page mapping preserved)\n"
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
    // Use MemoryContext's high-level API (preferred) with fallback to legacy.
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
