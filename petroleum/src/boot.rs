//! Boot-related utilities for UEFI initialization.

use crate::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE;
use crate::page_table::constants::get_frame_allocator_mut;
use x86_64::structures::paging::PageTableFlags;

/// Creates the primary UEFI framebuffer console if available, returns None if fallback to VGA is needed.
pub fn create_primary_console() -> Option<crate::graphics::framebuffer::UefiFramebufferWriter> {
    // 0: Initialize framebuffer config from KernelArgs if not already set
    if crate::FULLERENE_FRAMEBUFFER_CONFIG.get().is_none() {
        // SAFETY: KERNEL_ARGS is set by efi_main_stage2 before init_common
        // is called, and points to valid memory allocated by the bootloader.
        unsafe {
            let args_ptr = crate::transition::KERNEL_ARGS;
            if !args_ptr.is_null() {
                let args = &*args_ptr;
                // Validate KernelArgs framebuffer values
                const MAX_REASONABLE_WIDTH: u32 = 16384;
                const MAX_REASONABLE_HEIGHT: u32 = 16384;
                let fb_valid = args.fb_address >= 0x100000
                    && args.fb_width > 0 && args.fb_width <= MAX_REASONABLE_WIDTH
                    && args.fb_height > 0 && args.fb_height <= MAX_REASONABLE_HEIGHT
                    && (args.fb_bpp == 8 || args.fb_bpp == 16 || args.fb_bpp == 24 || args.fb_bpp == 32);
                if fb_valid {
                    // Use checked arithmetic to avoid overflow from garbage values
                    let stride = (args.fb_width as u64)
                        .checked_mul(args.fb_bpp as u64 / 8)
                        .and_then(|s| u32::try_from(s).ok())
                        .unwrap_or(0);
                    let config = crate::create_framebuffer_config(
                        args.fb_address,
                        args.fb_width,
                        args.fb_height,
                        crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                        args.fb_bpp,
                        stride,
                    );
                    if stride > 0 {
                        crate::FULLERENE_FRAMEBUFFER_CONFIG
                            .call_once(|| spin::Mutex::new(Some(config)));
                    }
                }
            }
        }
    }

    // 1 & 2: Try GOP / Framebuffer
    let config = crate::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    }).or_else(|| crate::kernel_fallback_framebuffer_detection());

    if let Some(fb_config) = config {
        // Map framebuffer region into page table
        let fb_phys = fb_config.address;
        let fb_size = (fb_config.width as u64 * fb_config.height as u64 * fb_config.bpp as u64) / 8;
        let fb_pages = ((fb_size + 4095) / 4096) as usize;
        let fb_virt = fb_phys + PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let frame_allocator = get_frame_allocator_mut();
        let phys_offset = x86_64::VirtAddr::new(PHYSICAL_MEMORY_OFFSET_BASE as u64);
        let l4 = unsafe { crate::page_table::active_level_4_table(phys_offset) };

        // Use NO_CACHE for framebuffer
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE;

        unsafe {
            for i in 0..fb_pages {
                let v = x86_64::VirtAddr::new(fb_virt + i as u64 * 4096);
                let p = x86_64::PhysAddr::new(fb_phys + i as u64 * 4096);
                crate::page_table::kernel::init::map_page_4k_l1(
                    l4, v, p, flags, frame_allocator, phys_offset,
                ).expect("Failed to map framebuffer page");
            }
        }
        // Flush TLB
        let cr3_val = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(cr3_val.0, cr3_val.1); }

        let info = crate::graphics::color::FramebufferInfo {
            address: fb_virt,
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: crate::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };

        let writer = crate::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
        Some(crate::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer))
    } else {
        None
    }
}

/// Initializes VGA text mode fallback and returns a VgaBuffer.
pub fn initialize_vga_fallback() -> crate::graphics::text::VgaBuffer {
    // Initialize VGA text buffer
    let mut vga = crate::graphics::text::VgaBuffer::with_address(
        crate::page_table::constants::VGA_MEMORY_START as usize
    );
    vga.enable();
    crate::Console::clear(&mut vga);
    vga
}