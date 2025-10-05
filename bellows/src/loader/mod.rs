// bellows/src/loader/mod.rs

use core::ffi::c_void;
use core::ptr;
use petroleum::common::{BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable};
use petroleum::println; // Added for debugging

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

    // First call: Get the required buffer size (with NULL buffer)
    let status = (bs.get_memory_map)(
        &mut map_size,
        ptr::null_mut(),
        &mut map_key,
        &mut descriptor_size,
        &mut descriptor_version,
    );

    if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
        println!("Error: Failed to get initial memory map size: {:?}", EfiStatus::from(status));
        return Err(BellowsError::InvalidState("Failed to get initial memory map size."));
    }

    // Allocate buffer with some extra space
    let alloc_size = map_size.saturating_add(4096);
    map_pages = alloc_size.div_ceil(4096);
    let mut alloc_phys = 0;
    let alloc_status = (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        map_pages,
        &mut alloc_phys,
    );

    if EfiStatus::from(alloc_status) != EfiStatus::Success {
        println!("Error: Failed to allocate memory map buffer: {:?}", EfiStatus::from(alloc_status));
        return Err(BellowsError::AllocationFailed("Failed to allocate memory map buffer."));
    }
    map_phys_addr = alloc_phys;

    // Retry loop: Call GetMemoryMap with the allocated buffer
    let mut attempts = 0;
    loop {
        attempts += 1;
        println!("Attempt {}: map_key={:#x}, size={}", attempts, map_key, map_size); // Added for debugging

        if attempts > 3 {
            // If BufferTooSmall occurs more than 3 times, reset map_key to 0
            // instead of forcing exit, as per the suggestion.
            // This allows the loop to continue with a potentially fresh map_key.
            if EfiStatus::from((bs.get_memory_map)(
                &mut map_size,
                map_phys_addr as *mut c_void,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )) == EfiStatus::BufferTooSmall {
                println!("BufferTooSmall occurred more than 3 times, resetting map_key to 0.");
                map_key = 0; // Reset map_key
                // Continue the loop to try again with the reset map_key
            } else {
                (bs.free_pages)(map_phys_addr, map_pages);
                println!("Error: Failed to get memory map after multiple attempts.");
                return Err(BellowsError::InvalidState("Failed to get memory map after multiple attempts."));
            }
        }

        let status = (bs.get_memory_map)(
            &mut map_size,
            map_phys_addr as *mut c_void, // Use the allocated buffer
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );

        match EfiStatus::from(status) {
            EfiStatus::Success => break, // Successfully got the memory map
            EfiStatus::BufferTooSmall => {
                // Memory map changed, re-allocate a larger buffer
                println!("Memory map changed, re-allocating buffer.");
                let new_alloc_size = map_size.saturating_add(4096);
                let new_pages = new_alloc_size.div_ceil(4096);
                let mut new_phys = 0;
                let new_status = (bs.allocate_pages)(
                    0usize,
                    EfiMemoryType::EfiLoaderData,
                    new_pages,
                    &mut new_phys,
                );
                if EfiStatus::from(new_status) != EfiStatus::Success {
                    (bs.free_pages)(map_phys_addr, map_pages);
                    println!("Error: Failed to re-allocate memory map buffer: {:?}", EfiStatus::from(new_status));
                    return Err(BellowsError::AllocationFailed("Failed to re-allocate memory map buffer."));
                }
                (bs.free_pages)(map_phys_addr, map_pages); // Free old buffer
                map_phys_addr = new_phys;
                map_pages = new_pages;
            }
            _ => {
                (bs.free_pages)(map_phys_addr, map_pages);
                println!("Error: Failed to get memory map: {:?}", EfiStatus::from(status));
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    // Exit boot services. This call must succeed.
    let exit_status = (bs.exit_boot_services)(image_handle, map_key);
    if EfiStatus::from(exit_status) != EfiStatus::Success {
        println!("Error: Failed to exit boot services: {:?}", EfiStatus::from(exit_status));
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
    entry(image_handle, system_table, map_ptr, map_size);
}
