// bellows/src/loader/file.rs

use crate::uefi::{
    EFI_FILE_INFO_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EfiFile, EfiFileInfo,
    EfiSimpleFileSystem, EfiSystemTable, Result,
};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

/// Read `KERNEL.EFI` or `kernel.efi` from the volume using UEFI SimpleFileSystem protocol.
pub fn read_efi_file(st: &EfiSystemTable) -> Result<(usize, usize)> {
    let bs = unsafe { &*st.boot_services };

    let mut fs_ptr: *mut c_void = ptr::null_mut();
    // Safety:
    // The `locate_protocol` call is a UEFI boot service. Its function pointer
    // is assumed to be valid. The GUID is static.
    let status = (bs.locate_protocol)(
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut fs_ptr,
    );
    if status != 0 {
        return Err("Failed to locate SimpleFileSystem protocol.");
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    let mut root: *mut EfiFile = ptr::null_mut();
    // Safety:
    // The `open_volume` function pointer is a valid member of the `EfiSimpleFileSystem`
    // struct, which was located successfully. The `fs` pointer is not null.
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
        // Safety:
        // `root` is a valid file pointer. The `open` function pointer is valid.
        // The `file_name` pointer is valid and points to a null-terminated UTF-16 string.
        if unsafe { ((*root).open)(root, &mut efi_file, file_name.as_ptr(), 0x1, 0) } == 0 {
            found = true;
            break;
        }
    }

    // Safety:
    // If the file was not found, we must close the root handle.
    if !found {
        unsafe { ((*root).close)(root) };
        return Err("Failed to open KERNEL.EFI or kernel.efi.");
    }

    // Safety:
    // `efi_file` is now a valid pointer. The `get_info` function pointer is valid.
    // We are calling it with a null buffer to get the required size first.
    let mut file_info_size: usize = 0;
    let mut status = unsafe {
        ((*efi_file).get_info)(
            efi_file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_size,
            ptr::null_mut(),
        )
    };

    if status != crate::uefi::EFI_BUFFER_TOO_SMALL {
        unsafe {
            ((*efi_file).close)(efi_file);
            ((*root).close)(root);
        }
        return Err("Get file info failed on first attempt.");
    }

    let mut file_info_buf: Vec<u8> = Vec::with_capacity(file_info_size);
    unsafe { file_info_buf.set_len(file_info_size) };

    // Safety:
    // We have a buffer with the correct capacity. `as_mut_ptr` is safe.
    let file_info_ptr = file_info_buf.as_mut_ptr() as *mut c_void;
    status = unsafe {
        ((*efi_file).get_info)(
            efi_file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_size,
            file_info_ptr,
        )
    };

    if status != 0 {
        unsafe {
            ((*efi_file).close)(efi_file);
            ((*root).close)(root);
        }
        return Err("Failed to get file info.");
    }

    // Safety:
    // The buffer is now populated with valid `EfiFileInfo` data.
    // The pointer is checked to be non-null and correctly aligned before dereferencing.
    let file_info: &EfiFileInfo = unsafe { &*(file_info_ptr as *const EfiFileInfo) };
    let file_size = file_info.file_size as usize;

    if file_size == 0 {
        unsafe {
            ((*efi_file).close)(efi_file);
            ((*root).close)(root);
        }
        return Err("Kernel file is empty.");
    }

    let pages = file_size.div_ceil(4096);
    let mut phys_addr: usize = 0;

    let status = {
        (bs.allocate_pages)(
            0usize,
            crate::uefi::EfiMemoryType::EfiLoaderData,
            pages,
            &mut phys_addr,
        )
    };
    if status != 0 {
        unsafe {
            ((*efi_file).close)(efi_file);
            ((*root).close)(root);
        }
        return Err("Failed to allocate pages for kernel file.");
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*efi_file).read)(efi_file, &mut read_size, buf_ptr) };
    if status != 0 || read_size as usize != file_size {
        unsafe {
            (bs.free_pages)(phys_addr, pages);
            ((*efi_file).close)(efi_file);
            ((*root).close)(root);
        }
        return Err("Failed to read kernel file completely.");
    }

    // Clean up: close the file and the root directory.
    unsafe {
        ((*efi_file).close)(efi_file);
        ((*root).close)(root);
    }

    Ok((phys_addr, read_size as usize))
}
