#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

pub(crate) mod font;
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
mod serial;
mod vga;

extern crate alloc;

use core::panic::PanicInfo;

use core::ffi::c_void;
use spin::Once;
use petroleum::common::{
    EfiMemoryType, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};
use petroleum::page_table::EfiMemoryDescriptor;
use x86_64::VirtAddr;
use x86_64::instructions::hlt;

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    petroleum::handle_panic(info);
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

    serial::serial_log("Kernel: efi_main entered (via serial_log).\n");

    // Reinitialize page table after exit boot services
    let descriptors = unsafe {
        core::slice::from_raw_parts(
            _memory_map as *const EfiMemoryDescriptor,
            _memory_map_size / core::mem::size_of::<EfiMemoryDescriptor>(),
        )
    };
    MEMORY_MAP.call_once(|| unsafe { &*(descriptors as *const _) });
    heap::init_frame_allocator(*MEMORY_MAP.get().unwrap());
    heap::reinit_page_table();

    // Common initialization for both UEFI and BIOS
    init_common(_memory_map, _memory_map_size);

    serial::serial_log("Interrupts initialized via init().");

    serial::serial_log("Entering efi_main...\n");
    serial::serial_log("Searching for framebuffer config table...\n");

    // Cast the system_table pointer to the correct type
    let system_table = unsafe { &*(system_table as *const EfiSystemTable) };

    let mut framebuffer_config: Option<&FullereneFramebufferConfig> = None;

    // Iterate through the configuration tables to find the framebuffer configuration
    let config_table_entries = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };
    for entry in config_table_entries {
        if entry.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            framebuffer_config =
                unsafe { Some(&*(entry.vendor_table as *const FullereneFramebufferConfig)) };
            break;
        }
    }

    if let Some(config) = framebuffer_config {
        if config.address == 0 {
            panic!("Framebuffer address 0 - check bootloader GOP install");
        } else {
            let _ = core::fmt::write(
                &mut *serial::SERIAL1.lock(),
                format_args!("  Address: {:#x}\n", config.address),
            );
            let _ = core::fmt::write(
                &mut *serial::SERIAL1.lock(),
                format_args!("  Resolution: {}x{}\n", config.width, config.height),
            );
            graphics::init(config);
            serial::serial_log("Graphics initialized.");
        }
    } else {
        serial::serial_log("Fullerene Framebuffer Config Table not found.\n");
    }

    // Main loop
    println!("Hello QEMU by FullereneOS");
    hlt_loop();
}

#[cfg(target_os = "uefi")]
fn init_common(_memory_map: *mut c_void, _memory_map_size: usize) {
    let descriptors = *MEMORY_MAP.get().unwrap();

    let mut loader_data_start = None;
    for desc in descriptors {
        if desc.type_ == EfiMemoryType::EfiLoaderData && desc.number_of_pages > 0 {
            loader_data_start = Some(x86_64::PhysAddr::new(desc.physical_start));
            break;
        }
    }
    let loader_data_start = loader_data_start.expect("No LoaderData region found in memory map");

    let heap_start = heap::allocate_heap_from_map(loader_data_start, heap::HEAP_SIZE);

    let heap_start = gdt::init(heap_start); // Initialize GDT with heap start, get adjusted heap start
    interrupts::init(); // Initialize IDT
    heap::init(heap_start, heap::HEAP_SIZE); // Initialize heap
    serial::serial_init(); // Initialize serial early for debugging
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
    serial::serial_init(); // Initialize serial early for debugging
}

#[cfg(not(target_os = "uefi"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use petroleum::common::VgaFramebufferConfig;

    init_common();
    serial::serial_log("Entering _start...\n");

    // Graphics initialization for VGA framebuffer (graphics mode)
    let vga_config = VgaFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        bpp: 8,
    };
    graphics::init_vga(&vga_config);

    serial::serial_log("VGA graphics mode initialized.");

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
