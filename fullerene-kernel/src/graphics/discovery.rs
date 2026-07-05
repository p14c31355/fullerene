//! Validation of GOP parameters captured at the boot boundary.
//!
//! The bootloader owns GOP discovery. The kernel consumes that explicit
//! contract instead of guessing scanout addresses from PCI BARs.

use petroleum::common::EfiGraphicsPixelFormat;

pub struct FramebufferProbeResult {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: EfiGraphicsPixelFormat,
}

// These values are written once by the single-core stage-2 boot path and are
// read-only before secondary CPUs or interrupts are enabled.
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

pub struct FramebufferDiscovery;

impl FramebufferDiscovery {
    pub fn discover() -> Option<FramebufferProbeResult> {
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
            _ => unreachable!(),
        };
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"[discovery] GOP contract valid\n");
        Some(FramebufferProbeResult {
            phys,
            width,
            height,
            stride,
            pixel_format,
        })
    }
}
