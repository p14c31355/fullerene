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
        //
        // VALIDATION: Before dereferencing args_ptr, verify the
        // framebuffer fields are sane.  If args_ptr lies outside the
        // identity huge-page range (0–64GB), the shallow clone_page_table
        // will translate it to wrong physical memory and we'll read
        // garbage.  Check that fb_width/height/stride/bpp/address are
        // within plausible bounds.  If they aren't, skip the .data store
        // and rely on the PCI BAR0 fallback later.
        crate::graphics::discovery::store_args_va(args_ptr as u64);
        {
            let args = &*args_ptr;
            // Sanity-check: are the framebuffer fields plausible?
            // On real InsydeH2O firmware, garbage reads produce values like
            // 1900544×4172873728 — these are clearly out of range.
            let fb_valid = args.fb_address >= 0x100000
                && args.fb_width > 0
                && args.fb_width <= 16384
                && args.fb_height > 0
                && args.fb_height <= 16384
                && args.fb_bpp == 32;
            if fb_valid {
                let stride_bytes = if args.fb_stride > 0 {
                    args.fb_stride
                } else {
                    args.fb_width.saturating_mul(4)
                };
                crate::graphics::discovery::store_boot_fb_params(
                    args.fb_address,
                    args.fb_width,
                    args.fb_height,
                    stride_bytes,
                    args.fb_bpp,
                    args.fb_pixel_format,
                );
            } else {
                // Framebuffer fields are garbage — args_ptr was not
                // identity-mapped correctly.  Do NOT store the corrupted
                // values; the kernel will fall back to PCI BAR0 probe.
                debug_serial(b"S2: WARNING: KernelArgs FB fields invalid, skipping .data store (identity map mismatch?)\n");
            }
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

    // On InsydeH2O firmware, explicitly creating 4 KB UC/WC page-table
    // entries for the GOP framebuffer here **breaks** the boot-loader's
    // identity huge-page mapping (WB via MTRR/PAT).  See README § "Real
    // Hardware Compatibility" item 3.
    //
    // The boot‑time huge‑page mapping already covers the entire lower
    // 4 GiB address space.  Even if the GOP FB is above 4 GiB, the
    // kernel's init_graphics() creates a proper WC mapping via
    // build_renderer_from_stored() later.  We rely on that instead of
    // pre‑splitting the huge page here.
    debug_serial(b"DEBUG: [uefi_main] skipping 4KB GOP FB remap (preserving huge page)\n");

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

    // 2. Flush kernel log to VFS before entering scheduler
    log::info!("Flushing boot log...");
    debug_serial(b"Flushing boot log to VFS\n");
    if let Err(()) = crate::klog::flush_to_vfs() {
        debug_serial(b"WARNING: flush_to_vfs failed (VFS not ready?)\n");
    }

    // 3. Enable interrupts and enter scheduler loop
    log::info!("Enabling interrupts and starting scheduler...");
    debug_serial(b"Entering scheduler_loop\n");
    x86_64::instructions::interrupts::enable();
    crate::scheduler::scheduler_loop();
}