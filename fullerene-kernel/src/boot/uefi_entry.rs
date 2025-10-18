// Use crate imports
use crate::scheduler::scheduler_loop;

use crate::MEMORY_MAP;
use crate::graphics::framebuffer::FramebufferLike;
use crate::heap;

use crate::memory::find_heap_start;
use crate::{gdt, graphics, interrupts, memory};
use alloc::boxed::Box;
use core::ffi::c_void;
use petroleum::common::EfiGraphicsOutputProtocol;
use petroleum::common::uefi::{efi_print, find_gop_framebuffer, write_vga_string};
use petroleum::common::{EfiSystemTable, FullereneFramebufferConfig};
use petroleum::{allocate_heap_from_map, debug_log, write_serial_bytes};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Mapper, Size4KiB},
};

use petroleum::graphics::{
    VGA_MODE13H_ADDRESS, VGA_MODE13H_BPP, VGA_MODE13H_HEIGHT, VGA_MODE13H_STRIDE, VGA_MODE13H_WIDTH,
};

/// Helper function to map a range of memory pages
/// Takes the base physical address, number of pages, mapper, frame allocator, and flags
#[cfg(target_os = "uefi")]
fn map_memory_range(
    base_phys_addr: PhysAddr,
    num_pages: u64,
    physical_memory_offset: VirtAddr,
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::MapToError<Size4KiB>> {
    for i in 0..num_pages {
        let phys_addr_u64 = base_phys_addr.as_u64() + (i * 4096);
        let phys_addr = PhysAddr::new(phys_addr_u64);
        let virt_addr = physical_memory_offset + phys_addr_u64;

        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virt_addr);
        let frame =
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(phys_addr);

        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)?
                .flush();
        }
    }
    Ok(())
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
    log::info!("About to initialize serial port...");
    petroleum::serial::serial_init();
    log::info!("Serial port initialized successfully");

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

    // Skip VGA detection and use standard mode to avoid PCI access issues post-UEFI
    petroleum::serial::serial_log(format_args!(
        "Initializing VGA graphics in standard mode...\n"
    ));
    petroleum::graphics::setup::setup_vga_mode_13h();
    petroleum::serial::serial_log(format_args!("VGA graphics initialization completed\n"));

    //debug_log!("VGA setup done");
    log::info!("VGA text mode setup function returned");

    // Direct VGA buffer test - write to hardware buffer directly
    log::info!("Direct VGA buffer write test...");
    unsafe {
        let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
        write_vga_string(vga_buffer, 0, b"Kernel boot", 0x1F00);
    }
    log::info!("Direct VGA write test completed");

    // Early text output using EFI console to ensure visible output on screen
    log::info!("About to output to EFI console");
    efi_print(system_table, b"UEFI Kernel: Display Test!\r\n");
    efi_print(system_table, b"This is output via EFI console.\r\n");
    log::info!("EFI console output completed");

    // Setup memory maps and initialize memory management
    let kernel_virt_addr = efi_main as u64;
    let kernel_phys_start =
        crate::memory::setup_memory_maps(memory_map, memory_map_size, kernel_virt_addr);

    // Initialize memory management components (heap, page tables, etc.)
    // Comment out reinit for now to allow desktop drawing
    log::info!("Starting heap frame allocator init...");

    log::info!(
        "Calling heap::init_frame_allocator with {} descriptors",
        MEMORY_MAP.get().expect("Memory map not initialized").len()
    );
    heap::init_frame_allocator(*MEMORY_MAP.get().expect("Memory map not initialized"));
    log::info!("Heap frame allocator init completed successfully");

    // Find framebuffer configuration before reiniting page tables and save it globally
    log::info!("Finding framebuffer config for page table mapping...");
    let framebuffer_config = crate::memory::find_framebuffer_config(system_table);
    // Save the config globally for later use after exit_boot_services
    if let Some(config) = framebuffer_config {
        petroleum::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(*config)));
        log::info!("Saved framebuffer config globally for kernel use");
    }
    let config = framebuffer_config.as_ref();
    let (fb_addr, fb_size) = if let Some(config) = config {
        let fb_size_bytes =
            (config.width as usize * config.height as usize * config.bpp as usize) / 8;
        log::info!(
            "Found framebuffer config: {}x{} @ {:#x}, size: {}",
            config.width,
            config.height,
            config.address,
            fb_size_bytes
        );
        (
            Some(x86_64::VirtAddr::new(config.address)),
            Some(fb_size_bytes as u64),
        )
    } else {
        log::info!("No framebuffer config found, using None");
        (None, None)
    };

    // Reinit page tables to kernel page tables with framebuffer size using frame allocator
    log::info!("Reinit page tables to kernel page tables with framebuffer info");
    let mut frame_allocator = unsafe {
        petroleum::page_table::BootInfoFrameAllocator::init(
            *MEMORY_MAP.get().expect("Memory map not initialized"),
        )
    };
    let physical_memory_offset = heap::reinit_page_table_with_allocator(
        kernel_phys_start,
        fb_addr,
        fb_size,
        &mut frame_allocator,
    );
    log::info!("Page table reinit completed successfully");

    // Set kernel CR3 for syscall to access kernel heap
    let kernel_cr3 = x86_64::registers::control::Cr3::read();
    crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
    log::info!(
        "Kernel CR3 set for syscall: {:#x}",
        kernel_cr3.0.start_address().as_u64()
    );

    // Set physical memory offset for process management
    crate::memory_management::set_physical_memory_offset(
        crate::memory_management::PHYSICAL_MEMORY_OFFSET_BASE,
    );

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(*MEMORY_MAP.get().expect("Memory map not initialized"));
    log::info!("Kernel: heap_phys_start=0x{:x}", heap_phys_start.as_u64());
    let start_addr =
        if heap_phys_start.as_u64() < 0x1000 || heap_phys_start.as_u64() >= 0x0000800000000000 {
            log::info!(
                "Kernel: ERROR - Invalid heap_phys_start, using fallback 0x{:x}",
                petroleum::FALLBACK_HEAP_START_ADDR
            );
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            heap_phys_start
        };
    let heap_start = allocate_heap_from_map(start_addr, heap::HEAP_SIZE);
    log::info!("Kernel: heap_start=0x{:x}", heap_start.as_u64());

    // Map heap memory after page table reinit so it's accessible at virtual address
    log::info!("Mapping heap memory to virtual addresses");
    let heap_pages = (heap::HEAP_SIZE as u64).div_ceil(4096);
    let mut mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    let flags = x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    map_memory_range(heap_start, heap_pages, physical_memory_offset, &mut mapper, &mut frame_allocator, flags).expect("Failed to map heap memory");
    log::info!("Heap memory mapped successfully");

    // Calculate virtual heap address after offset mapping
    let virtual_heap_start = physical_memory_offset + heap_start.as_u64();
    log::info!("Virtual heap start: 0x{:x}", virtual_heap_start.as_u64());

    log::info!("Kernel: About to call gdt::init...");
    let gdt_heap_start = virtual_heap_start;
    let heap_start_after_gdt = gdt::init(gdt_heap_start);
    log::info!(
        "Kernel: gdt::init returned heap_start_after_gdt=0x{:x}",
        heap_start_after_gdt.as_u64()
    );
    log::info!("Kernel: GDT init done");
    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: After gdt init in uefi_entry\n");

    // Allocate a proper kernel stack for safe kernel operations
    // Even though GDT/TSS loading is skipped in UEFI, the kernel still needs a stable stack
    const KERNEL_STACK_SIZE: usize = 4096 * 16; // 64KB
    let stack_bottom = heap_start_after_gdt;
    let stack_top = stack_bottom + KERNEL_STACK_SIZE as u64;

    // Map stack memory before switching RSP
    let stack_pages = (KERNEL_STACK_SIZE as u64).div_ceil(4096);
    let stack_base_phys = PhysAddr::new(stack_bottom.as_u64() - physical_memory_offset.as_u64());
    let flags = x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    map_memory_range(stack_base_phys, stack_pages, physical_memory_offset, &mut mapper, &mut frame_allocator, flags).expect("Failed to map stack memory");
    log::info!("Stack memory mapped successfully");

    // Switch RSP to new kernel stack for safety
    unsafe {
        core::arch::asm!("mov rsp, {}", in(reg) stack_top.as_u64());
    }
    log::info!("Switched to kernel stack at 0x{:x}", stack_top.as_u64());

    // Update heap_start_after_gdt to account for the kernel stack allocation
    let heap_start_after_stack = stack_top;

    // Initialize linked_list_allocator with the remaining memory
    petroleum::serial::serial_log(format_args!("About to calculate heap memory usage...\n"));
    let kernel_overhead = (heap_start_after_stack.as_u64() - virtual_heap_start.as_u64()) as usize;
    let heap_size_remaining = heap::HEAP_SIZE - kernel_overhead;

    petroleum::serial::serial_log(format_args!("About to initialize linked_list_allocator\n"));
    debug_log!("Link alloc");

    use petroleum::page_table::ALLOCATOR;
    // Use direct serial writes to avoid UEFI console hang
    let mut serial_buf = [0u8; 128];
    let addr_str = heap_start_after_stack.as_u64();
    let addr_str_len = petroleum::serial::format_hex_to_buffer(addr_str, &mut serial_buf, 16);
    write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"About to call ALLOCATOR.lock().init() with addr=0x"
    );
    write_serial_bytes!(0x3F8, 0x3FD, &serial_buf[..addr_str_len]);
    let size_str = heap_size_remaining;
    let size_str_len = petroleum::serial::format_dec_to_buffer(size_str, &mut serial_buf);
    write_serial_bytes!(0x3F8, 0x3FD, b" size=");
    write_serial_bytes!(0x3F8, 0x3FD, &serial_buf[..size_str_len]);
    write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    unsafe {
        write_serial_bytes!(0x3F8, 0x3FD, b"Calling ALLOCATOR.lock()...\n");
        let mut allocator = ALLOCATOR.lock();
        write_serial_bytes!(0x3F8, 0x3FD, b"ALLOCATOR.lock() succeeded\n");
        let ptr_str_len = petroleum::serial::format_hex_to_buffer(
            heap_start_after_stack.as_mut_ptr::<u8>() as u64,
            &mut serial_buf,
            16,
        );
        write_serial_bytes!(0x3F8, 0x3FD, b"Before allocator.init() with ptr=0x");
        write_serial_bytes!(0x3F8, 0x3FD, &serial_buf[..ptr_str_len]);
        let size_str_len =
            petroleum::serial::format_dec_to_buffer(heap_size_remaining, &mut serial_buf);
        write_serial_bytes!(0x3F8, 0x3FD, b" size=");
        write_serial_bytes!(0x3F8, 0x3FD, &serial_buf[..size_str_len]);
        write_serial_bytes!(0x3F8, 0x3FD, b"\n");
        write_serial_bytes!(0x3F8, 0x3FD, b"Just before allocator.init() call\n");
        write_serial_bytes!(0x3F8, 0x3FD, b"Calling allocator.init() with size=");
        write_serial_bytes!(0x3F8, 0x3FD, &serial_buf[..size_str_len]);
        write_serial_bytes!(0x3F8, 0x3FD, b"\n");
        allocator.init(
            heap_start_after_stack.as_mut_ptr::<u8>(),
            heap_size_remaining,
        );
        write_serial_bytes!(0x3F8, 0x3FD, b"allocator.init() completed successfully\n");
        write_serial_bytes!(0x3F8, 0x3FD, b"Allocator initialized successfully\n");
    }

    petroleum::serial::serial_log(format_args!("About to print final allocator message...\n"));
    petroleum::serial::serial_log(format_args!(
        "Linked list allocator initialized successfully\n"
    ));
    petroleum::serial::serial_log(format_args!("About to check heap_phys_start...\n"));

    // Initialize the global memory manager with the EFI memory map
    log::info!("Initializing global memory manager...");
    write_serial_bytes!(0x3F8, 0x3FD, b"Calling MEMORY_MAP.get()\n");
    if let Some(memory_map) = MEMORY_MAP.get() {
        write_serial_bytes!(0x3F8, 0x3FD, b"MEMORY_MAP.get() succeeded\n");
        if let Err(e) = crate::memory_management::init_memory_manager(memory_map) {
            log::error!(
                "Failed to initialize global memory manager: {:?}. Halting.",
                e
            );
            petroleum::halt_loop();
        }
    } else {
        log::error!("MEMORY_MAP not initialized. Cannot initialize memory manager. Halting.");
        petroleum::halt_loop();
    }

    // Now that allocator is set up, initialize VGA buffer writer
    log::info!("Initializing VGA writer after allocator setup...");
    crate::vga::init_vga();
    log::info!("VGA writer initialized - text should be visible now");

    // Early serial log works now
    write_serial_bytes!(0x3F8, 0x3FD, b"About to complete basic init\n");
    petroleum::serial::serial_log(format_args!("About to log basic init complete...\n"));
    log::info!("Kernel: basic init complete");
    write_serial_bytes!(0x3F8, 0x3FD, b"Basic init complete logged\n");
    petroleum::serial::serial_log(format_args!("basic init complete logged successfully\n"));

    write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: About to init interrupts\n");

    // Common initialization for both UEFI and BIOS
    // Initialize IDT before enabling interrupts
    interrupts::init();
    log::info!("Kernel: IDT init done");

    log::info!("Kernel: Jumping straight to graphics testing");

    log::info!("About to call init_common");
    // Initialize interrupts and other components call init_common here
    crate::init::init_common();
    log::info!("init_common completed");

    // Initialize graphics with framebuffer configuration
    log::info!("Initialize graphics with framebuffer configuration");
    let success = initialize_graphics_with_config(system_table);
    log::info!("Graphics initialization result: {}", success);
    if !success {
        log::info!("Graphics initialization failed, continuing without graphics for debugging");
    }

    // Always enable interrupts and proceed to scheduler
    log::info!("Enabling interrupts...");
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // Initialize keyboard input driver
    crate::keyboard::init();
    log::info!("Keyboard initialized");

    // Start the main kernel scheduler that orchestrates all system functionality
    log::info!("Starting full system scheduler...");
    scheduler_loop();
    // scheduler_loop should never return in normal operation
    panic!("scheduler_loop returned unexpectedly - kernel critical failure!");
}

