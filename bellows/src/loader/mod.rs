// bellows/src/loader/mod.rs

use crate::uefi::{EfiMemoryType, EfiSystemTable, Result};
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

    // First call to GetMemoryMap to get buffer size.
    let status = unsafe {
        (bs.get_memory_map)(
            &mut map_size,
            ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };
    if status != 0 {
        return Err("Failed to get memory map size on first attempt.");
    }

    // Add extra space just in case the memory map changes between calls.
    map_size += 4096;
    let mut map_pages = map_size.div_ceil(4096);
    let mut map_phys_addr: usize = 0;

    // Allocate an appropriately-sized buffer for the memory map.
    let status = unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            map_pages,
            &mut map_phys_addr,
        )
    };
    if status != 0 {
        return Err("Failed to allocate memory map buffer.");
    }
    let map_ptr = map_phys_addr as *mut c_void;

    // Retry GetMemoryMap in a loop to handle `EFI_BUFFER_TOO_SMALL`
    loop {
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                map_ptr,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };
        if status == 0 {
            break;
        } else if status == 0x8000000000000005 {
            // EFI_BUFFER_TOO_SMALL
            // The memory map has changed. We need to free the old buffer, re-allocate a larger one, and try again.
            unsafe {
                (bs.free_pages)(map_phys_addr, map_pages);
            }
            map_size += descriptor_size; // Increase buffer size
            let new_map_pages = map_size.div_ceil(4096);
            let mut new_map_phys_addr: usize = 0;
            let new_status = unsafe {
                (bs.allocate_pages)(
                    0usize,
                    EfiMemoryType::EfiLoaderData,
                    new_map_pages,
                    &mut new_map_phys_addr,
                )
            };
            if new_status != 0 {
                return Err("Failed to re-allocate memory map buffer.");
            }
            map_phys_addr = new_map_phys_addr;
            map_pages = new_map_pages; // Update map_pages with the new size
            continue;
        } else {
            // Unexpected error status
            unsafe {
                (bs.free_pages)(map_phys_addr, map_pages);
            }
            return Err("Failed to get memory map after multiple attempts.");
        }
    }

    // Exit boot services. This call must succeed.
    let exit_status = unsafe { (bs.exit_boot_services)(image_handle, map_key) };
    if exit_status != 0 {
        // If this fails, there's no way to recover.
        return Err("Failed to exit boot services.");
    }

    // Jump to the kernel. This is the last instruction in the bootloader.
    // Safety:
    // This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    unsafe {
        entry(image_handle, system_table, map_ptr, map_size);
    }
}
