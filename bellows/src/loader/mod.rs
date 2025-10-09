// bellows/src/loader/mod.rs

use core::ffi::c_void;
use core::ptr;
use alloc::boxed::Box;
use petroleum::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable, FULLERENE_MEMORY_MAP_CONFIG_TABLE_GUID, FullereneMemoryMap};
use petroleum::println; // Added for debugging
pub use petroleum::serial::{debug_print_hex, debug_print_str_to_com1 as debug_print_str};

pub mod file;
pub mod heap;
pub mod pe;

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable) -> !,
) -> petroleum::common::Result<!> {
    debug_print_str("Inside exit_boot_services_and_jump.\n");
    debug_print_str("system_table = ");
    debug_print_hex(system_table as usize);
    debug_print_str("\n");
    let bs = unsafe { &*(*system_table).boot_services };
    debug_print_str("bs obtained.\n");
    debug_print_str("About to set up memory map vars.\n");

    // Initial setup for memory map
    let mut map_size: usize = 0;
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;
    let mut map_phys_addr: usize = 0;
    let mut map_pages: usize = 0;

    // Loop to get the memory map until successful
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 5;

    loop {
        if attempts >= MAX_ATTEMPTS {
            return Err(BellowsError::InvalidState(
                "Too many attempts to get memory map.",
            ));
        }
        attempts += 1;
        debug_print_str("Loop start, attempts=");
        debug_print_hex(attempts);
        debug_print_str(", map_size=");
        debug_print_hex(map_size);
        debug_print_str("\n");

        // First call: Get the required buffer size (with NULL buffer)
        debug_print_str("About to call get_memory_map (null buffer)\n");
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                ptr::null_mut(),
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };
        debug_print_str("get_memory_map returned, status=");
        debug_print_hex(status as usize);
        debug_print_str("\n");

        if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
            petroleum::serial::_print(format_args!(
                "Error: Failed to get initial memory map size: {:?}\n",
                EfiStatus::from(status)
            ));
            return Err(BellowsError::InvalidState(
                "Failed to get initial memory map size.",
            ));
        }

        // Allocate buffer with some extra space, capped at 64KiB
        let alloc_size = map_size.saturating_add(4096);
        let new_map_pages = alloc_size.div_ceil(4096).max(1);
        let mut new_map_phys_addr: usize = 0;

        let alloc_status = (bs.allocate_pages)(
            0usize, // AllocateAnyPages
            EfiMemoryType::EfiLoaderData,
            new_map_pages,
            &mut new_map_phys_addr,
        );

        if EfiStatus::from(alloc_status) != EfiStatus::Success {
            // If we had a previous allocation, free it before returning an error
            if map_phys_addr != 0 {
                let _ = (bs.free_pages)(map_phys_addr, map_pages); // Ignore status
            }
            petroleum::serial::_print(format_args!(
                "Error: Failed to allocate memory map buffer: {:?}\n",
                EfiStatus::from(alloc_status)
            ));
            return Err(BellowsError::AllocationFailed(
                "Failed to allocate memory map buffer.",
            ));
        }

        // Free previous allocation if it exists
        if map_phys_addr != 0 {
            let _ = (bs.free_pages)(map_phys_addr, map_pages); // Ignore status
        }

        map_phys_addr = new_map_phys_addr;
        map_pages = new_map_pages;

        // Set map_size to the allocated buffer size for input
        map_size = new_map_pages * 4096;

        // Second call: Get the memory map with the allocated buffer
        let status = (bs.get_memory_map)(
            &mut map_size,
            map_phys_addr as *mut c_void,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );

        match EfiStatus::from(status) {
            EfiStatus::Success => {
                petroleum::serial::_print(format_args!(
                    "Memory map acquired after {} attempts. Size: {}\n",
                    attempts, map_size
                ));
                break;
            }
            EfiStatus::BufferTooSmall => {
                petroleum::serial::_print(format_args!("Buffer too small (size now {}), retrying...\n", map_size));
                continue;
            }
            _ => {
                (bs.free_pages)(map_phys_addr, map_pages);
                petroleum::serial::_print(format_args!(
                    "Error: Failed to get memory map: {:?}\n",
                    EfiStatus::from(status)
                ));
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    // Install the memory map into the configuration table
    let mm_config = FullereneMemoryMap {
        physical_address: map_phys_addr as u64,
        size: map_size,
    };
    let mm_config_ptr = Box::into_raw(Box::new(mm_config));
    unsafe {
        let status = (bs.install_configuration_table)(
            FULLERENE_MEMORY_MAP_CONFIG_TABLE_GUID.as_ptr() as *const u8,
            mm_config_ptr as *mut c_void,
        );
        if EfiStatus::from(status) != EfiStatus::Success {
            petroleum::println!("Failed to install memory map config table");
            return Err(BellowsError::InvalidState(
                "Failed to install memory map config table.",
            ));
        }
    }

    // Exit boot services. This call must succeed.
    let exit_status = (bs.exit_boot_services)(image_handle, map_key);
    if EfiStatus::from(exit_status) != EfiStatus::Success {
        petroleum::serial::_print(format_args!(
            "Error: Failed to exit boot services: {:?}\n",
            EfiStatus::from(exit_status)
        ));
        return Err(BellowsError::InvalidState("Failed to exit boot services."));
    }

    // Note: The memory map buffer at `map_phys_addr` is intentionally not freed here
    // because after `exit_boot_services` is called, the boot services are no longer
    // available to the bootloader, making `bs.free_pages` an invalid call.

    // Jump to the kernel. This is the last instruction in the bootloader.
    // Safety:
    // This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    petroleum::serial::_print(format_args!(
        "Jumping to kernel at {:#x} with map at {:#x} size {}\n",
        entry as usize, map_phys_addr, map_size
    ));
    debug_print_str("About to call kernel entry.\n");
    entry(image_handle, system_table);
}