/// Kernel-side fallback framebuffer detection when config table is not available
/// Uses shared logic from petroleum crate
#[cfg(target_os = "uefi")]
pub fn kernel_fallback_framebuffer_detection() -> Option<FullereneFramebufferConfig> {
    log::info!(
        "Attempting kernel-side fallback framebuffer detection (bootloader config table not available)"
    );

    // Call petroleum's consolidated QEMU framebuffer detection
    petroleum::detect_qemu_framebuffer(&petroleum::QEMU_CONFIGS)
}

/// Helper function to try initializing graphics with a framebuffer config.
/// Returns true if graphics were successfully initialized and drawn.
/// source_name is used for logging purposes (e.g., "UEFI custom" or "GOP").
#[cfg(target_os = "uefi")]
pub fn try_init_graphics(config: &FullereneFramebufferConfig, source_name: &str) -> bool {
    log::info!("=== ENTERING try_init_graphics for {} ===", source_name);

    // Save current VGA buffer content before attempting graphics initialization
    let vga_backup = match create_vga_backup() {
        Some(backup) => backup,
        None => {
            log::info!("Failed to allocate VGA backup buffer");
            return false;
        }
    };

    log::info!(
        "Calling graphics::text::init with {} config...",
        source_name
    );
    graphics::text::init(config);

    log::info!("Checking if framebuffer was initialized...");

    // Verify the framebuffer was initialized
    if let Some(fb_writer) = graphics::text::FRAMEBUFFER_UEFI.get() {
        let fb_info = fb_writer.lock();
        log::info!(
            "SUCCESS: {} framebuffer initialized successfully - width: {}, height: {}, pixel_format: {:?}",
            source_name,
            fb_info.get_width(),
            fb_info.get_height(),
            config.pixel_format
        );

        // Test direct pixel write to verify access
        log::info!(
            "Testing {} framebuffer access with direct pixel write...",
            source_name
        );
        fb_writer.lock().put_pixel(100, 100, 0xFF0000);
        log::info!(
            "Direct {} pixel write test completed - red dot should be visible at 100,100",
            source_name
        );
    } else {
        log::error!(
            "CRITICAL ERROR: {} framebuffer initialization failed! text::FRAMEBUFFER_UEFI.get() returned None",
            source_name
        );
        // Restore VGA text buffer if graphics init failed
        restore_vga_text_buffer(&vga_backup);
        petroleum::graphics::init_vga_text_mode();
        crate::vga::init_vga();
        log::info!("Restored VGA text mode after graphics initialization failure");
        return false;
    }

    log::info!(
        "About to call graphics::draw_os_desktop() for {}...",
        source_name
    );
    graphics::draw_os_desktop();
    log::info!(
        "=== SUCCESS: {} graphics desktop drawn - if you see this, draw_os_desktop completed ===",
        source_name
    );
    petroleum::serial::serial_log(format_args!("Desktop should be visible now!\n"));
    true
}

