//! VirtIO-GPU driver — thin kernel wrapper.
//!
//! Hardware-level initialisation (PCI probe, BAR mapping, queue setup,
//! display negotiation) is handled by `nitrogen::virtio::gpu::init`.
//!
//! This module bridges the nitrogen result to `petroleum::graphics`
//! types and creates the `UefiFramebufferWriter`.

use alloc::boxed::Box;
use nitrogen::virtio::gpu::VirtioGpu;
use nitrogen::DriverContext;
use petroleum::graphics::UefiFramebufferWriter;

use crate::driver_context_impl::KernelDriverContext;

/// Complete VirtIO-GPU initialisation: probe → queue → display → renderer.
///
/// Returns the GPU handle and the framebuffer renderer on success,
/// or `None` if any step fails (caller falls back to GOP/VGA).
pub fn init() -> Option<(Box<VirtioGpu>, UefiFramebufferWriter)> {
    let ctx = KernelDriverContext;
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;

    // 1. Hardware-level init (PCI probe, BAR mapping, queues, display)
    let result = nitrogen::virtio::gpu::init::init(&ctx)?;

    // 2. Framebuffer info
    let fb_config = {
        let opt = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|m| m.lock().clone());
        opt.unwrap_or(petroleum::common::FullereneFramebufferConfig {
            address: 0x40000000,
            width: 1024,
            height: 768,
            stride: 1024,
            pixel_format:
                petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            bpp: 32,
        })
    };
    let fb_phys = fb_config.address;
    let fb_virt = fb_phys + off;
    let fb_byte_size = (fb_config.stride * fb_config.height * (fb_config.bpp / 8)) as u64;
    let fb_pages = ((fb_byte_size + 4095) / 4096) as usize;

    // 3. Map framebuffer WC via DriverContext
    let fb_flags = nitrogen::PageFlags::FRAMEBUFFER_WC;
    for i in 0..fb_pages {
        ctx.map_page(
            (fb_virt + (i * 4096) as u64) as usize,
            (fb_phys + (i * 4096) as u64) as usize,
            fb_flags,
        )
        .ok()
        .or_else(|| {
            log::error!("virtio_gpu: failed to map fb page {}/{}", i, fb_pages);
            None
        })?;
    }

    // 4. Create renderer
    let fb_info = petroleum::graphics::color::FramebufferInfo {
        address: fb_virt,
        width: fb_config.width,
        height: fb_config.height,
        stride: fb_config.stride,
        pixel_format: Some(fb_config.pixel_format),
        colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
    };
    let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
    let renderer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);

    log::info!(
        "virtio-gpu: display {}x{} ready",
        fb_config.width,
        fb_config.height
    );
    Some((result.gpu, renderer))
}