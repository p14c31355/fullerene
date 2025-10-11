#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

// Kernel modules
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
mod vga;
// Kernel modules
mod context_switch; // Context switching
mod fs; // Basic filesystem
mod keyboard; // Keyboard input driver
mod loader; // Program loader
mod memory_management; // Virtual memory management
mod process; // Process management
mod shell;
mod syscall; // System calls // Shell/CLI interface

extern crate alloc;

// use petroleum::serial::{SERIAL_PORT_WRITER as SERIAL1, serial_init, serial_log};
use petroleum::graphics::init_vga_text_mode;
use petroleum::serial::{
    SERIAL_PORT_WRITER as SERIAL1, debug_print_hex, debug_print_str_to_com1 as debug_print_str,
};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    petroleum::handle_panic(info)
}

use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig, VgaFramebufferConfig,
};
use core::ffi::c_void;
use petroleum::page_table::EfiMemoryDescriptor;
use petroleum::write_serial_bytes;
use spin::Once;
use x86_64::instructions::hlt;
use x86_64::{PhysAddr, VirtAddr};

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

// Removved helper function, use write_serial_bytes! directly

// Generic helper for searching memory descriptors
fn find_memory_descriptor_address<F>(
    descriptors: &[EfiMemoryDescriptor],
    predicate: F,
) -> Option<usize>
where
    F: Fn(&EfiMemoryDescriptor) -> bool,
{
    descriptors
        .iter()
        .find(|desc| predicate(desc))
        .map(|desc| desc.physical_start as usize)
}