/// Helper function to backup VGA text buffer content
#[cfg(target_os = "uefi")]
fn backup_vga_text_buffer(buffer: &mut [[u16; 80]; 25]) {
    unsafe {
        let vga_ptr = crate::VGA_BUFFER_ADDRESS as *const [[u16; 80]; 25];
        *buffer = *vga_ptr;
    }
}

/// Helper function to allocate a buffer for VGA backup
#[cfg(target_os = "uefi")]
fn create_vga_backup() -> Option<Box<[[u16; 80]; 25]>> {
    let mut buffer = Box::new([[0u16; 80]; 25]);
    backup_vga_text_buffer(&mut buffer);
    Some(buffer)
}

/// Helper function to restore VGA text buffer content
#[cfg(target_os = "uefi")]
fn restore_vga_text_buffer(buffer: &Box<[[u16; 80]; 25]>) {
    unsafe {
        let vga_ptr = crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25];
        *vga_ptr = **buffer;
    }
}

/// Helper function to try initializing Cirrus graphics mode for desktop display
/// Returns true if graphics mode was successfully initialized and desktop drawn
#[cfg(target_os = "uefi")]
pub fn try_initialize_cirrus_graphics_mode() -> bool {
    log::info!("Trying to initialize Cirrus graphics mode...");
    // Check if Cirrus VGA device was detected
    if !petroleum::graphics::detect_cirrus_vga() {
        log::info!("No Cirrus VGA device detected, cannot initialize graphics mode");
        return false;
    }

    log::info!("Cirrus VGA device detected, setting up graphics mode...");
    // Set up VGA mode 13h (320x200, 256 colors) for graphics
    petroleum::graphics::setup_cirrus_vga_mode();

    // VGA framebuffer configuration is handled by uefi_vga_config below

    log::info!("Initializing VGA framebuffer writer...");

    // For UEFI target, we need to initialize VGA framebuffer in UEFI context
    // Create VGA framebuffer configuration for UEFI
    // VGA mode 13h constants are now defined in petroleum::graphics
    let uefi_vga_config = FullereneFramebufferConfig {
        address: VGA_MODE13H_ADDRESS, // Standard VGA framebuffer address
        width: VGA_MODE13H_WIDTH,
        height: VGA_MODE13H_HEIGHT,
        pixel_format: petroleum::common::EfiGraphicsPixelFormat::PixelFormatMax, // Special marker for VGA mode
        bpp: VGA_MODE13H_BPP,
        stride: VGA_MODE13H_STRIDE, // 320 bytes per line in mode 13h
    };

    graphics::text::init(&uefi_vga_config);

    // Verify the framebuffer was initialized
    if let Some(fb_writer) = graphics::text::FRAMEBUFFER_UEFI.get() {
        let fb_info = &mut fb_writer.lock();
        log::info!(
            "VGA framebuffer initialized successfully - width: {}, height: {}",
            fb_info.get_width(),
            fb_info.get_height()
        );

        log::info!("Drawing desktop on VGA graphics mode...");
        graphics::draw_os_desktop();
        log::info!("Desktop drawing completed - graphics mode should be visible");
        petroleum::serial::serial_log(format_args!("Desktop should be visible now!\n"));
        true
    } else {
        log::info!("ERROR: VGA framebuffer initialization failed!");
        false
    }
}

