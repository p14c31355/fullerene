// bellows/src/loader/mod.rs

use crate::uefi::{EfiMemoryType, EfiSystemTable, Result};
use core::ffi::c_void;
use core::{ptr};

pub mod file;
pub mod heap;
pub mod pe;

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

    let status = (unsafe {
        (bs.get_memory_map)(
            &mut map_size,
            ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    });
    if status != 0 {
        return Err("Failed to get memory map size.");
    }

    map_size += 4096;
    let map_pages = map_size.div_ceil(4096);
    let mut map_phys_addr: usize = 0;
    let status = (unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            map_pages,
            &mut map_phys_addr,
        )
    });
    if status != 0 {
        return Err("Failed to allocate memory map buffer.");
    }

    let map_ptr = map_phys_addr as *mut c_void;
    let status = (unsafe {
        (bs.get_memory_map)(
            &mut map_size,
            map_ptr,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    });
    if status != 0 {
        (unsafe {
            (bs.free_pages)(map_phys_addr, map_pages)
        });
        return Err("Failed to get memory map on second attempt.");
    }

    let status = (unsafe { (bs.exit_boot_services)(image_handle, map_key) });
    if status != 0 {
        (unsafe {
            (bs.free_pages)(map_phys_addr, map_pages)
        });
        return Err("Failed to exit boot services.");
    }
    entry(image_handle, system_table, map_ptr, map_size);
}
