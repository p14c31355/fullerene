//! Graphics subsystem — thin bridge to [`crate::contexts::FramebufferContext`].
//!
//! # Initialisation order
//!
//! 1. `efi_main_stage2` reads GOP parameters from the still-valid
//!    `args_ptr` and stores them in `.data`-section globals.
//! 2. `init_common` → `init_graphics()` reads the `.data` globals,
//!    falls back to `STORED_ARGS_VA` / PCI scan if needed, and
//!    calls `build_renderer_from_stored()`.
use crate::contexts::kernel::{get_kernel, with_kernel, with_kernel_mut};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// KernelArgs virtual address preserved in `.data` section.
#[unsafe(link_section = ".data")]
pub static mut STORED_ARGS_VA: u64 = 0;

/// GOP framebuffer parameters stored in `.data` during `efi_main_stage2`.
/// Simple integer values survive the world-switch and shallow
/// `clone_page_table` reliably (unlike the `FRAMEBUFFER` Mutex static
/// used by `define_context!`).
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_PHYS: u64 = 0;
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_WIDTH: u32 = 0;
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_HEIGHT: u32 = 0;
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_STRIDE: u32 = 0;
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_BPP: u32 = 0;
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_PIXEL_FORMAT: u32 = 0;

pub fn store_args_va(va: u64) {
    unsafe { STORED_ARGS_VA = va; }
    petroleum::serial::_print(format_args!("[store_args] va=0x{va:x}\n"));
}

/// Store GOP parameters from the bootloader's KernelArgs.
/// Called from `efi_main_stage2` while `args_ptr` is valid.
pub fn store_boot_fb_params(
    phys: u64, width: u32, height: u32, stride: u32, bpp: u32, pixel_format: u32,
) {
    unsafe {
        STORED_FB_PHYS = phys;
        STORED_FB_WIDTH = width;
        STORED_FB_HEIGHT = height;
        STORED_FB_STRIDE = stride;
        STORED_FB_BPP = bpp;
        STORED_FB_PIXEL_FORMAT = pixel_format;
    }
    petroleum::serial::_print(format_args!(
        "[store_fb] {width}x{height} stride={stride} phys=0x{phys:x} bpp={bpp} fmt={pixel_format}\n"
    ));
}

/// Read GOP parameters stored in `.data` by `efi_main_stage2`.
/// Returns `Some((phys, w, h, stride, pixel_format))` if valid.
fn read_boot_fb_params() -> Option<(u64, u32, u32, u32, petroleum::common::EfiGraphicsPixelFormat)> {
    let (phys, w, h, stride, bpp, fmt_raw) = unsafe {
        (STORED_FB_PHYS, STORED_FB_WIDTH, STORED_FB_HEIGHT,
         STORED_FB_STRIDE, STORED_FB_BPP, STORED_FB_PIXEL_FORMAT)
    };
    if phys < 0x100000 || w == 0 || w > 16384 || h == 0 || h > 16384
        || stride == 0 || bpp != 32
    {
        return None;
    }
    let pixel_format = match fmt_raw {
        0 => petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
        1 => petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        _ => return None,
    };
    Some((phys, w, h, stride, pixel_format))
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
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel is None, initializing\n");
            drop(kg);
            crate::contexts::kernel::init_kernel();
        }
    }
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] kernel lock released\n");

    // ── Path 1: Read GOP params from `.data` globals ──────────
    // efi_main_stage2 saved them before the world-switch.
    let mut fb_params: Option<(
        u64, u32, u32, u32, petroleum::common::EfiGraphicsPixelFormat,
    )> = read_boot_fb_params();
    if fb_params.is_some() {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] using .data globals\n");
    }

    // ── Path 2: Detect from KernelArgs via STORED_ARGS_VA ───
    if fb_params.is_none() {
        let args_va = unsafe { STORED_ARGS_VA };
        if args_va >= 0xFFFF_8000_0000_0000 {
            let args = unsafe { &*(args_va as *const petroleum::assembly::KernelArgs) };
            let mut buf = [0u8; 64];
            let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] KernelArgs fb=0x");
            petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
            if args.fb_address >= 0x100000
                && args.fb_width > 0 && args.fb_width <= 16384
                && args.fb_height > 0 && args.fb_height <= 16384
                && args.fb_bpp == 32
            {
                let stride = if args.fb_stride > 0 {
                    args.fb_stride * 4
                } else {
                    args.fb_width * 4
                };
                let pixel_format = match args.fb_pixel_format {
                    0 => petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    1 => petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
                    _ => petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
                };
                fb_params = Some((args.fb_address, args.fb_width, args.fb_height,
                                  stride, pixel_format));
                petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] KernelArgs valid\n");
            }
        }
    }

    // ── Path 3: PCI BAR0 scan ──────────────────────────────
    if fb_params.is_none() {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] scanning PCI\n");
        fb_params = with_kernel(|k| {
            for dev in k.pci.devices.iter() {
                let vendor = nitrogen::pci::PciConfigSpace::read_config_word(
                    dev.bus, dev.device, 0, 0);
                if vendor == 0xFFFF || vendor == 0x0000 { continue; }
                let class = nitrogen::pci::PciConfigSpace::read_config_byte(
                    dev.bus, dev.device, 0, 0x0B);
                let subclass = nitrogen::pci::PciConfigSpace::read_config_byte(
                    dev.bus, dev.device, 0, 0x0A);
                if class == 0x03 && subclass == 0x00 {
                    let bar0 = nitrogen::pci::PciConfigSpace::read_config_dword(
                        dev.bus, dev.device, 0, 0x10);
                    let fb_phys = (bar0 & 0xFFFFFFF0) as u64;
                    if fb_phys >= 0x100000 {
                        return Some((fb_phys, 1280, 800, 1280 * 4,
                            petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor));
                    }
                }
            }
            None
        }).flatten();
    }

    // ── Store params into KernelContext.framebuffer ─────────
    if let Some((phys, w, h, stride, pixel_format)) = fb_params {
        with_kernel_mut(|k| {
            k.framebuffer.store_raw_params(phys, w, h, stride, 32, pixel_format);
        });
    }

    // ── Build renderer ─────────────────────────────────────
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] calling build_renderer_from_stored\n");
    let built = with_kernel_mut(|k| k.framebuffer.build_renderer_from_stored()).unwrap_or(false);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[init_gfx] build_renderer_from_stored returned\n");

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
            vga_virt as usize, vga_phys as usize,
            x86_64::structures::paging::PageTableFlags::NO_CACHE
                | x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE,
        );
    } else {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().unwrap();
        let _ = mm.safe_map_page(
            vga_virt as usize, vga_phys as usize,
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