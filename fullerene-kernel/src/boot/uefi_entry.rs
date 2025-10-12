// use super::macros::kernel_log;
use super::utils::calculate_framebuffer_size;
use super::constants::FALLBACK_HEAP_START_ADDR;
use crate::{VGA_BUFFER_ADDRESS, VGA_COLOR_GREEN_ON_BLACK, hlt_loop, MEMORY_MAP};
use alloc::boxed::Box;
use core::ffi::c_void;
use petroleum::common::{
    EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
    VgaFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;
use x86_64::{PhysAddr, VirtAddr};
use petroleum::graphics::init_vga_text_mode;
use petroleum::{debug_log, serial, write_serial_bytes};
use crate::graphics::framebuffer::{FramebufferLike, UefiFramebuffer};
use crate::memory::{
    find_framebuffer_config, find_heap_start, init_memory_management, setup_memory_maps,
};
use crate::{gdt, graphics, heap, interrupts, keyboard, process, shell, syscall};
use petroleum::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputModeInformation, EfiGraphicsOutputProtocol,
    EfiGraphicsOutputProtocolMode,
};

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

    debug_log!("Early VGA write done");

    // Debug parameter values
    debug_log!(
        "Parameters: system_table={:x} memory_map={:x} memory_map_size={:x}",
        system_table as usize,
        memory_map as usize,
        memory_map_size
    );

    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: starting to parse parameters.\n");

    // Verify our own address as sanity check for PE relocation
    let self_addr = efi_main as u64;
    debug_log!("Kernel: efi_main located at {:x}", self_addr as usize);

    // Cast system_table to reference
    let system_table = unsafe { &*system_table };

    init_vga_text_mode();

    debug_log!("VGA setup done");
    kernel_log!("VGA text mode setup function returned");

    // Early VGA text output to ensure visible output on screen
    kernel_log!("About to write to VGA buffer at 0xb8000");
    {
        let vga_buffer = unsafe { &mut *(VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]) };
        // Clear screen first
        for row in 0..25 {
            for col in 0..80 {
                vga_buffer[row][col] = VGA_COLOR_GREEN_ON_BLACK | b' ' as u16;
            }
        }
        // Write modified hello message
        let hello = b"UEFI Kernel: Display Test!";
        for (i, &byte) in hello.iter().enumerate() {
            vga_buffer[0][i] = VGA_COLOR_GREEN_ON_BLACK | (byte as u16);
        }
        let hello2 = b"This should be visible.";
        for (i, &byte) in hello2.iter().enumerate() {
            vga_buffer[1][i] = VGA_COLOR_GREEN_ON_BLACK | (byte as u16);
        }
    }
    kernel_log!("VGA buffer write completed");

    // Setup memory maps and initialize memory management
    let kernel_virt_addr = efi_main as u64;
    let (higher_half_offset, kernel_phys_start) =
        setup_memory_maps(memory_map, memory_map_size, kernel_virt_addr);
    let physical_memory_offset = VirtAddr::new(0); // UEFI identity maps initially, offset handled by higher-half in reinit_page_table

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

    // Initialize graphics with framebuffer config to get framebuffer info
    kernel_log!("Initializing graphics temporarily to get framebuffer size...");
    let (fb_addr, fb_size) = if let Some(gop_config) = find_gop_framebuffer(system_table) {
        calculate_framebuffer_size(&gop_config, "GOP")
    } else if let Some(fb_config) = find_framebuffer_config(system_table) {
        calculate_framebuffer_size(&fb_config, "custom")
    } else {
        (None, None)
    };

    // Reinit page tables to kernel page tables
    kernel_log!("Reinit page tables to kernel page tables with framebuffer size");
    heap::reinit_page_table(physical_memory_offset, kernel_phys_start, fb_addr, fb_size);
    kernel_log!("Page table reinit completed successfully");

    // Set physical memory offset for process management
    crate::memory_management::set_physical_memory_offset(physical_memory_offset);

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(*MEMORY_MAP.get().unwrap());
    kernel_log!("Kernel: heap_phys_start=0x{:x}", heap_phys_start.as_u64());
    let start_addr = if heap_phys_start.as_u64() < 0x1000 {
        kernel_log!(
            "Kernel: ERROR - Invalid heap_phys_start, using fallback 0x{:x}",
            FALLBACK_HEAP_START_ADDR
        );
        PhysAddr::new(FALLBACK_HEAP_START_ADDR)
    } else {
        heap_phys_start
    };

    let heap_start = heap::allocate_heap_from_map(start_addr, heap::HEAP_SIZE);
    kernel_log!("Kernel: heap_start=0x{:x}", heap_start.as_u64());
    let heap_start_after_gdt = gdt::init(heap_start);
    kernel_log!(
        "Kernel: heap_start_after_gdt=0x{:x}",
        heap_start_after_gdt.as_u64()
    );
    kernel_log!("Kernel: GDT init done");

    // Initialize heap with the remaining memory
    let gdt_mem_usage = heap_start_after_gdt - heap_start;
    let heap_size_remaining = heap::HEAP_SIZE - gdt_mem_usage as usize;
    heap::init(heap_start_after_gdt, heap_size_remaining);

    if heap_phys_start.as_u64() < 0x1000 {
        kernel_log!("Kernel: heap initialized with fallback");
    } else {
        kernel_log!("Kernel: gdt_mem_usage=0x{:x}", gdt_mem_usage);
        kernel_log!("Kernel: heap initialized");
    }

    // Early serial log works now
    kernel_log!("Kernel: basic init complete");

    // Common initialization for both UEFI and BIOS
    // Initialize IDT before enabling interrupts
    interrupts::init();
    kernel_log!("Kernel: IDT init done");

    kernel_log!("Kernel: Jumping straight to graphics testing");

    // CRITICAL: Disable interrupts during graphics initialization to avoid process switching issues
    x86_64::instructions::interrupts::disable();
    kernel_log!("Interrupts disabled for graphics initialization");

    // Initialize graphics with framebuffer config
    let framebuffer_initialized = initialize_graphics_with_config(system_table);

    if !framebuffer_initialized {
        kernel_log!("No UEFI framebuffer available, falling back to VGA mode");
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

    kernel_log!("Initializing graphics and shell...");

    // Initialize graphics with framebuffer configuration
    if initialize_graphics_with_config(system_table) {
        kernel_log!("Graphics initialized successfully");

        // Initialize keyboard input driver
        crate::keyboard::init();
        kernel_log!("Keyboard initialized");

        // Initialize syscall handling
        crate::syscall::init();
        kernel_log!("Syscalls initialized");

        // Initialize process management
        crate::process::init();
        kernel_log!("Process management initialized");

        // Start the shell as the main interface
        kernel_log!("Starting shell...");
        crate::shell::shell_main();
        // shell_main should never return in normal operation

        kernel_log!("Shell exited unexpectedly, entering idle loop");
    } else {
        kernel_log!("Graphics initialization failed, entering idle loop");
    }

    hlt_loop();
}

