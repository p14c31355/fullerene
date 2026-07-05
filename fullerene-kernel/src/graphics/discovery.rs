//! Framebuffer discovery — probes hardware for GOP parameters.
//!
//! Three probe strategies in priority order:
//! 1. `.data`-section globals (stored by `efi_main_stage2`)
//! 2. `KernelArgs` struct via `STORED_ARGS_VA`
//! 3. PCI BAR0 scan

use petroleum::common::EfiGraphicsPixelFormat;

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
///
/// # Safety
///
/// This `static mut` is written exactly once during `efi_main_stage2` (boot phase)
/// and read-only thereafter.  Access is serialised by the single‑core boot sequence.
/// No concurrent readers or writers exist.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_PHYS: u64 = 0;
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_WIDTH: u32 = 0;
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_HEIGHT: u32 = 0;
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_STRIDE: u32 = 0;
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_BPP: u32 = 0;
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
#[unsafe(link_section = ".data")]
pub static mut STORED_FB_PIXEL_FORMAT: u32 = 0;

/// KernelArgs virtual address preserved in `.data`.
///
/// # Safety
/// Written once during boot (`efi_main_stage2`), read-only after. Single-core.
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

/// Discovery engine — tries each probe strategy in order.
pub struct FramebufferDiscovery;

impl FramebufferDiscovery {
    /// Probe `.data` globals saved by `efi_main_stage2`.
    pub fn probe_data_globals() -> Option<FramebufferProbeResult> {
        let (phys, w, h, stride, bpp, fmt_raw) = unsafe {
            (
                STORED_FB_PHYS,
                STORED_FB_WIDTH,
                STORED_FB_HEIGHT,
                STORED_FB_STRIDE,
                STORED_FB_BPP,
                STORED_FB_PIXEL_FORMAT,
            )
        };
        // Strict validation to reject garbage from a broken identity mapping.
        // On InsydeH2O, if args_phys_addr ≥ 64GB, the shallow clone_page_table
        // reads wrong physical memory, producing values like 1900544×4172873728.
        //
        // Checks:
        //   1. phys must be in a plausible MMIO range (0x100000 .. 256 GiB)
        //   2. width/height must be reasonable (80..16384 and 25..16384)
        //   3. stride must be ≈ width×4, with up to 256 bytes of padding
        //   4. bpp must be exactly 32
        //   5. pixel_format must be 0 (RGB) or 1 (BGR)
        if phys < 0x100000
            || phys > 0x40_0000_0000 // 256 GiB — no PCI MMIO above this
            || w < 80
            || w > 16384
            || h < 25
            || h > 16384
            || bpp != 32
            || fmt_raw > 1
        {
            return None;
        }
        // Stride sanity: should be ≈ width×4, with up to 4096 bytes of padding
        // (some GPU firmware reports stride aligned to 4KB or 64KB boundaries)
        let expected_stride_min = w.saturating_mul(4);
        let expected_stride_max = expected_stride_min.saturating_add(4096);
        if stride < expected_stride_min || stride > expected_stride_max {
            return None;
        }
        let pixel_format = match fmt_raw {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => return None,
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] .data globals valid\n");
        Some(FramebufferProbeResult {
            phys,
            width: w,
            height: h,
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

    /// Probe PCI BAR for a VGA-compatible display controller.
    ///
    /// NOTE: On Intel GPUs (vendor 0x8086), BAR0 (offset 0x10) is MMIO
    /// register space, NOT the framebuffer.  The framebuffer lives behind
    /// BAR2 (offset 0x18, GTT aperture).  Other vendors (AMD, NVIDIA)
    /// typically put the framebuffer at BAR0.  We try BAR2 for Intel,
    /// BAR0 for others.
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
                let fb_bar_offset = if vendor == 0x8086 { 0x18u8 } else { 0x10u8 };
                let upper_bar_offset = if vendor == 0x8086 { 0x1Cu8 } else { 0x14u8 };
                let bar = nitrogen::pci::PciConfigSpace::read_config_dword(
                    dev.bus, dev.device, 0, fb_bar_offset,
                );
                let fb_phys = if (bar & 0x6) == 0x4 {
                    let bar_upper = nitrogen::pci::PciConfigSpace::read_config_dword(
                        dev.bus, dev.device, 0, upper_bar_offset,
                    );
                    ((bar_upper as u64) << 32) | ((bar & 0xFFFFFFF0) as u64)
                } else {
                    (bar & 0xFFFFFFF0) as u64
                };
                if fb_phys >= 0x100000 {
                    // PCI BAR gives us the physical address but not the
                    // actual panel resolution.  Fall back to a safe default.
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
