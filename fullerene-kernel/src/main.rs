#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

mod gdt; // Add GDT module
mod graphics;
mod interrupts;
mod serial;

extern crate alloc;

use core::ffi::c_void;
use petroleum::common::{
    EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
};
use x86_64::instructions::hlt;

#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut c_void,
    _memory_map: *mut c_void,
    _memory_map_size: usize,
) -> ! {
    gdt::init(); // Initialize GDT
    interrupts::init(); // Initialize IDT

    serial::serial_init(); // Initialize serial early for debugging

    serial::serial_log("Initializing PICs...");
    // Initialize the PIC before enabling interrupts to prevent premature timer interrupts.
    unsafe { interrupts::PICS.lock().initialize() };
    serial::serial_log("PICs initialized.");

    // Now enable interrupts after everything is set up.
    x86_64::instructions::interrupts::enable();
    serial::serial_log("Interrupts enabled.");

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
            serial::serial_log("Fullerene Framebuffer Config Table found, but address is 0.\n");
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
            graphics::init(config);
            serial::serial_log("Graphics initialized.");
        }
    } else {
        serial::serial_log("Fullerene Framebuffer Config Table not found.\n");
    }

    // Main loop
    serial::serial_log("Initialization complete. Entering kernel main loop.\n");
    hlt_loop();
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

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}
