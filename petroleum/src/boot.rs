//! Boot-related utilities for UEFI initialization.
//!
//! ⚠️ **DEPRECATED**: Use `petroleum::early::framebuffer` instead.
//! This module will be removed in a future version.
//! Boot code should import from `petroleum::early::framebuffer::*`.

use crate::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE;
use crate::page_table::constants::get_frame_allocator_mut;
use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags;

fn trace_fmt(args: core::fmt::Arguments) {
    crate::serial::_print(format_args!("[TRACE:boot] {}", args));
}
macro_rules! trace {
    ($($arg:tt)*) => { trace_fmt(format_args!($($arg)*)); };
}

/// Creates the primary UEFI framebuffer console if available, returns None if fallback to VGA is needed.
pub fn create_primary_console() -> Option<crate::graphics::framebuffer::UefiFramebufferWriter> {
    trace!("create_primary_console start\n");
    // 0: Initialize framebuffer config from KernelArgs if not already set
    if crate::FULLERENE_FRAMEBUFFER_CONFIG.get().is_none() {
        trace!("FULLERENE_FRAMEBUFFER_CONFIG not set, checking KERNEL_ARGS\n");
        unsafe {
            let args_ptr = crate::transition::KERNEL_ARGS;
            trace!("KERNEL_ARGS ptr = {:p}\n", args_ptr);
            if !args_ptr.is_null() {
                let args = &*args_ptr;
                trace!(
                    "fb_address=0x{:x}, fb_width={}, fb_height={}, fb_bpp={}\n",
                    args.fb_address, args.fb_width, args.fb_height, args.fb_bpp
                );
                const MAX_REASONABLE_WIDTH: u32 = 16384;
                const MAX_REASONABLE_HEIGHT: u32 = 16384;
                let fb_valid = args.fb_address >= 0x100000
                    && args.fb_width > 0
                    && args.fb_width <= MAX_REASONABLE_WIDTH
                    && args.fb_height > 0
                    && args.fb_height <= MAX_REASONABLE_HEIGHT
                    && (args.fb_bpp == 8
                        || args.fb_bpp == 16
                        || args.fb_bpp == 24
                        || args.fb_bpp == 32);
                trace!("fb_valid = {}\n", fb_valid);
                if fb_valid {
                    let bpp = args.fb_bpp as u64;
                    let stride_raw = (args.fb_width as u64).checked_mul(bpp / 8).unwrap_or(0);
                    let stride = u32::try_from(stride_raw).ok().unwrap_or(0);
                    trace!("computed stride = {}\n", stride);
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
                        trace!("FULLERENE_FRAMEBUFFER_CONFIG set from KERNEL_ARGS\n");
                    } else {
                        trace!("stride == 0, skipping config\n");
                    }
                }
            } else {
                trace!("KERNEL_ARGS is null\n");
            }
        }
    } else {
        trace!("FULLERENE_FRAMEBUFFER_CONFIG already set\n");
    }

    // Validate config from FULLERENE_FRAMEBUFFER_CONFIG before using it.
    // The config may contain garbage if the bootloader's Once/Mutex was corrupted
    // during the world switch or page table re-initialization.
    trace!("Attempting to get fb_config\n");
    let raw_config = crate::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    });

    let config = if let Some(cfg) = raw_config {
        let fb_valid = cfg.address >= 0x100000
            && cfg.address <= 0x0000_FFFF_FFFF_FFFF
            && cfg.width > 0
            && cfg.width <= 16384
            && cfg.height > 0
            && cfg.height <= 16384
            && (cfg.bpp == 8 || cfg.bpp == 16 || cfg.bpp == 24 || cfg.bpp == 32);
        if fb_valid {
            trace!(
                "FULLERENE_FRAMEBUFFER_CONFIG validated OK: {:#x} {}x{} bpp={}\n",
                cfg.address, cfg.width, cfg.height, cfg.bpp
            );
            Some(cfg)
        } else {
            trace!("FULLERENE_FRAMEBUFFER_CONFIG validation FAILED, trying fallback\n");
            None
        }
    } else {
        trace!("FULLERENE_FRAMEBUFFER_CONFIG empty, trying fallback detection\n");
        None
    };

    // Skip QEMU framebuffer fallback — hardcoded QEMU addresses (0xFC000000)
    // cause triple faults on real InsydeH2O hardware.
    // config = config.or_else(|| crate::kernel_fallback_framebuffer_detection());
    let config = config.or_else(|| None);

    if let Some(fb_config) = config {
        let fb_phys = fb_config.address;
        let fb_width = fb_config.width;
        let fb_height = fb_config.height;
        let fb_bpp = fb_config.bpp;
        let fb_stride = fb_config.stride;

        // Use stride (not width) to calculate the real framebuffer byte size.
        // On real hardware pixels_per_scan_line > horizontal_resolution is
        // common (e.g. 2560→2688), and only mapping width×height pages would
        // leave trailing scan-line padding unmapped → page fault → triple fault.
        let fb_byte_size = (fb_stride as u64) * (fb_height as u64);
        let fb_pages = ((fb_byte_size + 4095) / 4096) as usize;
        let fb_virt = fb_phys + PHYSICAL_MEMORY_OFFSET_BASE as u64;
        trace!(
            "fb_config: phys=0x{:x}, virt=0x{:x}, {}x{} bpp={} stride={}\n",
            fb_phys, fb_virt, fb_width, fb_height, fb_bpp, fb_stride
        );
        trace!(
            "fb_byte_size={} bytes, fb_pages={}\n",
            fb_byte_size, fb_pages
        );

        // Debugging: Verify stride matches expected bytes-per-line
        let expected_stride = (fb_width as u64 * (fb_bpp as u64 / 8)) as u32;
        if fb_stride != expected_stride {
            trace!(
                "WARNING: fb_stride ({}) != expected_stride ({}) — using stride for page mapping\n",
                fb_stride, expected_stride
            );
        }

        let phys_offset = x86_64::VirtAddr::new(PHYSICAL_MEMORY_OFFSET_BASE as u64);

        // UC (Uncacheable): PCD=1, PWT=1 → PAT entry 3 → true UC.
        // WC MTRR + UC page table → effective = UC. Writes are immediate.
        // NO_CACHE + WRITE_THROUGH together give both PCD and PWT.
        // At this point UMM has initialized full physical memory mappings,
        // so frame allocations for page tables are safe.
        let fb_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::WRITE_THROUGH;
        unsafe {
            let frame_allocator = get_frame_allocator_mut();
            let l4 = crate::page_table::active_level_4_table(phys_offset);
            for i in 0..fb_pages {
                let v = x86_64::VirtAddr::new(fb_virt + i as u64 * 4096);
                let p = x86_64::PhysAddr::new(fb_phys + i as u64 * 4096);
                let _ = crate::early::mapper::map_page_4k_l1(
                    l4,
                    v,
                    p,
                    fb_flags,
                    frame_allocator,
                    phys_offset,
                );
            }
        }
        x86_64::instructions::tlb::flush_all();

        let info = crate::graphics::color::FramebufferInfo {
            address: fb_virt,
            width: fb_width,
            height: fb_height,
            stride: fb_stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: crate::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };
        trace!(
            "FramebufferInfo created: addr=0x{:x}, stride={}, format={:?}\n",
            info.address, info.stride, info.pixel_format
        );

        let writer = crate::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
        trace!("create_primary_console returning Some(Uefi32)\n");
        Some(crate::graphics::framebuffer::UefiFramebufferWriter::Uefi32(
            writer,
        ))
    } else {
        trace!("create_primary_console returning None (no fb config)\n");
        None
    }
}

/// Initializes VGA text mode fallback and returns a VgaBuffer.
pub fn initialize_vga_fallback() -> crate::graphics::text::VgaBuffer {
    // Initialize VGA text buffer
    let mut vga = crate::graphics::text::VgaBuffer::with_address(
        crate::page_table::constants::VGA_MEMORY_START as usize,
    );
    vga.enable();
    crate::Console::clear(&mut vga);
    vga
}
