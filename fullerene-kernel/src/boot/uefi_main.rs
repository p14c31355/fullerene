//! UEFI main stage 2 (only compiled for uefi target)
#![cfg(target_os = "uefi")]

use crate::MEMORY_MAP;
use crate::interrupts;
use petroleum::write_serial_bytes;
use x86_64::VirtAddr;

use crate::boot::uefi_init::UefiInitContext;

// Re-export debug_serial helper from uefi_init (reduces repetitive port I/O in debug logging)
use super::uefi_init::debug_serial;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn efi_main_stage2(
    args_ptr: *const petroleum::assembly::KernelArgs,
    physical_memory_offset: VirtAddr,
) -> ! {
    unsafe {
        core::arch::asm!(
            "mov dx, 0x3f8",
            "mov al, 0x44",
            "out dx, al", // Signal 'D'
            options(nomem, preserves_flags)
        );
        debug_serial(b"S2: Entering efi_main_stage2\n");

        // Store args_ptr where it survives register clobbers.
        petroleum::transition::KERNEL_ARGS = args_ptr;

        // ── Early framebuffer parameter capture ───────────────────
        // Store raw integers NOW while args_ptr is valid.  Do NOT
        // dereference args_ptr later — it may be corrupted by the
        // world‑switch page‑table rebuild.
        // NOTE: KernelContext::init_kernel() does PCI scan (heap alloc)
        // so we init framebuffer only at this early stage.
        crate::contexts::framebuffer::init_framebuffer();
        {
            let args = &*args_ptr;
            // Use fb_stride if the bootloader provided it (new field).
            // On real hardware pixels_per_scan_line > horizontal_resolution is
            // common (e.g. 2560→2688 on Intel GOP), so the bootloader's stride
            // from GOP is authoritative.  Fall back to width*4 for old bootloaders
            // that don't set fb_stride.
            let stride = if args.fb_stride > 0 {
                args.fb_stride
            } else {
                args.fb_width.saturating_mul(4)
            };
            let pixel_format = match args.fb_pixel_format {
                0 => {
                    petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
                }
                1 => {
                    petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor
                }
                _ => {
                    petroleum::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor
                }
            };
            // Store KernelArgs virtual address (already higher-half) in .data.
            // init_and_jump identity-maps kernel_args_page, and shallow
            // clone_page_table preserves it.  init_graphics can dereference
            // this pointer directly even after page-table rebuilds.
            crate::graphics::store_args_va(args_ptr as u64);
            crate::contexts::framebuffer::with_framebuffer_mut(|fb| {
                fb.store_raw_params(
                    args.fb_address,
                    args.fb_width,
                    args.fb_height,
                    stride,
                    args.fb_bpp,
                    pixel_format,
                );
            });
        }

        // Signal '3': After early FB param capture
        core::arch::asm!(
            "mov dx, 0x3f8",
            "mov al, 0x33",
            "out dx, al",
            options(nomem, preserves_flags)
        );
        debug_serial(b"S2: Signals 1-3 sent\n");
    }

    // CRITICAL: Set physical memory offset BEFORE initializing the global memory manager
    // to avoid page faults when creating the OffsetPageTable in PageTableManager::init.
    petroleum::set_physical_memory_offset(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE);
    debug_serial(b"DEBUG: Physical memory offset set before memory manager init\n");

    // Initialize the global memory manager with the EFI memory map
    debug_serial(b"DEBUG: Initializing global memory manager...\n");
    debug_serial(b"Calling MEMORY_MAP.get()\n");
    if let Some(memory_map) = *MEMORY_MAP.lock() {
        debug_serial(b"DEBUG: MEMORY_MAP acquired, calling init_memory_manager\n");

        if let Err(_e) = crate::memory_management::init_memory_manager(memory_map) {
            debug_serial(b"ERROR: init_memory_manager failed!\n");
            petroleum::halt_loop();
        }
        petroleum::set_memory_initialized(true);
        debug_serial(b"Memory management initialized successfully\n");
    } else {
        debug_serial(b"ERROR: MEMORY_MAP not initialized. Halting.\n");
        petroleum::halt_loop();
    }

    // ============ MMIO mapping BEFORE any graphics/device access ============
    // Map APIC, IOAPIC, VGA text buffer, and GOP framebuffer NOW so that
    // init_common → init_graphics can safely access the framebuffer.
    // This must happen AFTER memory manager init (which sets up the frame allocator)
    // but BEFORE any code that touches MMIO regions.
    debug_serial(b"DEBUG: [uefi_main] Mapping MMIO regions before init_common\n");
    // Initialize LOCAL_APIC_ADDRESS and validate FB config (no 4KB mappings).
    crate::boot::uefi_init::UefiInitContext::map_mmio();
    debug_serial(b"DEBUG: [uefi_main] APIC addr set, mapping GOP FB pages\n");

    // On real hardware (InsydeH2O), the GOP framebuffer is often at a high
    // physical address (> 0x80000000) that falls outside the boot-time identity
    // huge-page mapping.  We must explicitly create page-table entries for the
    // framebuffer BEFORE anything tries to write to it.
    //
    // QEMU/OVMF places FB at low addresses (< 4 GiB) that happen to be covered
    // by transition.rs's huge-page map, so this was never noticeable in QEMU.
    {
        let args = unsafe { &*args_ptr };
        if args.fb_address >= 0x100000 && args.fb_width > 0 && args.fb_height > 0 && args.fb_bpp == 32 {
            let stride = if args.fb_stride > 0 { args.fb_stride } else { args.fb_width * 4 };
            let fb_byte_size = stride as u64 * args.fb_height as u64;
            let fb_pages = ((fb_byte_size + 4095) / 4096) as usize;
            let off = petroleum::common::memory::get_physical_memory_offset() as u64;
            let fb_virt = args.fb_address + off;

            debug_serial(b"DEBUG: [uefi_main] mapping GOP FB phys=0x");
            let mut buf = [0u8; 32];
            let len = petroleum::serial::format_hex_to_buffer(args.fb_address, &mut buf, 16);
            debug_serial(&buf[..len]);
            debug_serial(b" pages=");
            let len2 = petroleum::serial::format_hex_to_buffer(fb_pages as u64, &mut buf, 16);
            debug_serial(&buf[..len2]);
            debug_serial(b"\n");

            // Use Uncached (UC) for MMIO framebuffer — real hardware MTRRs
            // typically mark the PCI BAR range as UC, and using WB or WC
            // may conflict → #GP triple fault (README Fix #2, #3).
            let flags = x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE
                | x86_64::structures::paging::PageTableFlags::NO_CACHE;

            let mut mm = crate::memory_management::get_memory_manager().lock();
            if let Some(ref mut mgr) = *mm {
                for i in 0..fb_pages {
                    let v = (fb_virt + (i * 4096) as u64) as usize;
                    let p = (args.fb_address + (i * 4096) as u64) as usize;
                    if mgr.safe_map_page(v, p, flags).is_err() {
                        debug_serial(b"ERROR: [uefi_main] FB page map failed\n");
                        break;
                    }
                }
            }
            drop(mm);
        }
    }
    debug_serial(b"DEBUG: [uefi_main] MMIO init complete\n");

    // CRITICAL: On InsydeH2O firmware, VirtIO-GPU init_display() can trigger
    // MSI/MSI-X interrupts as soon as SET_SCANOUT completes. If the APIC LVTs
    // are unmasked and no handler is registered, the CPU receives a spurious
    // interrupt that may escalate to a triple fault.
    //
    // We pre-initialise the APIC hardware (mask all LVTs, disable legacy PIC,
    // enable APIC) BEFORE init_common so that any DMA/MSI interrupt from the
    // GPU is safely suppressed during graphics initialisation.  The full APIC
    // setup (IO APIC routing, syscall handlers) is done later in
    // kernel_main_higher_half as before.
    debug_serial(b"DEBUG: [uefi_main] Pre-initialising APIC (mask LVTs) before init_common\n");
    crate::interrupts::apic::init_apic_hw_only();
    debug_serial(b"DEBUG: [uefi_main] APIC hw-only init complete\n");

    // NOTE: vga_puts (identity address 0xB8000) removed — after CR3 switch
    // identity VGA access can cause QEMU iothread lock re-entrancy.
    // Framebuffer diagnostic writes also removed — huge-page WB mapping
    // conflicts with InsydeH2O MTRR=UC on PCI MMIO → #GP triple fault.
    // init_graphics() creates proper WC/UC mappings later.
    // Use only debug_serial for post-world-switch logging.

    // Common initialization for both UEFI and BIOS with correct physical memory offset
    debug_serial(b"DEBUG: [uefi_main] About to call init_common\n");
    petroleum::serial::serial_log(format_args!("About to call init_common\n"));
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        debug_serial(b"DEBUG: [uefi_main] RSP before init_common\n");
        // Use raw serial print to avoid potential deadlock in bootloader_log/println
        let mut buf = [0u8; 32];
        let len = petroleum::serial::format_hex_to_buffer(rsp, &mut buf, 16);
        debug_serial(b"RSP: 0x");
        debug_serial(&buf[..len]);
        debug_serial(b"\n");
    }
    debug_serial(b"DEBUG: [uefi_main] Calling init_common now\n");
    crate::init::init_common(physical_memory_offset);
    debug_serial(b"DEBUG: [uefi_main] init_common returned\n");
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        debug_serial(b"DEBUG: [uefi_main] Got RSP, about to call init_log\n");

        petroleum::init_log!("RSP after init_common: 0x{:x}", rsp);
        debug_serial(b"DEBUG: [uefi_main] init_log returned\n");
    }
    debug_serial(b"DEBUG: [uefi_main] About to call log::info\n");
    log::info!("init_common completed");
    debug_serial(b"DEBUG: [uefi_main] log::info returned\n");

    debug_serial(b"About to complete basic init\n");
    debug_serial(b"DEBUG: [uefi_main] About to call serial_log\n");
    petroleum::serial::serial_log(format_args!("About to log basic init complete...\n"));
    debug_serial(b"DEBUG: [uefi_main] serial_log returned\n");

    debug_serial(b"DEBUG: [uefi_main] About to call log::info (basic init complete)\n");
    log::info!("Kernel: basic init complete");
    debug_serial(b"DEBUG: [uefi_main] log::info returned\n");

    debug_serial(b"Basic init complete logged\n");
    debug_serial(b"DEBUG: [uefi_main] About to call serial_log (success)\n");
    petroleum::serial::serial_log(format_args!("basic init complete logged successfully\n"));
    debug_serial(b"DEBUG: [uefi_main] serial_log returned\n");

    // Transition to the formal kernel main in the higher half
    kernel_main_higher_half(args_ptr, physical_memory_offset);
}

fn kernel_main_higher_half(
    _args_ptr: *const petroleum::assembly::KernelArgs,
    _physical_memory_offset: VirtAddr,
) -> ! {
    debug_serial(b"Entering kernel_main_higher_half...\n");

    // NOTE: MMIO mapping (APIC, IOAPIC, VGA, framebuffer) was already done
    // in efi_main_stage2 BEFORE init_common, so init_graphics can safely
    // access the framebuffer. No need to call map_mmio again here.

    // 1. Initialize APIC (IDT, exceptions, syscalls already set up in init_common)
    crate::interrupts::apic::init_apic();
    log::info!("APIC initialized");

    // 3. Enable interrupts and enter scheduler loop
    log::info!("Enabling interrupts and starting scheduler...");
    debug_serial(b"Entering scheduler_loop\n");
    x86_64::instructions::interrupts::enable();
    crate::scheduler::scheduler_loop();
}
