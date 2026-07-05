//! UEFI main stage 2 (only compiled for uefi target)
#![cfg(target_os = "uefi")]

use crate::MEMORY_MAP;
use x86_64::VirtAddr;

// Re-export debug_serial helper from uefi_init (reduces repetitive port I/O in debug logging)
use super::uefi_init::debug_serial;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn efi_main_stage2(
    args_ptr: *const petroleum::assembly::KernelArgs,
    physical_memory_offset: VirtAddr,
) -> ! {
    unsafe {
        // ════════════════════════════════════════════════════════════
        // STAGE -1: Ultra-early serial + VGA text diagnostic
        //
        // We do NOT write to speculative GOP FB addresses here because
        // on real hardware the identity-mapped physical addresses
        // 0xE0000000 etc. may hit non-framebuffer MMIO regions (PCIe
        // config space, SPI flash, PCH registers) and can cause
        // unpredictable system behavior or machine checks.
        //
        // Instead we only write to the VGA text buffer at 0xB8000
        // (always safe on x86 UEFI — the memory exists even if unused)
        // and to the stage marker at physical 0x700.
        let off_ = petroleum::common::memory::get_physical_memory_offset() as u64;
        let vga_text = (0xB8000u64 + off_) as *mut u16;
        for i in 0..80*25 {
            core::ptr::write_volatile(vga_text.add(i), 0x0420u16); // red-on-black space
        }

        // ── Ultra-early diagnostic: write stage markers to physical 0x700 ──
        // These survive triple-fault / reset and can be read via UEFI shell
        // (`dmem 0x700 10`) or a mini-bootloader stub.  Values written:
        //   0x700: magic "FS" (0x4653) — confirms efi_main_stage2 is reached
        //   0x702: incremented on each major step
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let diag = (0x700u64 + off) as *mut u16;
        core::ptr::write_volatile(diag, 0x4653u16);               // "FS" magic
        core::ptr::write_volatile(diag.add(1), 0x0001u16);        // step = 1
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
                // Update step marker
                core::ptr::write_volatile(diag.add(1), 0x0004u16);

                // ── PCI BAR fallback ────────────────────────────────
                // If args_ptr was garbage, the identity huge-page mapping
                // is broken.  Probe PCI config space via port I/O (0xCF8/
                // 0xCFC) which needs zero memory-mapped registers, then
                // store the discovered FB params directly.
                //
                // NOTE: On Intel GPUs (vendor 0x8086), BAR0 (offset 0x10)
                // is the MMIO register space, NOT the framebuffer.  The
                // framebuffer lives behind BAR2 (offset 0x18, GTT aperture).
                // Other vendors (AMD, NVIDIA) typically put the framebuffer
                // at BAR0.  We try BAR2 for Intel, BAR0 for others.
                let mut fb_stored = false;
                // Scan bus 0 only (VGA-class display is always on bus 0)
                for dev in 0..=31u16 {
                    let addr: u32 = 0x8000_0000u32 | ((dev as u32) << 11) | 0x00;
                    x86_64::instructions::port::PortWriteOnly::new(0xCF8).write(addr);
                    let vendor = x86_64::instructions::port::PortReadOnly::<u32>::new(0xCFC).read();
                    if (vendor & 0xFFFF) as u16 == 0xFFFF || (vendor & 0xFFFF) as u16 == 0x0000 {
                        continue;
                    }
                    let class_addr: u32 = 0x8000_0000u32 | ((dev as u32) << 11) | 0x08;
                    x86_64::instructions::port::PortWriteOnly::new(0xCF8).write(class_addr);
                    let class_rev =
                        x86_64::instructions::port::PortReadOnly::<u32>::new(0xCFC).read();
                    let class = ((class_rev >> 24) & 0xFF) as u8;
                    let subclass = ((class_rev >> 16) & 0xFF) as u8;
                    if class == 0x03 && subclass == 0x00 {
                        let vendor_id = (vendor & 0xFFFF) as u16;
                        // Intel puts framebuffer at BAR2, most others at BAR0
                        let fb_bar_offset = if vendor_id == 0x8086 { 0x18u32 } else { 0x10u32 };
                        let upper_bar_offset = if vendor_id == 0x8086 { 0x1Cu32 } else { 0x14u32 };
                        let bar_addr: u32 = 0x8000_0000u32 | ((dev as u32) << 11) | fb_bar_offset;
                        x86_64::instructions::port::PortWriteOnly::new(0xCF8).write(bar_addr);
                        let bar_val =
                            x86_64::instructions::port::PortReadOnly::<u32>::new(0xCFC).read();
                        let fb_phys = if (bar_val & 0x6) == 0x4 {
                            let bar1_addr: u32 = 0x8000_0000u32 | ((dev as u32) << 11) | upper_bar_offset;
                            x86_64::instructions::port::PortWriteOnly::new(0xCF8).write(bar1_addr);
                            let bar1 =
                                x86_64::instructions::port::PortReadOnly::<u32>::new(0xCFC).read();
                            ((bar1 as u64) << 32) | ((bar_val & 0xFFFF_FFF0) as u64)
                        } else {
                            (bar_val & 0xFFFF_FFF0) as u64
                        };
                        if fb_phys >= 0x100_000 && fb_phys <= 0x10_0000_0000 {
                            let stride = 1280u32 * 4;
                            crate::graphics::discovery::store_boot_fb_params(
                                fb_phys, 1280, 800, stride, 32, 1,
                            );
                            fb_stored = true;
                            debug_serial(b"S2: PCI fallback FB stored\n");
                            core::ptr::write_volatile(diag.add(1), 0x0005u16);
                            break;
                        }
                    }
                }
                if !fb_stored {
                    debug_serial(b"S2: PCI fallback: no VGA controller found\n");
                    core::ptr::write_volatile(diag.add(1), 0x0006u16);
                }
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