pub fn find_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    use core::ptr;
    use petroleum::common::{EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiBootServices};

    petroleum::serial::serial_log(format_args!(
        "find_gop_framebuffer: Looking for GOP protocol\n"
    ));

    if system_table.boot_services.is_null() {
        petroleum::serial::serial_log(format_args!(
            "find_gop_framebuffer: Boot services is null\n"
        ));
        return None;
    }

    let boot_services = unsafe { &*system_table.boot_services };

    // Use locate_protocol to find GOP (simpler than locate_handle)
    let mut gop_handle: *mut EfiGraphicsOutputProtocol = ptr::null_mut();
    let status = (boot_services.locate_protocol)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
        core::ptr::null_mut(),
        core::ptr::addr_of_mut!(gop_handle) as *mut *mut c_void,
    );

    if status != 0 {
        petroleum::serial::serial_log(format_args!(
            "find_gop_framebuffer: locate_protocol failed with status 0x{:x}\n",
            status
        ));
        return None;
    }

    if gop_handle.is_null() {
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: GOP handle is null\n"));
        return None;
    }

    let gop = unsafe { &*gop_handle };
    if gop.mode.is_null() {
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: GOP mode is null\n"));
        return None;
    }

    let gop_mode = unsafe { &*gop.mode };
    let address = gop_mode.frame_buffer_base;

    if address == 0 {
        petroleum::serial::serial_log(format_args!(
            "find_gop_framebuffer: Framebuffer base is 0\n"
        ));
        return None;
    }

    // Get current mode info - we already have the mode info from gop_mode.info
    if !gop_mode.info.is_null() {
        let mode_info = unsafe { &*gop_mode.info };

        petroleum::serial::serial_log(format_args!(
            "find_gop_framebuffer: Found GOP framebuffer {}x{} @ 0x{:x}, stride: {}, format: {:?}\n",
            mode_info.horizontal_resolution,
            mode_info.vertical_resolution,
            address,
            mode_info.pixels_per_scan_line,
            mode_info.pixel_format
        ));

        Some(FullereneFramebufferConfig {
            address,
            width: mode_info.horizontal_resolution,
            height: mode_info.vertical_resolution,
            pixel_format: mode_info.pixel_format,
            bpp: petroleum::common::get_bpp_from_pixel_format(mode_info.pixel_format),
            stride: mode_info.pixels_per_scan_line,
        })
    } else {
        petroleum::serial::serial_log(format_args!(
            "find_gop_framebuffer: GOP mode info is null\n"
        ));
        None
    }
}

