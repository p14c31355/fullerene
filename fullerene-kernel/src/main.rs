#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

pub(crate) mod font;
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
// mod serial; // Removed, now using petroleum
mod vga;

extern crate alloc;

use core::panic::PanicInfo;

// use petroleum::serial::{SERIAL_PORT_WRITER as SERIAL1, serial_init, serial_log};
use petroleum::serial::{
    SERIAL_PORT_WRITER as SERIAL1, debug_print_hex, debug_print_str_to_com1 as debug_print_str,
};

use core::ffi::c_void;
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig, VgaFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;
use petroleum::write_serial_bytes;
use spin::Once;
use x86_64::instructions::hlt;
use x86_64::{PhysAddr, VirtAddr};

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($($arg:tt)*) => {
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!($($arg)*));
        let _ = core::fmt::write(&mut *SERIAL1.lock(), format_args!("\n"));
    };
}

// Helper function to find framebuffer config
fn find_framebuffer_config(system_table: &EfiSystemTable) -> Option<&FullereneFramebufferConfig> {
    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };
    for entry in config_table_entries {
        kernel_log!(
            "Checking config table entry: GUID={:?}",
            entry.vendor_guid
        );
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            kernel_log!("Found matching Fullerene framebuffer config GUID");
            return unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
        }
    }
    None
}

// Helper function to find heap start from memory map
fn find_heap_start(descriptors: &[EfiMemoryDescriptor]) -> x86_64::PhysAddr {
    // First, try to find EfiLoaderData
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiLoaderData && desc.number_of_pages > 0 {
            return x86_64::PhysAddr::new(desc.physical_start);
        }
    }
    // If not found, find EfiConventionalMemory large enough
    let required_pages = (heap::HEAP_SIZE + 4095) / 4096;
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiConventionalMemory
            && desc.number_of_pages >= required_pages as u64
        {
            return x86_64::PhysAddr::new(desc.physical_start);
        }
    }
    panic!("No suitable memory region found for heap");
}

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    petroleum::handle_panic(info)
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

    // Early VGA text output to ensure visible output on screen
    {
        let vga_buffer = unsafe { &mut *(0xb8000 as *mut [[u16; 80]; 25]) };
        let hello = b"UEFI Kernel Starting...";
        for (i, &byte) in hello.iter().enumerate() {
            if i < 80 {
                vga_buffer[0][i] = (0x0F00 as u16) | (byte as u16); // White on black
            }
        }
    }

    // Helper function for kernel debug prints
    fn print_kernel(msg: &str) {
        write_serial_bytes!(0x3F8, 0x3FD, msg.as_bytes());
    }

    print_kernel("Kernel: starting to parse parameters.\n");

    // Verify our own address as sanity check for PE relocation
    let self_addr = efi_main as u64;
    debug_print_str("Kernel: efi_main located at ");
    debug_print_hex(self_addr as usize);
    debug_print_str("\n");

    // Cast system_table to reference
    let system_table = unsafe { &*system_table };

    // Use the passed memory map
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            memory_map as *const EfiMemoryDescriptor,
            memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    MEMORY_MAP.call_once(|| unsafe { &*(descriptors as *const _) });

    // Calculate physical_memory_offset from kernel's location in memory map
    let kernel_virt_addr = efi_main as u64;
    let mut physical_memory_offset = VirtAddr::new(0);
    let mut kernel_phys_start = x86_64::PhysAddr::new(0);

    // Find physical_memory_offset and kernel_phys_start
    for desc in descriptors {
        let virt_start = desc.virtual_start;
        let virt_end = virt_start + desc.number_of_pages * 4096;
        if kernel_virt_addr >= virt_start && kernel_virt_addr < virt_end {
            physical_memory_offset = VirtAddr::new(desc.virtual_start - desc.physical_start);
            if desc.type_ == EfiMemoryType::EfiLoaderCode {
                kernel_phys_start = x86_64::PhysAddr::new(desc.physical_start);
            }
        }
    }

    if kernel_phys_start.is_null() {
        panic!("Could not determine kernel's physical start address.");
    }

    print_kernel("Kernel: phys offset found.\n");
    kernel_log!("Kernel: memory map parsed, kernel_phys_start found");
    kernel_log!("Starting heap frame allocator init...");

    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    print_kernel("Kernel: frame allocator init done.\n");
    heap::init_page_table(physical_memory_offset);
    print_kernel("Kernel: page table init done.\n");

    heap::reinit_page_table(physical_memory_offset, kernel_phys_start);
    print_kernel("Kernel: page table reinit done.\n");

    // Initialize GDT with proper heap address
    let heap_phys_start = find_heap_start(descriptors);
    let heap_start = heap::allocate_heap_from_map(heap_phys_start, heap::HEAP_SIZE);
    let heap_start_after_gdt = gdt::init(heap_start);
    print_kernel("Kernel: GDT init done.\n");

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
    print_kernel("Kernel: IDT init done.\n");

    // Common initialization (enables interrupts)
    init_common();
    print_kernel("Kernel: init_common done.\n");

    kernel_log!("Kernel: efi_main entered (via serial_log).");
    kernel_log!("GDT initialized.");
    kernel_log!("IDT initialized.");
    kernel_log!("APIC initialized.");
    kernel_log!("Heap initialized.");
    kernel_log!("Serial initialized.");

    kernel_log!("Searching for framebuffer config table...");
    if let Some(config) = find_framebuffer_config(system_table) {
        if config.address != 0 {
            graphics::init(config);
            kernel_log!("GOP graphics initialized.");
        } else {
            panic!("Framebuffer address is 0, check bootloader GOP install");
        }
    } else {
        kernel_log!("Fullerene Framebuffer Config Table not found, falling back to VGA.");
        let vga_config = VgaFramebufferConfig {
            address: 0xA0000,
            width: 320,
            height: 200,
            bpp: 8,
        };
        graphics::init_vga(&vga_config);
        kernel_log!("VGA graphics initialized.");
    }
    println!("Hello QEMU by FullereneOS");

    // Keep kernel running instead of exiting
    print_kernel("Kernel: running in main loop...\n");
    kernel_log!("FullereneOS kernel is now running.");
    hlt_loop();
}

#[cfg(target_os = "uefi")]
fn init_common() {
    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    interrupts::init_apic();
    kernel_log!("Kernel: APIC initialized and interrupts enabled");

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
    use x86_64::VirtAddr;

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
