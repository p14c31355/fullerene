//! Framebuffer discovery — probes hardware for GOP parameters.
//!
//! Three probe strategies in priority order:
//! 1. `.data`-section globals (stored by `efi_main_stage2`)
//! 2. `KernelArgs` struct via `STORED_ARGS_VA`
//! 3. PCI BAR0 scan

use petroleum::common::EfiGraphicsPixelFormat;
use petroleum::graphics::boot_screen::BootFramebuffer;

/// Raw probe result — physical address, dimensions, stride (bytes), pixel format.
pub struct FramebufferProbeResult {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

/// GOP parameters stored in `.data` during `efi_main_stage2`.
/// Simple integers survive the world‑switch + shallow `clone_page_table`.
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

/// KernelArgs virtual address preserved in `.data`.
#[unsafe(link_section = ".data")]
pub static mut STORED_ARGS_VA: u64 = 0;

/// Store the virtual address of KernelArgs.  Called from `efi_main_stage2`.
pub fn store_args_va(va: u64) {
    unsafe {
        STORED_ARGS_VA = va;
    }
    petroleum::serial::_print(format_args!("[store_args] va=0x{va:x}\n"));
}

/// Store GOP parameters from the bootloader's KernelArgs.
/// Called from `efi_main_stage2` while `args_ptr` is valid.
pub fn store_boot_fb_params(
    phys: u64,
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    pixel_format: u32,
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

/// Return the framebuffer through the bootstrap's direct mapping.
///
/// The initial page table maps the first 64 GiB into the higher-half direct
/// map, which is shared by every process page table. Early Bellows diagnostics
/// may use the identity alias, but kernel-owned rendering must not retain it.
pub fn direct_boot_framebuffer() -> Option<BootFramebuffer> {
    let (phys, width, height, stride, bpp, format) = unsafe {
        (
            STORED_FB_PHYS,
            STORED_FB_WIDTH,
            STORED_FB_HEIGHT,
            STORED_FB_STRIDE,
            STORED_FB_BPP,
            STORED_FB_PIXEL_FORMAT,
        )
    };
    let size = u64::from(stride).checked_mul(u64::from(height))?;
    if phys.checked_add(size)? > 64 * 1024 * 1024 * 1024 {
        return None;
    }
    let direct_map_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    let direct_map_address = phys.checked_add(direct_map_offset)?;
    BootFramebuffer::new(direct_map_address, width, height, stride, bpp, format)
}

/// Discovery engine — tries each probe strategy in order.
pub struct FramebufferDiscovery;

impl FramebufferDiscovery {
    /// Probe `.data` globals saved by `efi_main_stage2`.
    pub fn probe_data_globals() -> Option<FramebufferProbeResult> {
        let (phys, width, height, stride, bpp, format) = unsafe {
            (
                STORED_FB_PHYS,
                STORED_FB_WIDTH,
                STORED_FB_HEIGHT,
                STORED_FB_STRIDE,
                STORED_FB_BPP,
                STORED_FB_PIXEL_FORMAT,
            )
        };
        let minimum_stride = width.checked_mul(4)?;
        let framebuffer_bytes = u64::from(stride).checked_mul(u64::from(height))?;
        if !(0x10_0000..1 << 52).contains(&phys)
            || !(80..=16_384).contains(&width)
            || !(25..=16_384).contains(&height)
            || bpp != 32
            || format > 1
            || stride < minimum_stride
            || stride % 4 != 0
            || framebuffer_bytes > 1 << 30
        {
            return None;
        }
        let pixel_format = match format {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => return None,
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] .data globals valid\n");
        Some(FramebufferProbeResult {
            phys,
            width,
            height,
            stride,
            pixel_format,
        })
    }

    /// Probe `KernelArgs` via `STORED_ARGS_VA`.
    pub fn probe_kernel_args() -> Option<FramebufferProbeResult> {
        let args_va = unsafe { STORED_ARGS_VA };
        if args_va < 0xFFFF_8000_0000_0000 {
            return None;
        }
        let args = unsafe { &*(args_va as *const petroleum::assembly::KernelArgs) };
        let mut buf = [0u8; 64];
        let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] KernelArgs fb=0x");
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");
        if args.fb_address < 0x100000
            || args.fb_width == 0
            || args.fb_width > 16384
            || args.fb_height == 0
            || args.fb_height > 16384
            || args.fb_bpp != 32
        {
            return None;
        }
        let stride = if args.fb_stride > 0 {
            args.fb_stride
        } else {
            args.fb_width.saturating_mul(4)
        };
        let pixel_format = match args.fb_pixel_format {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] KernelArgs valid\n");
        Some(FramebufferProbeResult {
            phys: args.fb_address,
            width: args.fb_width,
            height: args.fb_height,
            stride,
            pixel_format,
        })
    }

    /// Probe PCI BAR0 for a VGA-compatible display controller.
    pub fn probe_pci(pci_devices: &[nitrogen::pci::PciDevice]) -> Option<FramebufferProbeResult> {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] scanning PCI\n");
        for dev in pci_devices {
            let vendor = nitrogen::pci::PciConfigSpace::read_config_word(dev.bus, dev.device, 0, 0);
            if vendor == 0xFFFF || vendor == 0x0000 {
                continue;
            }
            let class =
                nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0B);
            let subclass =
                nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, 0, 0x0A);
            if class == 0x03 && subclass == 0x00 {
                // Intel GPUs (vendor 0x8086) use BAR2 for GTT aperture
                // (physical framebuffer behind BAR2 offset + 0).  Other
                // vendors use BAR0.
                let bar_reg = if vendor == 0x8086 { 0x18 } else { 0x10 };
                let bar = nitrogen::pci::PciConfigSpace::read_config_dword(
                    dev.bus, dev.device, 0, bar_reg,
                );
                let fb_phys = if (bar & 0x6) == 0x4 {
                    let bar_upper = nitrogen::pci::PciConfigSpace::read_config_dword(
                        dev.bus,
                        dev.device,
                        0,
                        bar_reg + 4,
                    );
                    ((bar_upper as u64) << 32) | ((bar & 0xFFFFFFF0) as u64)
                } else {
                    (bar & 0xFFFFFFF0) as u64
                };
                if fb_phys >= 0x100000 {
                    return Some(FramebufferProbeResult {
                        phys: fb_phys,
                        width: 1280,
                        height: 800,
                        stride: 1280 * 4,
                        pixel_format: EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
                    });
                }
            }
        }
        None
    }

    /// Run all probe strategies in priority order.
    pub fn discover(pci_devices: &[nitrogen::pci::PciDevice]) -> Option<FramebufferProbeResult> {
        Self::probe_data_globals()
            .or_else(|| Self::probe_kernel_args())
            .or_else(|| Self::probe_pci(pci_devices))
    }
}
