// bellows/src/loader/file.rs

use crate::uefi::{EfiFile, EfiFileInfo, EfiSimpleFileSystem, EfiSystemTable, Result, EFI_FILE_INFO_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID};
use core::ffi::c_void;
use core::ptr;
use alloc::vec::Vec;

/// Read `KERNEL.EFI` or `kernel.efi` from the volume using UEFI SimpleFileSystem protocol.
pub fn read_efi_file(st: &EfiSystemTable) -> Result<(usize, usize)> {
    let bs = unsafe { &*st.boot_services };

    let mut fs_ptr: *mut c_void = ptr::null_mut();
    if unsafe { (bs.locate_protocol)(
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut fs_ptr,
    ) } != 0 {
        return Err("Failed to locate SimpleFileSystem protocol.");
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    let mut root: *mut EfiFile = ptr::null_mut();
    if unsafe { ((*fs).open_volume)(fs, &mut root) } != 0 {
        return Err("Failed to open volume.");
    }

    let file_names = [
        "KERNEL.EFI\0".encode_utf16().collect::<Vec<u16>>(),
        "kernel.efi\0".encode_utf16().collect::<Vec<u16>>(),
    ];
    let mut efi_file: *mut EfiFile = ptr::null_mut();
    let mut found = false;
    for file_name in &file_names {
        if unsafe { ((*root).open)(root, &mut efi_file, file_name.as_ptr(), 0x1, 0) } == 0 {
            found = true;
            break;
        }
    }
    if !found {
        return Err("Failed to open KERNEL.EFI or kernel.efi.");
    }

    let mut file_info_size: usize = 0;
    unsafe { ((*efi_file).get_info)(efi_file, EFI_FILE_INFO_GUID.as_ptr(), &mut file_info_size, ptr::null_mut()) };

    let mut file_info_buf: Vec<u8> = Vec::with_capacity(file_info_size);
    let file_info_ptr = file_info_buf.as_mut_ptr() as *mut c_void;
    if unsafe { ((*efi_file).get_info)(efi_file, EFI_FILE_INFO_GUID.as_ptr(), &mut file_info_size, file_info_ptr) } != 0 {
        unsafe { ((*efi_file).close)(efi_file) };
        return Err("Failed to get file info.");
    }
    let file_info: &EfiFileInfo = unsafe { &*(file_info_ptr as *const EfiFileInfo) };
    let file_size = file_info.file_size as usize;

    let pages = file_size.div_ceil(4096);
    let mut phys_addr: usize = 0;
    if unsafe { (bs.allocate_pages)(0usize, crate::uefi::EfiMemoryType::EfiLoaderData, pages, &mut phys_addr) } != 0 {
        unsafe { ((*efi_file).close)(efi_file) };
        return Err("Failed to allocate pages for kernel file.");
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size: u64 = file_size as u64;
    if unsafe { ((*efi_file).read)(efi_file, &mut read_size, buf_ptr) } != 0 {
        unsafe { (bs.free_pages)(phys_addr, pages); }
        unsafe { ((*efi_file).close)(efi_file) };
        return Err("Failed to read kernel file.");
    }

    unsafe { ((*efi_file).close)(efi_file) };
    Ok((phys_addr, read_size as usize))
}
