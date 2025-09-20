// bellows/src/loader/mod.rs

use crate::uefi::{
    BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable, Result,
};
use core::ffi::c_void;
use core::ptr;

pub mod file;
pub mod heap;
pub mod pe;

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
) -> Result<!> {
    let bs = unsafe { &*(*system_table).boot_services };

    let mut map_size = 0;
    let mut map_key = 0;
    let mut descriptor_size = 0;
    let mut descriptor_version = 0;

    // Use a loop to handle the case where the memory map changes between calls.
    // This is a common and recommended UEFI pattern.
    let mut map_phys_addr = 0;
    let mut map_pages = 0;
    let mut attempts = 0;

    loop {
        attempts += 1;
        if attempts > 3 {
            // Safety:
(bs.free_pages)(map_phys_addr, map_pages);
            return Err(BellowsError::InvalidState(
                "Failed to get memory map after multiple attempts.",
            ));
        }

        // First call to GetMemoryMap to get the required buffer size.
        let status = (bs.get_memory_map)(
            &mut map_size,
            ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );

        // If the buffer was too small, allocate a larger one and try again.
        if EfiStatus::from(status) == EfiStatus::BufferTooSmall {
            // Add extra space just in case the memory map changes.
            let new_map_size = map_size.saturating_add(4096);
            let new_map_pages = new_map_size.div_ceil(4096);
            let mut new_map_phys_addr = 0;
            let new_status = (bs.allocate_pages)(
                0usize,
                EfiMemoryType::EfiLoaderData,
                new_map_pages,
                &mut new_map_phys_addr,
            );

            if EfiStatus::from(new_status) != EfiStatus::Success {
                // If allocation fails, free any previously allocated memory.
                if map_phys_addr != 0 {
        (bs.free_pages)(map_phys_addr, map_pages);
                }
                return Err(BellowsError::AllocationFailed(
                    "Failed to re-allocate memory map buffer.",
                ));
            }

            // Free the old buffer before re-assigning.
            if map_phys_addr != 0 {
                (bs.free_pages)(map_phys_addr, map_pages);
            }
            map_phys_addr = new_map_phys_addr;
            map_pages = new_map_pages;
        } else if EfiStatus::from(status) == EfiStatus::Success {
            // Memory map size is now correct, proceed to exit boot services.
            break;
        } else {
            return Err(BellowsError::InvalidState(
                "Failed to get memory map size on first attempt.",
            ));
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    // Second call to GetMemoryMap, this time with the allocated buffer.
    // This call must succeed as we allocated a large enough buffer.
    let status = (bs.get_memory_map)(
        &mut map_size,
        map_ptr,
        &mut map_key,
        &mut descriptor_size,
        &mut descriptor_version,
    );

    if EfiStatus::from(status) != EfiStatus::Success {
        // If this fails, the system state is corrupted. We cannot use boot services to recover.
        return Err(BellowsError::InvalidState(
            "Failed to get memory map with allocated buffer.",
        ));
    }

    // Exit boot services. This call must succeed.
    let exit_status = (bs.exit_boot_services)(image_handle, map_key);
    if EfiStatus::from(exit_status) != EfiStatus::Success {
        // If this fails, there's no way to recover.
        // We cannot use the UEFI boot services anymore.
        // The bootloader is in a failed state.
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
