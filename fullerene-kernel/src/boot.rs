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
use x86_64::PhysAddr;
use petroleum::page_table::EfiMemoryDescriptor;

use crate::memory::{find_framebuffer_config, find_heap_start, init_memory_management, setup_memory_maps};
use crate::{gdt, graphics, heap, interrupts, shell, MEMORY_MAP};
use crate::graphics::framebuffer::{FramebufferLike, UefiFramebuffer};

use petroleum::common::{
    EfiGraphicsOutputProtocol, EfiGraphicsOutputProtocolMode, EfiGraphicsOutputModeInformation,
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
};

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

    // Reinit page tables to kernel page tables
    // Comment out reinit to avoid page fault during boot
    // kernel_log!("Reinit page tables to kernel page tables");
    // heap::reinit_page_table(physical_memory_offset, kernel_phys_start, None);
    // kernel_log!("Page table reinit completed successfully");

    // Set physical memory offset for process management
    crate::memory_management::set_physical_memory_offset(physical_memory_offset);

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(*MEMORY_MAP.get().unwrap());
    kernel_log!("Kernel: heap_phys_start=0x{:x}", heap_phys_start.as_u64());
    const FALLBACK_HEAP_START_ADDR: u64 = 0x100000;
        let start_addr = if heap_phys_start.as_u64() < 0x1000 {
        kernel_log!("Kernel: ERROR - Invalid heap_phys_start, using fallback 0x{:x}", FALLBACK_HEAP_START_ADDR);
        PhysAddr::new(FALLBACK_HEAP_START_ADDR)
    } else {
        heap_phys_start
    };

    let heap_start = heap::allocate_heap_from_map(start_addr, heap::HEAP_SIZE);
    kernel_log!("Kernel: heap_start=0x{:x}", heap_start.as_u64());
    let heap_start_after_gdt = gdt::init(heap_start);
    kernel_log!("Kernel: heap_start_after_gdt=0x{:x}", heap_start_after_gdt.as_u64());
    kernel_log!("Kernel: GDT init done");

    // Initialize heap with the remaining memory
    // Comment out heap init to avoid page fault during boot
    // let gdt_mem_usage = heap_start_after_gdt - heap_start;
    // let heap_size_remaining = heap::HEAP_SIZE - gdt_mem_usage as usize;
    // heap::init(heap_start_after_gdt, heap_size_remaining);
    //
    // if heap_phys_start.as_u64() < 0x1000 {
    //     kernel_log!("Kernel: heap initialized with fallback");
    // } else {
    //     kernel_log!("Kernel: gdt_mem_usage=0x{:x}", gdt_mem_usage);
    //     kernel_log!("Kernel: heap initialized");
    // }

    // Early serial log works now
    kernel_log!("Kernel: basic init complete");

    // Common initialization for both UEFI and BIOS
    // Initialize IDT before enabling interrupts
    interrupts::init();
    kernel_log!("Kernel: IDT init done");

    kernel_log!("Kernel: Jumping straight to graphics testing");

    // Skip graphics initialization to avoid crashes
    kernel_log!("Kernel: initialization complete, entering idle loop");
    super::hlt_loop();
}

fn find_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    use petroleum::common::{EfiBootServices, EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID};
    use core::ptr;

    petroleum::serial::serial_log(format_args!("find_gop_framebuffer: Looking for GOP protocol\n"));

    if system_table.boot_services.is_null() {
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: Boot services is null\n"));
        return None;
    }

    let boot_services = unsafe { &*system_table.boot_services };

    // Use locate_protocol to find GOP (simpler than locate_handle)
    let mut gop_handle: *mut EfiGraphicsOutputProtocol = ptr::null_mut();
    let status = (boot_services.locate_protocol)(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID as *const _ as *const u8,
        core::ptr::null_mut(),
        core::ptr::addr_of_mut!(gop_handle) as *mut *mut c_void,
    );

    if status != 0 {
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: locate_protocol failed with status 0x{:x}\n", status));
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
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: Framebuffer base is 0\n"));
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
        petroleum::serial::serial_log(format_args!("find_gop_framebuffer: GOP mode info is null\n"));
        None
    }
}

/// Helper function to try initializing graphics with a framebuffer config.
/// Returns true if graphics were successfully initialized and drawn.
/// source_name is used for logging purposes (e.g., "UEFI custom" or "GOP").
#[cfg(target_os = "uefi")]
fn try_init_graphics(config: &FullereneFramebufferConfig, source_name: &str) -> bool {
    kernel_log!("Initializing {} graphics mode...", source_name);
    graphics::text::init(config);

    // Verify the framebuffer was initialized
    if let Some(fb_writer) = unsafe { graphics::text::FRAMEBUFFER_UEFI.get() } {
        let fb_info = fb_writer.lock();
        kernel_log!("{} framebuffer initialized successfully - width: {}, height: {}", source_name, fb_info.get_width(), fb_info.get_height());

        // Test direct pixel write to verify access
        kernel_log!("Testing {} framebuffer access...", source_name);
        unsafe { fb_writer.lock().put_pixel(100, 100, 0xFF0000) };
        kernel_log!("Direct {} pixel write test completed", source_name);
    } else {
        kernel_log!("ERROR: {} framebuffer initialization failed!", source_name);
        return false;
    }

    kernel_log!("{} graphics mode initialized, calling draw_os_desktop...", source_name);
    graphics::draw_os_desktop();
    kernel_log!("{} graphics desktop drawn - if you see this, draw_os_desktop completed", source_name);
    petroleum::serial::serial_log(format_args!("Desktop should be visible now!\n"));
    true
}

/// Helper function to initialize graphics with framebuffer configuration
/// Returns true if graphics were successfully initialized and drawn
#[cfg(target_os = "uefi")]
fn initialize_graphics_with_config(system_table: &EfiSystemTable) -> bool {
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
