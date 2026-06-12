//! Graphics subsystem — thin bridge to [`crate::contexts::FramebufferContext`].
//! All state lives in [`FramebufferContext`]; this module provides the public API
//! for existing callers (`gui.rs`, `scheduler.rs`, `badapple.rs`).
use crate::contexts::framebuffer::{
    FramebufferContext, get_framebuffer, with_framebuffer, with_framebuffer_mut,
};
use core::sync::atomic::{AtomicBool, Ordering};

static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init_graphics() {
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    // Path 1: VirtIO-GPU
    if let Some((gpu, renderer)) = crate::virtio_gpu::init() {
        with_framebuffer_mut(|fb| {
            fb.renderer = Some(renderer);
            fb.gpu = Some(gpu);
            fb.bpp = 32;
        });
        return;
    }
    // Path 2: GOP framebuffer
    if let Some(fb_config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
        .get()
        .and_then(|m| m.lock().clone())
    {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let fb_virt = fb_config.address + off;
        if fb_config.bpp == 8 {
            petroleum::graphics::setup::setup_vga_mode_13h();
            petroleum::graphics::setup::setup_vga_text_mode();
        } else {
            let info = petroleum::graphics::color::FramebufferInfo {
                address: fb_virt,
                width: fb_config.width,
                height: fb_config.height,
                stride: fb_config.stride,
                pixel_format: Some(fb_config.pixel_format),
                colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
            };
            let w = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
            let r = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(w);
            with_framebuffer_mut(|fb| {
                fb.renderer = Some(r);
                fb.bpp = fb_config.bpp;
            });
            return;
        }
    }
    // Path 3: VGA text mode fallback
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let vga_phys = petroleum::page_table::constants::VGA_MEMORY_START;
    let vga_virt = vga_phys + off;
    let vga_flags = x86_64::structures::paging::PageTableFlags::NO_CACHE
        | x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().unwrap();
        let _ = mm.safe_map_page(vga_virt as usize, vga_phys as usize, vga_flags);
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
