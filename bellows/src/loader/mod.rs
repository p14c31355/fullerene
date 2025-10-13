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
    // Immediate debug prints on entry to pinpoint exact hang location
    #[cfg(feature = "debug_loader")]
    {
        debug_print_str("ENTER\n");
        debug_print_str("system_table=");
        debug_print_hex(system_table as usize);
        debug_print_str("\n");
    }

    #[cfg(feature = "debug_loader")]
    debug_print_str("About to get boot_services ptr\n");
    let bs = unsafe { &*(*system_table).boot_services };
    #[cfg(feature = "debug_loader")]
    debug_print_str("Got boot_services ptr\n");

    #[cfg(feature = "debug_loader")]
    {
        debug_print_str("bs obtained.\n");
        debug_print_str("About to set up memory map vars.\n");
        debug_print_str("About to setup buffer vars\n");
    }
    // Pre-allocate buffer before loop to include it in map key
    let map_buffer_size: usize = 128 * 1024; // 128 KiB
    let alloc_pages = map_buffer_size.div_ceil(4096).max(1);
    #[cfg(feature = "debug_loader")]
    debug_print_str("Buffer vars setup\n");

    #[cfg(feature = "debug_loader")]
    {
        debug_print_str("About to allocate fixed map buffer\n");
    }

    let mut map_phys_addr: usize = 0;
    let alloc_status = (bs.allocate_pages)(
        0usize, // AllocateAnyPages
        EfiMemoryType::EfiLoaderData,
        alloc_pages,
        &mut map_phys_addr,
    );

    if EfiStatus::from(alloc_status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate memory map buffer.",
        ));
    }

    let map_ptr: *mut c_void = map_phys_addr as *mut c_void;

    // Setup variables for memory map
    let mut map_size: usize = map_buffer_size; // Start with full buffer size
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;

    // Loop to retry both get_memory_map and exit_boot_services until exit_boot_services succeeds
    // UEFI can make allocations between get_memory_map and exit_boot_services, causing map to become stale
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 10; // Allow more attempts since both calls may need to be retried

    loop {
        if attempts >= MAX_ATTEMPTS {
            let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup before returning error
            return Err(BellowsError::InvalidState(
                "Too many attempts to exit boot services.",
            ));
        }
        attempts += 1;

        #[cfg(feature = "debug_loader")]
        {
            debug_print_str("Combined loop, attempt ");
            debug_print_hex(attempts);
            debug_print_str("\n");
        }

        // Call get_memory_map with pre-allocated buffer
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                map_ptr,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };

        match EfiStatus::from(status) {
            EfiStatus::Success => {
                #[cfg(feature = "debug_loader")]
                {
                    debug_print_str("Memory map acquired successfully on attempt ");
                    debug_print_hex(attempts);
                    debug_print_str(", size=");
                    debug_print_hex(map_size);
                    debug_print_str(", key=");
                    debug_print_hex(map_key);
                    debug_print_str("\n");
                    debug_print_str("About to call exit_boot_services...\n");
                }

                // Immediately call exit_boot_services with the freshly acquired map_key
                let exit_status = (bs.exit_boot_services)(image_handle, map_key);

                match EfiStatus::from(exit_status) {
                    EfiStatus::Success => {
                        #[cfg(feature = "debug_loader")]
                        {
                            debug_print_str("Exit boot services succeeded on attempt ");
                            debug_print_hex(attempts);
                            debug_print_str(".\n");
                            debug_print_str("About to jump to kernel.\n");
                        }
                        break; // Success, exit the loop and proceed to kernel jump
                    }
                    EfiStatus::Unsupported => {
                        #[cfg(feature = "debug_loader")]
                        {
                            debug_print_str(
                                "exit_boot_services returned Unsupported, proceeding anyway\n",
                            );
                        }
                        break; // Proceed to jump to kernel
                    }
                    EfiStatus::InvalidParameter => {
                        #[cfg(feature = "debug_loader")]
                        {
                            debug_print_str(
                                "exit_boot_services returned InvalidParameter, retrying get_memory_map...\n",
                            );
                        }
                        // The map key is stale. Loop again to get a new memory map and key.
                        map_size = map_buffer_size;
                        continue;
                    }
                    _ => {
                        let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                        #[cfg(feature = "debug_loader")]
                        {
                            debug_print_str("Error: Failed to exit boot services: status=");
                            debug_print_hex(exit_status);
                            debug_print_str("\n");
                        }
                        return Err(BellowsError::InvalidState("Failed to exit boot services."));
                    }
                }
            }
            EfiStatus::BufferTooSmall => {
                #[cfg(feature = "debug_loader")]
                {
                    debug_print_str("Buffer too small, required size is now ");
                    debug_print_hex(map_size);
                    debug_print_str(" bytes\n");
                }
                // If our fixed buffer is too small, this is a fatal error.
                let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                petroleum::serial::_print(format_args!(
                    "Error: Memory map size {} exceeds fixed buffer capacity {}\n",
                    map_size, map_buffer_size
                ));
                return Err(BellowsError::InvalidState(
                    "Memory map too large for buffer.",
                ));
            }
            _ => {
                let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                #[cfg(feature = "debug_loader")]
                {
                    debug_print_str("Error: Failed to get memory map: status=");
                    debug_print_hex(status);
                    debug_print_str("\n");
                }
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    // Note: The memory map buffer at `map_phys_addr` is intentionally not freed here
    // because after `exit_boot_services` is called, the boot services are no longer
    // available to the bootloader, making `bs.free_pages` an invalid call.

    // Jump to the kernel. This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    #[cfg(feature = "debug_loader")]
    {
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