/// Helper function to try initializing graphics with a framebuffer config.
/// Returns true if graphics were successfully initialized and drawn.
/// source_name is used for logging purposes (e.g., "UEFI custom" or "GOP").
#[cfg(target_os = "uefi")]
pub fn try_init_graphics(config: &FullereneFramebufferConfig, source_name: &str) -> bool {
    kernel_log!("Initializing {} graphics mode...", source_name);
    graphics::text::init(config);

    // Verify the framebuffer was initialized
    if let Some(fb_writer) = unsafe { graphics::text::FRAMEBUFFER_UEFI.get() } {
        let fb_info = fb_writer.lock();
        kernel_log!(
            "{} framebuffer initialized successfully - width: {}, height: {}",
            source_name,
            fb_info.get_width(),
            fb_info.get_height()
        );

        // Test direct pixel write to verify access
        kernel_log!("Testing {} framebuffer access...", source_name);
        unsafe { fb_writer.lock().put_pixel(100, 100, 0xFF0000) };
        kernel_log!("Direct {} pixel write test completed", source_name);
    } else {
        kernel_log!("ERROR: {} framebuffer initialization failed!", source_name);
        return false;
    }

    kernel_log!(
        "{} graphics mode initialized, calling draw_os_desktop...",
        source_name
    );
    graphics::draw_os_desktop();
    kernel_log!(
        "{} graphics desktop drawn - if you see this, draw_os_desktop completed",
        source_name
    );
    petroleum::serial::serial_log(format_args!("Desktop should be visible now!\n"));
    true
}

/// Helper function to initialize graphics with framebuffer configuration
/// Returns true if graphics were successfully initialized and drawn
#[cfg(target_os = "uefi")]
pub fn initialize_graphics_with_config(system_table: &EfiSystemTable) -> bool {
    // Check if framebuffer config is available from UEFI bootloader
    kernel_log!("Checking framebuffer config from UEFI bootloader...");
    if let Some(fb_config) = find_framebuffer_config(system_table) {
        kernel_log!(
            "Found framebuffer config: {}x{} @ {:#x}, stride: {}, pixel_format: {:?}",
            fb_config.width,
            fb_config.height,
            fb_config.address,
            fb_config.stride,
            fb_config.pixel_format
        );
        return try_init_graphics(&fb_config, "UEFI custom");
    }

    kernel_log!("No custom framebuffer config found, trying standard UEFI GOP...");

    // Try to find GOP (Graphics Output Protocol) from UEFI
    if let Some(gop_config) = find_gop_framebuffer(system_table) {
        kernel_log!(
            "Found GOP framebuffer config: {}x{} @ {:#x}, stride: {}, pixel_format: {:?}",
            gop_config.width,
            gop_config.height,
            gop_config.address,
            gop_config.stride,
            gop_config.pixel_format
        );
        return try_init_graphics(&gop_config, "UEFI GOP");
    }

    false
}
