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

    // 2. Initialize xHCI Debug Capability (USB debug output)
    init_xhci_dbc();

    // 3. Flush kernel log to VFS before entering scheduler
    log::info!("Flushing boot log...");
    debug_serial(b"Flushing boot log to VFS\n");
    if let Err(()) = crate::klog::flush_to_vfs() {
        debug_serial(b"WARNING: flush_to_vfs failed (VFS not ready?)\n");
    }

    // 4. Enable interrupts and enter scheduler loop
    log::info!("Enabling interrupts and starting scheduler...");
    debug_serial(b"Entering scheduler_loop\n");
    x86_64::instructions::interrupts::enable();
    crate::scheduler::scheduler_loop();
}

/// Scan PCI for an xHCI controller and enable the Debug Capability
/// so serial output is mirrored over USB.
fn init_xhci_dbc() {
    let offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    nitrogen::xhci_dbc::set_physical_offset(offset);

    let kernel_lock = crate::contexts::kernel::get_kernel();
    let kg = kernel_lock.lock();
    let k = match kg.as_ref() {
        Some(k) => k,
        None => {
            debug_serial(b"XHCI: kernel context not available, skipping\n");
            return;
        }
    };

    let xhc_dev = match k.pci.find_xhci() {
        Some(d) => d.clone(),
        None => {
            debug_serial(b"XHCI: no xHCI controller found in PCI scan\n");
            return;
        }
    };

    // Enable memory access on the xHC before reading BAR
    xhc_dev.enable_memory_access();

    let bar0 = match xhc_dev.read_bar(0) {
        Some(a) => a,
        None => {
            debug_serial(b"XHCI: BAR0 not available\n");
            return;
        }
    };

    // Map BAR0 MMIO (64 KiB is enough for xHC registers + extended caps)
    let bar0_virt = match crate::memory_management::convenience::map_mmio(bar0 as usize, 0x10000)
    {
        Ok(v) => v,
        Err(_) => {
            debug_serial(b"XHCI: failed to map BAR0\n");
            return;
        }
    };

    // Read HCCPARAMS1 (offset 0x10 from BAR0) to find extended capabilities
    let hccparams1 = unsafe { core::ptr::read_volatile((bar0_virt + 0x10) as *const u32) };
    let dbc_offset = nitrogen::xhci_dbc::find_dbc_capability(bar0_virt, hccparams1);

    if dbc_offset == 0 {
        debug_serial(b"XHCI: DbC capability not found\n");
        return;
    }

    unsafe {
        nitrogen::xhci_dbc::init(bar0_virt, dbc_offset);
    }

    if nitrogen::xhci_dbc::is_ready() {
        debug_serial(b"XHCI: DbC initialized -- USB debug active\n");
        log::info!("xHCI Debug Capability initialized");
    } else {
        debug_serial(b"XHCI: DbC init returned false (cable not connected?)\n");
    }
}
