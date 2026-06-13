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
use crate::contexts::kernel::{get_kernel, with_kernel_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Framebuffer parameters stored in `.data` section to survive
/// page-table rebuilds that corrupt kernel `.bss`.
/// Layout: [fb_phys, width, height, stride, bpp]
#[unsafe(link_section = ".data")]
pub static mut STORED_FB: [u64; 5] = [0; 5];

/// Store framebuffer parameters into the `.data`-backed static.
/// Called from `efi_main_stage2` before the world switch.
pub fn store_fb_params(phys: u64, w: u32, h: u32, stride: u32, bpp: u32) {
    unsafe {
        STORED_FB[0] = phys;
        STORED_FB[1] = w as u64;
        STORED_FB[2] = h as u64;
        STORED_FB[3] = stride as u64;
        STORED_FB[4] = bpp as u64;
    }
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
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel is None, initializing\n");
            drop(kg);
            crate::contexts::kernel::init_kernel();
        }
        // Drop kg here to release the lock before with_kernel_mut
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel lock released\n");

    // Build the GOP renderer from parameters stored by
    // `efi_main_stage2` (store_raw_params).  The raw integers were
    // captured before the world‑switch so they survive pointer
    // corruption in `landing_zone_logic`.
    //
    // Both .bss and .data sections are corrupted by the shallow
    // page-table clone in init_memory_manager.  Instead, read the
    // framebuffer base address directly from the PCI VGA device's
    // BAR 0 (the PCI scan in the previous step already populated it).
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] probing PCI for VGA BAR0\n");
    {
        // Read BAR0 directly from VGA device (bus 0, device 1, function 0 for QEMU)
        // or search for any VGA-compatible device (class 0x03, subclass 0x00).
        for bus in 0u8..=0u8 {
            for dev in 0u8..=31u8 {
                let vendor = nitrogen::pci::PciConfigSpace::read_config_word(bus, dev, 0, 0);
                if vendor == 0xFFFF || vendor == 0x0000 {
                    continue;
                }
                let class = nitrogen::pci::PciConfigSpace::read_config_byte(bus, dev, 0, 0x0B);
                let subclass = nitrogen::pci::PciConfigSpace::read_config_byte(bus, dev, 0, 0x0A);
                if class == 0x03 && subclass == 0x00 {
                    // VGA-compatible device found, read BAR0
                    let bar0_low = nitrogen::pci::PciConfigSpace::read_config_dword(bus, dev, 0, 0x10);
                    let fb_phys = (bar0_low & 0xFFFFFFF0) as u64;
                    let mut buf = [0u8; 64];
                    let len = petroleum::serial::format_hex_to_buffer(fb_phys, &mut buf, 16);
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] PCI VGA BAR0=0x");
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
                    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
                    if fb_phys >= 0x100000 {
                        // Use default 1280x800x32 for now (matching GOP info from bellows log)
                        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] PCI BAR0 valid, storing\n");
                        with_kernel_mut(|k| {
                            k.framebuffer.store_raw_params(
                                fb_phys,
                                1280,
                                800,
                                1280 * 4,
                                32,
                                petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
                            );
                        });
                    }
                }
            }
        }
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] calling build_renderer_from_stored\n");
    let built = with_kernel_mut(|k| k.framebuffer.build_renderer_from_stored()).unwrap_or(false);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] build_renderer_from_stored returned\n");
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
