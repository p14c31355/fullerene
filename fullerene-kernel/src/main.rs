#![feature(abi_x86_interrupt)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

mod gdt; // Add GDT module
mod graphics;
mod interrupts;
mod serial;
mod vga;
mod heap;
pub(crate) mod font;

extern crate alloc;

use core::ffi::c_void;
use petroleum::common::{
    EfiSystemTable, FullereneFramebufferConfig, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID
};
use x86_64::instructions::hlt;

#[cfg(target_os = "uefi")]
#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut c_void,
    _memory_map: *mut c_void,
    _memory_map_size: usize,
) -> ! {
    // Early debug print to confirm kernel entry point is reached
    serial::serial_log("Kernel: efi_main entered.\n");

    // Common initialization for both UEFI and BIOS
    init_common();

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
    println!("Initialization complete. Entering kernel main loop.");
    hlt_loop();
}

// Function to perform common initialization steps for both UEFI and BIOS.
fn init_common() {
    gdt::init(); // Initialize GDT
    interrupts::init(); // Initialize IDT
    heap::init();
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
    println!("Initialization complete. Entering kernel main loop.");
    hlt_loop();
}

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        hlt();
    }
}