// Helper function to find framebuffer config (using generic)
fn find_framebuffer_config(system_table: &EfiSystemTable) -> Option<&FullereneFramebufferConfig> {
    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };
    for entry in config_table_entries {
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

// Helper function to find heap start from memory map (using generic)
fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> x86_64::PhysAddr {
    // First, try to find EfiLoaderData
    if let Some(addr) = find_memory_descriptor_address(descriptors, |desc| {
        desc.type_ == EfiMemoryType::EfiLoaderData && desc.number_of_pages > 0
    }) {
        return x86_64::PhysAddr::new(addr as u64);
    }
    // If not found, find EfiConventionalMemory large enough
    let required_pages = (heap::HEAP_SIZE + 4095) / 4096;
    if let Some(addr) = find_memory_descriptor_address(descriptors, |desc| {
        desc.type_ == EfiMemoryType::EfiConventionalMemory
            && desc.number_of_pages >= required_pages as u64
    }) {
        return x86_64::PhysAddr::new(addr as u64);
    }
    panic!("No suitable memory region found for heap");
}

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

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
        let vga_buffer = unsafe { &mut *(VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]) };
        // Clear screen first
        for row in 0..25 {
            for col in 0..80 {
                vga_buffer[row][col] = VGA_COLOR_GREEN_ON_BLACK | b' ' as u16;
            }
        }
        // Write hello message
        let hello = b"Hello from UEFI Kernel!";
        for (i, &byte) in hello.iter().enumerate() {
            if i < hello.len() {
                vga_buffer[0][i] = VGA_COLOR_GREEN_ON_BLACK | (byte as u16);
            }
        }
    }
    kernel_log!("VGA buffer write completed");

    // Use the passed memory map
    debug_print_str("About to create memory map slice\n");
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            memory_map as *const EfiMemoryDescriptor,
            memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    debug_print_str("Memory map slice created\n");
    kernel_log!("Memory map slice size: {}, descriptor count: {}", memory_map_size, descriptors.len());
    for (i, desc) in descriptors.iter().enumerate() {
        kernel_log!("Memory descriptor {}: type={:#x}, phys_start=0x{:x}, virt_start=0x{:x}, pages=0x{:x}",
                   i, desc.type_ as u32, desc.physical_start, desc.virtual_start, desc.number_of_pages);
    }
    kernel_log!("Memory map parsing: finished descriptor dump");
    MEMORY_MAP.call_once(|| unsafe { &*(descriptors as *const _) });
    kernel_log!("MEMORY_MAP initialized");

    // Calculate physical_memory_offset from kernel's location in memory map
    kernel_log!("Starting to calculate physical_memory_offset...");
    let kernel_virt_addr = efi_main as u64;
    let mut physical_memory_offset = VirtAddr::new(0);
    let mut kernel_phys_start = x86_64::PhysAddr::new(0);
    kernel_log!("Kernel virtual address: 0x{:x}", kernel_virt_addr);

    // Find physical_memory_offset and kernel_phys_start
    kernel_log!("Scanning memory descriptors to find kernel location...");
    let mut found_in_descriptor = false;
    for (i, desc) in descriptors.iter().enumerate() {
        let virt_start = desc.virtual_start;
        let virt_end = virt_start + desc.number_of_pages * 4096;
        kernel_log!("Checking descriptor {}: virt_start=0x{:x}, virt_end=0x{:x}, type={:#x}",
                   i, virt_start, virt_end, desc.type_ as u32);
        if kernel_virt_addr >= virt_start && kernel_virt_addr < virt_end {
            physical_memory_offset = VirtAddr::new(desc.virtual_start - desc.physical_start);
            found_in_descriptor = true;
            kernel_log!("Found kernel in descriptor {}: phys_offset=0x{:x}",
                       i, physical_memory_offset.as_u64());
            if desc.type_ == EfiMemoryType::EfiLoaderCode {
                kernel_phys_start = x86_64::PhysAddr::new(desc.physical_start);
                kernel_log!("Kernel is in EfiLoaderCode, phys_start=0x{:x}",
                           kernel_phys_start.as_u64());
            }
        }
    }

    if !found_in_descriptor {
        kernel_log!("WARNING: Kernel virtual address not found in any descriptor!");
    }

    if kernel_phys_start.is_null() {
        kernel_log!("Could not determine kernel's physical start address, setting to 0");
        // Try to find any suitable memory for kernel
        for desc in descriptors {
            if desc.type_ == EfiMemoryType::EfiLoaderCode && desc.number_of_pages > 0 {
                kernel_phys_start = x86_64::PhysAddr::new(desc.physical_start);
                kernel_log!("Using first EfiLoaderCode descriptor: phys_start=0x{:x}",
                           kernel_phys_start.as_u64());
                break;
            }
        }
        if kernel_phys_start.is_null() {
            kernel_log!("Still null, setting kernel_phys_start to 0");
            kernel_phys_start = x86_64::PhysAddr::new(0);
        }
    }

    // Assume identity mapping for now
    if !found_in_descriptor {
        physical_memory_offset = VirtAddr::new(0);
        kernel_log!("WARNING: Kernel virtual address not found, assuming identity mapping");
    }
    kernel_log!("Continuing with assumed offset=0");

    kernel_log!("Physical memory offset calculation complete: offset=0x{:x}, kernel_phys_start=0x{:x}",
               physical_memory_offset.as_u64(), kernel_phys_start.as_u64());
    kernel_log!("Starting heap frame allocator init...");

    kernel_log!("Calling heap::init_frame_allocator with {} descriptors", MEMORY_MAP.get().unwrap().len());
    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    kernel_log!("Heap frame allocator init completed successfully");

    kernel_log!("Calling heap::init_page_table with offset 0x{:x}", physical_memory_offset.as_u64());
    heap::init_page_table(physical_memory_offset);
    kernel_log!("Page table init completed successfully");

    kernel_log!("Calling heap::reinit_page_table with offset 0x{:x} and kernel_phys_start 0x{:x}",
               physical_memory_offset.as_u64(), kernel_phys_start.as_u64());
    heap::reinit_page_table(physical_memory_offset, kernel_phys_start);
    kernel_log!("Page table reinit completed - this is where crash likely occurs");
    kernel_log!("Page table reinit completed successfully");

    // Set physical memory offset for process management
    crate::memory_management::set_physical_memory_offset(physical_memory_offset);

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(descriptors);
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
    init_common();
    kernel_log!("Kernel: init_common done");

    kernel_log!("Kernel: efi_main entered");
    kernel_log!("GDT initialized");
    kernel_log!("IDT initialized");
    kernel_log!("APIC initialized");
    kernel_log!("Heap initialized");
    kernel_log!("Serial initialized");

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
    kernel_log!("VGA graphics desktop drawn");

    kernel_log!("Kernel: running in main loop");
    kernel_log!("FullereneOS kernel is now running");
    hlt_loop();
}

