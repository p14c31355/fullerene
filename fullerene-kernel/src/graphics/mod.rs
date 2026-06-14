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

/// Framebuffer parameters stored in `.data` section to survive
/// page-table rebuilds that corrupt kernel `.bss`.
/// Layout: [fb_phys, width, height, stride, bpp, pixel_format]
#[unsafe(link_section = ".data")]
pub static mut STORED_FB: [u64; 6] = [0; 6];

/// Store framebuffer parameters into the `.data`-backed static.
/// Called from `efi_main_stage2` before the world switch.
pub fn store_fb_params(phys: u64, w: u32, h: u32, stride: u32, bpp: u32, pixel_format: u32) {
    unsafe {
        STORED_FB[0] = phys;
        STORED_FB[1] = w as u64;
        STORED_FB[2] = h as u64;
        STORED_FB[3] = stride as u64;
        STORED_FB[4] = bpp as u64;
        STORED_FB[5] = pixel_format as u64;
    }
    // Debug: verify the write was persisted
    let addr = unsafe { core::ptr::addr_of!(STORED_FB) as u64 };
    let val0 = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STORED_FB[0])) };
    petroleum::serial::_print(format_args!(
        "[store_fb] addr=0x{addr:x} phys=0x{phys:x} w={w} h={h} str={stride} bpp={bpp} pf={pixel_format} STORED_FB[0]=0x{val0:x}\n",
    ));
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

    // ── Detect framebuffer: try STORED_FB, then PCI BAR0 scan ──
    // STORED_FB (.data section) is written by efi_main_stage2 before the
    // world switch but may be zeroed by clone_page_table / create_page_table
    // temporary mappings during init_memory_manager.
    // Fall back to reading VGA BAR0 directly from PCI config space.
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] reading STORED_FB (.data)\n");
    let mut fb_params: Option<(u64, u32, u32, u32)> = None;
    unsafe {
        let phys = STORED_FB[0];
        let w = STORED_FB[1] as u32;
        let h = STORED_FB[2] as u32;
        let stride = STORED_FB[3] as u32;
        let bpp = STORED_FB[4] as u32;
        if phys >= 0x100000 && w > 0 && w <= 16384 && h > 0 && h <= 16384 && stride > 0 && bpp == 32 {
            fb_params = Some((phys, w, h, stride));
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] STORED_FB valid\n");
        }
    }
    if fb_params.is_none() {
        // STORED_FB was corrupted — scan PCI config space for VGA BAR0.
        // Use the PCI device list already populated in KernelContext.
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] STORED_FB empty, scanning PCI\n");
        fb_params = with_kernel(|k| {
            for dev in k.pci.devices.iter() {
                let vendor = nitrogen::pci::PciConfigSpace::read_config_word(dev.bus, dev.device, 0, 0);
                if vendor == 0xFFFF || vendor == 0x0000 {
                    continue;
                }
                let class = nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0B);
                let subclass = nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0A);
                if class == 0x03 && subclass == 0x00 {
                    let bar0 = nitrogen::pci::PciConfigSpace::read_config_dword(dev.bus, dev.device, 0, 0x10);
                    let fb_phys = (bar0 & 0xFFFFFFF0) as u64;
                    if fb_phys >= 0x100000 {
                        // Use stored width/height from .data if available,
                        // otherwise default to 1280x800.
                        let (w, h, stride) = unsafe {
                            let sw = STORED_FB[1] as u32;
                            let sh = STORED_FB[2] as u32;
                            if sw > 0 && sw <= 16384 && sh > 0 && sh <= 16384 {
                                (sw, sh, sw.saturating_mul(4))
                            } else {
                                (1280, 800, 1280 * 4)
                            }
                        };
                        let mut buf = [0u8; 64];
                        let len = petroleum::serial::format_hex_to_buffer(fb_phys, &mut buf, 16);
                        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] PCI BAR0=0x");
                        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
                        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
                        return Some((fb_phys, w, h, stride));
                    }
                }
            }
            None
        }).flatten();
    }
    if let Some((fb_phys, w, h, stride)) = fb_params {
        let pixel_format = petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor;
        with_kernel_mut(|k| {
            k.framebuffer.store_raw_params(fb_phys, w, h, stride, 32, pixel_format);
        });
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] calling build_renderer_from_stored\n");
    let built = with_kernel_mut(|k| k.framebuffer.build_renderer_from_stored()).unwrap_or(false);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] build_renderer_from_stored returned\n");
    if built {
        // Register the framebuffer mapping in VirtualMemoryContext so
        // "where is framebuffer mapped?" is always answerable.
        if let Some(mem) = crate::contexts::memory::get_memory().lock().as_mut() {
            let fb_phys = with_kernel_mut(|k| k.framebuffer.fb_phys).unwrap_or(0);
            let fb_size = with_kernel_mut(|k| {
                k.framebuffer.fb_stride_bytes as u64 * k.framebuffer.fb_height_px as u64
            }).unwrap_or(0);
            if fb_phys >= 0x100000 && fb_size > 0 {
                let _ = mem.map_framebuffer_vm(fb_phys, fb_size);
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] FB recorded in MemoryContext\n");
            }
        }
        petroleum::serial::serial_log(format_args!(
            "[init_gfx] GOP renderer built from efi_main_stage2 params.\n"
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
