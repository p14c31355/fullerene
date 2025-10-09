use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EfiMemoryType, EfiStatus, EfiSystemTable,
};
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
    #[cfg(feature = "debug_loader")]
    {
        debug_print_str("Inside exit_boot_services_and_jump.\n");
        debug_print_str("system_table = ");
        debug_print_hex(system_table as usize);
        debug_print_str("\n");
    }
    let bs = unsafe { &*(*system_table).boot_services };
    #[cfg(feature = "debug_loader")]
    {
        debug_print_str("bs obtained.\n");
        debug_print_str("About to set up memory map vars.\n");
    }

    // Initial setup for memory map
    // Start with a reasonable initial size to avoid EFI_INVALID_PARAMETER on some UEFI implementations
    let mut map_size: usize = 16 * 4096; // 64 KiB initial guess
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;
    let mut map_phys_addr: usize = 0;
    let mut map_pages: usize = 0;

    // Loop to get the memory map until successful
    // Since some UEFI implementations don't handle null buffers well, allocate directly
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 5;

    loop {
        if attempts >= MAX_ATTEMPTS {
            return Err(BellowsError::InvalidState(
                "Too many attempts to get memory map.",
            ));
        }
        attempts += 1;
        #[cfg(feature = "debug_loader")] {
            debug_print_str("Loop start, attempts=");
            debug_print_hex(attempts);
            debug_print_str(", map_size=");
            debug_print_hex(map_size);
            debug_print_str("\n");
        }

        // Allocate buffer for current map_size
        let alloc_pages = (map_size as usize).div_ceil(4096).max(1);
        let mut new_map_phys_addr: usize = 0;

        let alloc_status = (bs.allocate_pages)(
            0usize, // AllocateAnyPages
            EfiMemoryType::EfiLoaderData,
            alloc_pages,
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
        map_pages = alloc_pages;

        // Call get_memory_map with the allocated buffer
        #[cfg(feature = "debug_loader")] {
            debug_print_str("About to call get_memory_map (with buffer)\n");
        }
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                map_phys_addr as *mut c_void,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };
        #[cfg(feature = "debug_loader")] {
            debug_print_str("get_memory_map returned, status=");
            debug_print_hex(status as usize);
            debug_print_str("\n");
        }

        match EfiStatus::from(status) {
            EfiStatus::Success => {
                #[cfg(feature = "debug_loader")] {
                    debug_print_str("Memory map acquired after ");
                    debug_print_hex(attempts);
                    debug_print_str(" attempts. Size: ");
                    debug_print_hex(map_size);
                    debug_print_str(", map_key: ");
                    debug_print_hex(map_key);
                    debug_print_str("\n");
                }
                break;
            }
            EfiStatus::BufferTooSmall => {
                #[cfg(feature = "debug_loader")] {
                    debug_print_str("Buffer too small (size now ");
                    debug_print_hex(map_size);
                    debug_print_str("), retrying...\n");
                }
                // Continue with enlarged map_size (updated by the call)
                continue;
            }
            _ => {
                (bs.free_pages)(map_phys_addr, map_pages);
                #[cfg(feature = "debug_loader")] {
                    debug_print_str("Error: Failed to get memory map: status=");
                    debug_print_hex(status);
                    debug_print_str("\n");
                }
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    let map_ptr = map_phys_addr as *mut c_void;

    let exit_status = (bs.exit_boot_services)(image_handle, map_key);
    match EfiStatus::from(exit_status) {
        EfiStatus::Success => {
            #[cfg(feature = "debug_loader")] {
                debug_print_str("Exit boot services succeeded.\n");
                debug_print_str("About to jump.\n");
            }
        }
        _ => {
            let _ = (bs.free_pages)(map_phys_addr, map_pages);
            #[cfg(feature = "debug_loader")] {
                debug_print_str("Error: Failed to exit boot services: status=");
                debug_print_hex(exit_status);
                debug_print_str("\n");
            }
            return Err(BellowsError::InvalidState("Failed to exit boot services."));
        }
    }

    // Note: The memory map buffer at `map_phys_addr` is intentionally not freed here
    // because after `exit_boot_services` is called, the boot services are no longer
    // available to the bootloader, making `bs.free_pages` an invalid call.

    // Jump to the kernel. This is the last instruction in the bootloader.
    // Safety:
    // This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    #[cfg(feature = "debug_loader")] {
        debug_print_str("Jumping to kernel at ");
        debug_print_hex(entry as usize);
        debug_print_str(" with map at ");
        debug_print_hex(map_phys_addr);
        debug_print_str(" size ");
        debug_print_hex(map_size);
        debug_print_str("\n");
        debug_print_str("About to call kernel entry.\n");
    }
    entry(image_handle, system_table, map_ptr, map_size);
}
