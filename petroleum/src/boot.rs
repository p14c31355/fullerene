//! Boot-related utilities for UEFI initialization.

use crate::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE;
use crate::page_table::constants::get_frame_allocator_mut;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

fn trace_fmt(args: core::fmt::Arguments) {
    crate::serial::_print(format_args!("[TRACE:boot] {}", args));
}
macro_rules! trace {
    ($($arg:tt)*) => { trace_fmt(format_args!($($arg)*)); };
}

/// Walk the 4-level page table by hand (best-effort, lock-free).
unsafe fn debug_page_walk(vaddr: VirtAddr, phys_offset: VirtAddr) {
    let pt_flags = x86_64::structures::paging::PageTableFlags::PRESENT;
    let l4 = crate::page_table::active_level_4_table(phys_offset);
    let l4_idx = vaddr.p4_index();
    let l3_idx = vaddr.p3_index();
    let l2_idx = vaddr.p2_index();
    let l1_idx = vaddr.p1_index();
    // Use references to avoid moving non-Copy PageTableEntry
    let l4e = &(*l4)[l4_idx];
    trace!("  L4[{:?}]: addr=0x{:x} flags={:?}\n", l4_idx, l4e.addr().as_u64(), l4e.flags());
    if !l4e.flags().contains(pt_flags) { trace!("  → L4 !PRESENT\n"); return; }
    let l3_phys = l4e.addr().as_u64() + phys_offset.as_u64();
    let l3_ptr = l3_phys as *const x86_64::structures::paging::PageTable;
    let l3 = &*l3_ptr;
    let l3e = &(*l3)[l3_idx];
    trace!("  L3[{:?}]: addr=0x{:x} flags={:?}\n", l3_idx, l3e.addr().as_u64(), l3e.flags());
    if !l3e.flags().contains(pt_flags) { trace!("  → L3 !PRESENT\n"); return; }
    if l3e.flags().contains(x86_64::structures::paging::PageTableFlags::HUGE_PAGE) {
        trace!("  → 1GiB page\n"); return;
    }
    let l2_phys = l3e.addr().as_u64() + phys_offset.as_u64();
    let l2_ptr = l2_phys as *const x86_64::structures::paging::PageTable;
    let l2 = &*l2_ptr;
    let l2e = &(*l2)[l2_idx];
    trace!("  L2[{:?}]: addr=0x{:x} flags={:?}\n", l2_idx, l2e.addr().as_u64(), l2e.flags());
    if !l2e.flags().contains(pt_flags) { trace!("  → L2 !PRESENT\n"); return; }
    if l2e.flags().contains(x86_64::structures::paging::PageTableFlags::HUGE_PAGE) {
        trace!("  → 2MiB page\n"); return;
    }
    let l1_phys = l2e.addr().as_u64() + phys_offset.as_u64();
    let l1_ptr = l1_phys as *const x86_64::structures::paging::PageTable;
    let l1 = &*l1_ptr;
    let l1e = &(*l1)[l1_idx];
    trace!("  L1[{:?}]: addr=0x{:x} flags={:?}\n", l1_idx, l1e.addr().as_u64(), l1e.flags());
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
                trace!("fb_address=0x{:x}, fb_width={}, fb_height={}, fb_bpp={}\n",
                    args.fb_address, args.fb_width, args.fb_height, args.fb_bpp);
                const MAX_REASONABLE_WIDTH: u32 = 16384;
                const MAX_REASONABLE_HEIGHT: u32 = 16384;
                let fb_valid = args.fb_address >= 0x100000
                    && args.fb_width > 0 && args.fb_width <= MAX_REASONABLE_WIDTH
                    && args.fb_height > 0 && args.fb_height <= MAX_REASONABLE_HEIGHT
                    && (args.fb_bpp == 8 || args.fb_bpp == 16 || args.fb_bpp == 24 || args.fb_bpp == 32);
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

    // 1 & 2: Try GOP / Framebuffer
    trace!("Attempting to get fb_config\n");
    let config = crate::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    }).or_else(|| {
        trace!("FULLERENE_FRAMEBUFFER_CONFIG empty, trying fallback detection\n");
        let fb = crate::kernel_fallback_framebuffer_detection();
        if fb.is_some() { trace!("fallback detection succeeded\n"); }
        fb
    });

    if let Some(fb_config) = config {
        let fb_phys = fb_config.address;
        let fb_width = fb_config.width;
        let fb_height = fb_config.height;
        let fb_bpp = fb_config.bpp;
        let fb_stride = fb_config.stride;
        let fb_size = (fb_width as u64 * fb_height as u64 * fb_bpp as u64) / 8;
        let fb_pages = ((fb_size + 4095) / 4096) as usize;
        let fb_virt = fb_phys + PHYSICAL_MEMORY_OFFSET_BASE as u64;
        trace!("fb_config: phys=0x{:x}, virt=0x{:x}, {}x{} bpp={} stride={}\n",
            fb_phys, fb_virt, fb_width, fb_height, fb_bpp, fb_stride);
        trace!("fb_size={} bytes, fb_pages={}\n", fb_size, fb_pages);

        let frame_allocator = get_frame_allocator_mut();
        let phys_offset = x86_64::VirtAddr::new(PHYSICAL_MEMORY_OFFSET_BASE as u64);
        trace!("getting L4 table\n");
        let l4 = unsafe { crate::page_table::active_level_4_table(phys_offset) };

        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE;

        trace!("mapping {} framebuffer pages\n", fb_pages);
        unsafe {
            for i in 0..fb_pages {
                let v = x86_64::VirtAddr::new(fb_virt + i as u64 * 4096);
                let p = x86_64::PhysAddr::new(fb_phys + i as u64 * 4096);
                match crate::page_table::kernel::init::map_page_4k_l1(
                    l4, v, p, flags, frame_allocator, phys_offset,
                ) {
                    Ok(_) => {},
                    Err(e) => {
                        trace!("mapping FB page {} FAILED at virt=0x{:x}: {:?}\n", i, v.as_u64(), e);
                    }
                }
            }
        }
        trace!("FB pages mapped, flushing TLB\n");
        let cr3_val = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(cr3_val.0, cr3_val.1); }

        // Post-map verification: walk page table for first and last page
        trace!("Verifying page table walk for FB virt=0x{:x}\n", fb_virt);
        unsafe { debug_page_walk(x86_64::VirtAddr::new(fb_virt), phys_offset); }
        let last_fb_virt = fb_virt + ((fb_pages.saturating_sub(1)) as u64 * 4096);
        unsafe { debug_page_walk(x86_64::VirtAddr::new(last_fb_virt), phys_offset); }

        let info = crate::graphics::color::FramebufferInfo {
            address: fb_virt,
            width: fb_width,
            height: fb_height,
            stride: fb_stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: crate::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };
        trace!("FramebufferInfo created: addr=0x{:x}, stride={}, format={:?}\n",
            info.address, info.stride, info.pixel_format);

        let writer = crate::graphics::framebuffer::FramebufferWriter::<u32>::new(info);
        trace!("create_primary_console returning Some(Uefi32)\n");
        Some(crate::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer))
    } else {
        trace!("create_primary_console returning None (no fb config)\n");
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