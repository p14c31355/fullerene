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

use petroleum::serial::{SERIAL_PORT_WRITER as SERIAL1, serial_init, serial_log};

use core::ffi::c_void;
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;
use spin::Once;
use x86_64::VirtAddr;
use x86_64::instructions::hlt;

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($msg:expr) => {
        serial_log(concat!($msg, "\n"));
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
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
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
    system_table: *mut c_void,
    _memory_map: *mut c_void,
    _memory_map_size: usize,
) -> ! {
    // Early debug print to confirm kernel entry point is reached using direct port access
    use x86_64::instructions::port::Port;
    let mut port = Port::new(0x3F8);
    unsafe {
        let msg = b"Kernel: efi_main entered.\n";
        for &byte in msg {
            while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
            port.write(byte);
        }
    }

    // Reinitialize page table after exit boot services
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            _memory_map as *const EfiMemoryDescriptor,
            _memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
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

    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    heap::init_page_table(physical_memory_offset);
    heap::reinit_page_table(physical_memory_offset, kernel_phys_start);

    // Common initialization for both UEFI and BIOS
    init_common(_memory_map, _memory_map_size);

    kernel_log!("Kernel: efi_main entered (via serial_log).");
    kernel_log!("Interrupts initialized via init().");

    kernel_log!("Entering efi_main...");
    kernel_log!("Searching for framebuffer config table...");

    // Cast the system_table pointer to the correct type
    let system_table = unsafe { &*(system_table as *const EfiSystemTable) };

    if let Some(config) = find_framebuffer_config(system_table) {
        if config.address == 0 {
            panic!("Framebuffer address 0 - check bootloader GOP install");
        } else {
            let _ = core::fmt::write(
                &mut *SERIAL1.lock(),
                format_args!("  Address: {:#x}\n", config.address),
            );
            let _ = core::fmt::write(
                &mut *SERIAL1.lock(),
                format_args!("  Resolution: {}x{}\n", config.width, config.height),
            );
            graphics::init(config);
            kernel_log!("Graphics initialized.");
        }
    } else {
        kernel_log!("Fullerene Framebuffer Config Table not found.");
    }

    // Also initialize VGA text mode for reliable output
    vga::vga_init();

    // Main loop
    println!("Hello QEMU by FullereneOS");
    hlt_loop();
}

#[cfg(target_os = "uefi")]
fn init_common(_memory_map: *mut c_void, _memory_map_size: usize) {
    let descriptors = *MEMORY_MAP.get().unwrap();
    let heap_phys_start = find_heap_start(descriptors);
    let heap_start = heap::allocate_heap_from_map(heap_phys_start, heap::HEAP_SIZE);
    let heap_start = gdt::init(heap_start); // Initialize GDT with heap start, get adjusted heap start
    interrupts::init(); // Initialize IDT
    heap::init(heap_start, heap::HEAP_SIZE); // Initialize heap
    serial_init(); // Initialize serial early for debugging
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
    serial_init(); // Initialize serial early for debugging
}

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use petroleum::common::VgaFramebufferConfig;

    init_common();
    kernel_log!("Entering _start...");

    // Graphics initialization for VGA framebuffer (graphics mode)
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    kernel_log!("VGA graphics mode initialized.");

    // Main loop
    println!("Hello QEMU by FullereneOS");
    hlt_loop();
}

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}
