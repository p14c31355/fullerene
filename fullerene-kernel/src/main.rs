#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

mod gdt; // Add GDT module
mod interrupts;
mod serial;
mod uefi;
mod vga; // Add IDT module

extern crate alloc;

use core::ffi::c_void;
use uefi::{EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig};

#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut c_void,
    _memory_map: *mut c_void,
    _memory_map_size: usize,
) -> ! {
    gdt::init(); // Initialize GDT
    interrupts::init_idt(); // Initialize IDT
    unsafe { interrupts::PICS.lock().initialize() };
    x86_64::instructions::interrupts::enable();

    // Initialize serial and VGA first for logging
    serial::serial_init();
    vga::vga_init();

    vga::log("Entering efi_main...");
    vga::log("Searching for framebuffer config table...");

    // Cast the system_table pointer to the correct type
    let system_table = unsafe { &*(system_table as *const EfiSystemTable) };

    let mut framebuffer_config: Option<&FullereneFramebufferConfig> = None;

    // Iterate through the configuration tables to find the framebuffer configuration
    let config_tables = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries as usize,
        )
    };
    for table in config_tables {
        if table.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            framebuffer_config = unsafe {
                Some(&*(table.vendor_table as *const FullereneFramebufferConfig))
            };
            break;
        }
    }

    if let Some(config) = framebuffer_config {
        // Correct logging using the serial port, as `vga::log` doesn't support numeric formatting
        // without an allocator, which is not available.
        serial::serial_log("Found framebuffer configuration!");

        // Add a check to prevent `format_args!` from overflowing internal `u16`
        // operations if width/height are too large.
        if config.width > u16::MAX as u32 || config.height > u16::MAX as u32 {
            serial::serial_log("  WARNING: Framebuffer resolution is too large to format safely!");
            serial::serial_log("  This may be the cause of the kernel panic.");
        } else {
            let _ = core::fmt::write(
                &mut *serial::SERIAL1.lock(),
                format_args!("  Address: {:#x}\n", config.address),
            );
            let _ = core::fmt::write(
                &mut *serial::SERIAL1.lock(),
                format_args!("  Resolution: {}x{}\n", config.width, config.height),
            );
        }
    } else {
        vga::log("Fullerene Framebuffer Config Table not found.");
        serial::serial_log("Fullerene Framebuffer Config Table not found.");
    }

    // Main loop
    vga::log("Initialization complete. Entering kernel main loop.");
    hlt_loop();
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    vga::panic_log(info);
    serial::panic_log(info);
    loop {}
}

// Global allocator is required for `alloc::format!`
#[global_allocator]
static ALLOC: DummyAllocator = DummyAllocator;

pub struct DummyAllocator;

unsafe impl core::alloc::GlobalAlloc for DummyAllocator {
    unsafe fn alloc(&self, _layout: core::alloc::Layout) -> *mut u8 {
        panic!("memory allocation is not supported");
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {
        panic!("memory deallocation is not supported");
    }
}

// A simple loop to halt the CPU, preventing the program from exiting.
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}