#[cfg(target_os = "uefi")]
fn init_common() {
    crate::vga::init_vga();
    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    kernel_log!("Kernel: APIC initialized and interrupts enabled");

    // Initialize process management
    process::init();
    kernel_log!("Kernel: Process management initialized");

    // Initialize system calls
    syscall::init();
    kernel_log!("Kernel: System calls initialized");

    // Initialize filesystem
    fs::init();
    kernel_log!("Kernel: Filesystem initialized");

    // Initialize program loader
    loader::init();
    kernel_log!("Kernel: Program loader initialized");

    // Create a test user process
    let test_entry = x86_64::VirtAddr::new(test_process_main as usize as u64);
    let test_pid = process::create_process("test_process", test_entry);
    kernel_log!("Kernel: Created test process with PID {}", test_pid);

    // Test interrupt handling - should not panic or crash if APIC is working
    kernel_log!("Testing interrupt handling with int3...");
    unsafe {
        x86_64::instructions::interrupts::int3();
    }
    kernel_log!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
fn init_common() {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; heap::HEAP_SIZE] = [MaybeUninit::uninit(); heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        heap::ALLOCATOR.lock().init(heap_start_ptr, heap::HEAP_SIZE);
    }

    gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga();
}

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use petroleum::common::VgaFramebufferConfig;

    init_common();
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
    println!("Hello QEMU by FullereneOS");

    // Keep kernel running instead of exiting
    kernel_log!("BIOS boot complete, kernel running...");
    hlt_loop();
}

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}

// Test process main function
fn test_process_main() {
    // Simple test process that demonstrates system calls using proper syscall instruction
    unsafe fn syscall(num: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64, arg6: u64) -> u64 {
        let result: u64;
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            in("r8") arg5,
            in("r9") arg6,
            lateout("rax") result,
            out("rcx") _, out("r11") _,
        );
        result
    }

    // Write to stdout via syscall
    let message = b"Hello from test user process!\n";
    unsafe {
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1, // fd (stdout)
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
            0,
        );
    }

    // Get PID via syscall and print the actual PID
    unsafe {
        let pid = syscall(crate::syscall::SyscallNumber::GetPid as u64, 0, 0, 0, 0, 0, 0);
        let pid_msg = b"My PID is: ";
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_msg.as_ptr() as u64,
            pid_msg.len() as u64,
            0,
            0,
            0,
        );

        // Convert PID to string and print it
        let pid_str = alloc::format!("{}\n", pid);
        let pid_bytes = pid_str.as_bytes();
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_bytes.as_ptr() as u64,
            pid_bytes.len() as u64,
            0,
            0,
            0,
        );
    }

    // Yield a bit
    unsafe {
        syscall(crate::syscall::SyscallNumber::Yield as u64, 0, 0, 0, 0, 0, 0); // SYS_YIELD
        syscall(crate::syscall::SyscallNumber::Yield as u64, 0, 0, 0, 0, 0, 0); // SYS_YIELD
    }

    // Exit
    unsafe {
        syscall(crate::syscall::SyscallNumber::Exit as u64, 0, 0, 0, 0, 0, 0); // SYS_EXIT
    }
}
