//! Boot module containing UEFI and BIOS entry points and boot-specific logic

use petroleum::graphics::init_vga_text_mode;
use petroleum::serial::{SERIAL_PORT_WRITER as SERIAL1, debug_print_str_to_com1 as debug_print_str, debug_print_hex};
use petroleum::write_serial_bytes;

use alloc::boxed::Box;

use core::ffi::c_void;
use petroleum::common::{
    EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
    VgaFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;

use crate::memory::{find_framebuffer_config, find_heap_start, init_memory_management, setup_memory_maps};
use crate::{gdt, graphics, heap, interrupts, MEMORY_MAP};

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

#[cfg(target_os = "uefi")]
#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut c_void,
    memory_map_size: usize,
) -> ! {
    // Early debug print to confirm kernel entry point is reached using direct port access
    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: efi_main entered.\n");

    // Initialize serial early for debug logging
    petroleum::serial::serial_init();

    debug_print_str("Early VGA write done\n");

    // Debug parameter values
    debug_print_str("Parameters: system_table=");
    debug_print_hex(system_table as usize);
    debug_print_str(" memory_map=");
    debug_print_hex(memory_map as usize);
    debug_print_str(" memory_map_size=");
    debug_print_hex(memory_map_size);
    debug_print_str("\n");

    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: starting to parse parameters.\n");

    // Verify our own address as sanity check for PE relocation
    let self_addr = efi_main as u64;
    debug_print_str("Kernel: efi_main located at ");
    debug_print_hex(self_addr as usize);
    debug_print_str("\n");

    // Cast system_table to reference
    let system_table = unsafe { &*system_table };

    init_vga_text_mode();

    debug_print_str("VGA setup done\n");
    kernel_log!("VGA text mode setup function returned");

    // Early VGA text output to ensure visible output on screen
    kernel_log!("About to write to VGA buffer at 0xb8000");
    {
        let vga_buffer = unsafe { &mut *(super::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]) };
        // Clear screen first
        for row in 0..25 {
            for col in 0..80 {
                vga_buffer[row][col] = super::VGA_COLOR_GREEN_ON_BLACK | b' ' as u16;
            }
        }
        // Write hello message
        let hello = b"Hello from UEFI Kernel!";
        for (i, &byte) in hello.iter().enumerate() {
            vga_buffer[0][i] = super::VGA_COLOR_GREEN_ON_BLACK | (byte as u16);
        }
    }
    kernel_log!("VGA buffer write completed");

    // Setup memory maps and initialize memory management
    let kernel_virt_addr = efi_main as u64;
    let (physical_memory_offset, kernel_phys_start) = setup_memory_maps(memory_map, memory_map_size, kernel_virt_addr);

    // Initialize memory management components (heap, page tables, etc.)
    // Comment out reinit for now to allow desktop drawing
    kernel_log!("Starting heap frame allocator init...");

    kernel_log!(
        "Calling heap::init_frame_allocator with {} descriptors",
        MEMORY_MAP.get().unwrap().len()
    );
    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    kernel_log!("Heap frame allocator init completed successfully");

    kernel_log!(
        "Calling heap::init_page_table with offset 0x{:x}",
        physical_memory_offset.as_u64()
    );
    heap::init_page_table(physical_memory_offset);
    kernel_log!("Page table init completed successfully");

    // Skip reinit for now
    // kernel_log!(
    //     "Calling heap::reinit_page_table with offset 0x{:x} and kernel_phys_start 0x{:x}",
    //     physical_memory_offset.as_u64(),
    //     kernel_phys_start.as_u64()
    // );
    // heap::reinit_page_table(physical_memory_offset, kernel_phys_start);
    // kernel_log!("Page table reinit completed successfully");

    // Set physical memory offset for process management
    crate::memory_management::set_physical_memory_offset(physical_memory_offset);

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(*MEMORY_MAP.get().unwrap());
    let heap_start = heap::allocate_heap_from_map(heap_phys_start, heap::HEAP_SIZE);
    let heap_start_after_gdt = gdt::init(heap_start);
    kernel_log!("Kernel: GDT init done");

    // Initialize heap with the remaining memory
    let gdt_mem_usage = heap_start_after_gdt - heap_start;
    heap::init(
        heap_start_after_gdt,
        heap::HEAP_SIZE - gdt_mem_usage as usize,
    );
    kernel_log!("Kernel: heap initialized");

    // Early serial log works now
    kernel_log!("Kernel: basic init complete");

    // Common initialization for both UEFI and BIOS
    // Initialize IDT before enabling interrupts
    interrupts::init();
    kernel_log!("Kernel: IDT init done");

    // Common initialization (enables interrupts)
    super::init::init_common();
    kernel_log!("Kernel: init_common done");

    kernel_log!("Kernel: efi_main entered");
    kernel_log!("GDT initialized");
    kernel_log!("IDT initialized");
    kernel_log!("APIC initialized");
    kernel_log!("Heap initialized");
    kernel_log!("Serial initialized");

    // Check if framebuffer config is available from UEFI bootloader
    kernel_log!("Checking framebuffer config from UEFI bootloader...");
    if let Some(fb_config) = find_framebuffer_config(system_table) {
        kernel_log!(
            "Found framebuffer config: {}x{} @ {:#x}",
            fb_config.width,
            fb_config.height,
            fb_config.address
        );
        kernel_log!("Initializing UEFI graphics mode...");
        graphics::init(fb_config);
        kernel_log!("UEFI graphics mode initialized, calling draw_os_desktop...");
        graphics::draw_os_desktop();
        kernel_log!("UEFI graphics desktop drawn - if you see this, draw_os_desktop completed");
        petroleum::serial::serial_log(format_args!("Desktop should be visible now!\n"));
    } else {
        kernel_log!("No framebuffer config found, falling back to VGA mode");
        let vga_config = VgaFramebufferConfig {
            address: 0xA0000,
            width: 320,
            height: 200,
            bpp: 8,
        };
        kernel_log!("Initializing VGA graphics mode...");
        graphics::init_vga(&vga_config);
        kernel_log!("VGA graphics mode initialized, calling draw_os_desktop...");
        graphics::draw_os_desktop();
        kernel_log!("VGA graphics desktop drawn - if you see this, draw_os_desktop completed");
    }

    kernel_log!("Kernel: running in main loop");
    kernel_log!("FullereneOS kernel is now running");
    super::hlt_loop();
}

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use petroleum::common::VgaFramebufferConfig;

    super::init::init_common();
    kernel_log!("Entering _start (BIOS mode)...");

    // Graphics initialization for VGA framebuffer (graphics mode)
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    kernel_log!("VGA graphics mode initialized (BIOS mode).");

    // Main loop
    crate::graphics::_print(format_args!("Hello QEMU by FullereneOS\n"));

    // Keep kernel running instead of exiting
    kernel_log!("BIOS boot complete, kernel running...");
    super::hlt_loop();
}
