// bellows/src/loader/mod.rs

use core::ffi::c_void;
use core::ptr;
use petroleum::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable};
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
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
) -> petroleum::common::Result<!> {
    let bs = unsafe { &*(*system_table).boot_services };

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

        // First call: Get the required buffer size (with NULL buffer)
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                ptr::null_mut(),
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };

        if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
            println!(
                "Error: Failed to get initial memory map size: {:?}",
                EfiStatus::from(status)
            );
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
            println!(
                "Error: Failed to allocate memory map buffer: {:?}",
                EfiStatus::from(alloc_status)
            );
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
                println!(
                    "Memory map acquired after {} attempts. Size: {}",
                    attempts, map_size
                );
                break;
            }
            EfiStatus::BufferTooSmall => {
                println!("Buffer too small (size now {}), retrying...", map_size);
                continue;
            }
            _ => {
                (bs.free_pages)(map_phys_addr, map_pages);
                println!(
                    "Error: Failed to get memory map: {:?}",
                    EfiStatus::from(status)
                );
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    // Exit boot services. This call must succeed.
    let exit_status = (bs.exit_boot_services)(image_handle, map_key);
    if EfiStatus::from(exit_status) != EfiStatus::Success {
        println!(
            "Error: Failed to exit boot services: {:?}",
            EfiStatus::from(exit_status)
        );
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
    println!(
        "Jumping to kernel at {:#x} with map at {:#x} size {}",
        entry as usize, map_phys_addr, map_size
    );
    debug_print_str("About to call kernel entry.\n");
    entry(image_handle, system_table, map_ptr, map_size);
}
