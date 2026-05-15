//! UEFI main stage 2 (only compiled for uefi target)
#![cfg(target_os = "uefi")]

use crate::MEMORY_MAP;
use crate::interrupts;
use petroleum::write_serial_bytes;
use x86_64::VirtAddr;

use crate::boot::uefi_init::UefiInitContext;

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
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"S2: Entering efi_main_stage2\n");

        petroleum::transition::KERNEL_ARGS = args_ptr;

        // Signal '3': After setting KERNEL_ARGS
        core::arch::asm!(
            "mov dx, 0x3f8",
            "mov al, 0x33",
            "out dx, al",
            options(nomem, preserves_flags)
        );
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"S2: Signals 1-3 sent\n");
    }

    // CRITICAL: Set physical memory offset BEFORE initializing the global memory manager
    // to avoid page faults when creating the OffsetPageTable in PageTableManager::init.
    petroleum::set_physical_memory_offset(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE);
    write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: Physical memory offset set before memory manager init\n"
    );

    // Initialize the global memory manager with the EFI memory map
    write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: Initializing global memory manager...\n"
    );
    write_serial_bytes!(0x3F8, 0x3FD, b"Calling MEMORY_MAP.get()\n");
    if let Some(memory_map) = *MEMORY_MAP.lock() {
        write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: MEMORY_MAP acquired, calling init_memory_manager\n"
        );

        if let Err(_e) = crate::memory_management::init_memory_manager(memory_map) {
            write_serial_bytes!(0x3F8, 0x3FD, b"ERROR: init_memory_manager failed!\n");
            petroleum::halt_loop();
        }
        petroleum::set_memory_initialized(true);
        write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"Memory management initialized successfully\n"
        );
    } else {
        write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"ERROR: MEMORY_MAP not initialized. Halting.\n"
        );
        petroleum::halt_loop();
    }

    // ============ MMIO mapping BEFORE any graphics/device access ============
    // Map APIC, IOAPIC, VGA text buffer, and GOP framebuffer NOW so that
    // init_common → init_graphics can safely access the framebuffer.
    // This must happen AFTER memory manager init (which sets up the frame allocator)
    // but BEFORE any code that touches MMIO regions.
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] Mapping MMIO regions before init_common\n"
    );
    let _vga_virt_addr = crate::boot::uefi_init::UefiInitContext::map_mmio();
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] MMIO mapping completed before init_common\n"
    );

    // Common initialization for both UEFI and BIOS with correct physical memory offset
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] About to call init_common\n"
    );
    petroleum::serial::serial_log(format_args!("About to call init_common\n"));
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [uefi_main] RSP before init_common\n"
        );
        // Use raw serial print to avoid potential deadlock in bootloader_log/println
        let mut buf = [0u8; 32];
        let len = petroleum::serial::format_hex_to_buffer(rsp, &mut buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"RSP: 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] Calling init_common now\n"
    );
    crate::init::init_common(physical_memory_offset);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] init_common returned\n");
    unsafe {
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [uefi_main] Got RSP, about to call init_log\n"
        );

        petroleum::init_log!("RSP after init_common: 0x{:x}", rsp);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] init_log returned\n");
    }
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] About to call log::info\n"
    );
    log::info!("init_common completed");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] log::info returned\n");

    write_serial_bytes!(0x3F8, 0x3FD, b"About to complete basic init\n");
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] About to call serial_log\n"
    );
    petroleum::serial::serial_log(format_args!("About to log basic init complete...\n"));
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] serial_log returned\n");

    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] About to call log::info (basic init complete)\n"
    );
    log::info!("Kernel: basic init complete");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] log::info returned\n");

    write_serial_bytes!(0x3F8, 0x3FD, b"Basic init complete logged\n");
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_main] About to call serial_log (success)\n"
    );
    petroleum::serial::serial_log(format_args!("basic init complete logged successfully\n"));
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_main] serial_log returned\n");

    // Transition to the formal kernel main in the higher half
    kernel_main_higher_half(args_ptr, physical_memory_offset);
}

fn kernel_main_higher_half(
    _args_ptr: *const petroleum::assembly::KernelArgs,
    _physical_memory_offset: VirtAddr,
) -> ! {
    write_serial_bytes!(0x3F8, 0x3FD, b"Entering kernel_main_higher_half...\n");

    // NOTE: MMIO mapping (APIC, IOAPIC, VGA, framebuffer) was already done
    // in efi_main_stage2 BEFORE init_common, so init_graphics can safely
    // access the framebuffer. No need to call map_mmio again here.

    // 1. Initialize APIC (IDT, exceptions, syscalls already set up in init_common)
    crate::interrupts::apic::init_apic();
    log::info!("APIC initialized");

    // 3. Initialize keyboard input driver
    crate::keyboard::init();
    log::info!("Keyboard initialized");

    // 4. Enable interrupts and enter scheduler loop
    log::info!("Enabling interrupts and starting scheduler...");
    write_serial_bytes!(0x3F8, 0x3FD, b"Entering scheduler_loop\n");
    x86_64::instructions::interrupts::enable();
    crate::scheduler::scheduler_loop();
}
