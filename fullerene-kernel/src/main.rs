#![no_std]
#![no_main]

mod serial;
mod uefi;
mod vga;

extern crate alloc;

use core::ffi::c_void;
use uefi::{EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig};

#[unsafe(export_name = "efi_main")]
#[unsafe(link_section = ".text.efi_main")]
pub unsafe extern "efiapi" fn efi_main(_image_handle: usize, system_table: *mut c_void) -> ! {
    // Initialize serial and VGA first for logging
    serial::serial_init();
    vga::vga_init();

    vga::log("Entering efi_main...");
    vga::log("Searching for framebuffer config table...");

    // Cast the system_table pointer to the correct type
    let system_table = unsafe { &*(system_table as *const EfiSystemTable) };

    let mut framebuffer_config: Option<&FullereneFramebufferConfig> = None;

    // Iterate through the configuration tables to find the framebuffer config GUID
    let config_tables = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries,
        )
    };

    for table in config_tables {
        if table.vendor_guid == FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID {
            let config = unsafe { &*(table.vendor_table as *const FullereneFramebufferConfig) };
            framebuffer_config = Some(config);
            break;
        }
    }

    if let Some(_config) = framebuffer_config {
        vga::log("Found framebuffer configuration!");
        vga::log("  Address: <not available without a proper allocator>");
        vga::log("  Resolution: <not available without a proper allocator>");
    } else {
        vga::log("Fullerene Framebuffer Config Table not found.");
    }

    // Main loop
    vga::log("Initialization complete. Entering kernel main loop.");
    loop {}
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
        core::ptr::null_mut()
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}