/// Helper function to initialize graphics with framebuffer configuration
/// Returns true if graphics were successfully initialized and drawn
#[cfg(target_os = "uefi")]
pub fn initialize_graphics_with_config(_system_table: &EfiSystemTable) -> bool {
    // First, check if we have a saved framebuffer config from before exit_boot_services
    log::info!("Checking for saved framebuffer config...");
    if let Some(saved_config_mutex) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get() {
        if let Some(saved_config) = *saved_config_mutex.lock() {
            log::info!(
                "Using saved framebuffer config: {}x{} @ {:#x}, stride: {}, pixel_format: {:?}",
                saved_config.width,
                saved_config.height,
                saved_config.address,
                saved_config.stride,
                saved_config.pixel_format
            );
            return try_init_graphics(&saved_config, "Saved UEFI config");
        }
    }

    log::info!("No saved framebuffer config found, trying kernel-side fallback detection...");
    // Try kernel-side fallback framebuffer detection when bootloader config table installation hangs
    if let Some(fallback_config) = kernel_fallback_framebuffer_detection() {
        log::info!(
            "Found kernel-detected framebuffer config: {}x{} @ {:#x}, stride: {}, pixel_format: {:?}",
            fallback_config.width,
            fallback_config.height,
            fallback_config.address,
            fallback_config.stride,
            fallback_config.pixel_format
        );
        return try_init_graphics(&fallback_config, "Kernel fallback");
    }

    log::info!("No kernel fallback graphics modes found, trying Cirrus VGA fallback...");

    // As a fallback, try Cirrus VGA graphics if the function exists
    try_initialize_cirrus_graphics_mode()
}
