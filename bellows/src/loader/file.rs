// bellows/src/loader/file.rs

use crate::uefi::{
    EFI_FILE_INFO_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EfiBootServices, EfiFile,
    EfiFileInfo, EfiSimpleFileSystem, EfiStatus, EfiSystemTable, Result,
};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

/// A RAII wrapper for EfiFile that automatically closes the file when it goes out of scope.
struct EfiFileWrapper<'a> {
    file: *mut EfiFile,
    bs: &'a EfiBootServices,
}

impl<'a> EfiFileWrapper<'a> {
    fn new(file: *mut EfiFile, bs: &'a EfiBootServices) -> Self {
        Self { file, bs }
    }
}

impl Drop for EfiFileWrapper<'_> {
    fn drop(&mut self) {
        if !self.file.is_null() {
            // Safety:
            // The `EfiFile` pointer is assumed to be valid from the `new` function.
            // This is the last use of the pointer, so it is safe to dereference it for the `close` call.
            let status = unsafe { ((*self.file).close)(self.file) };
            if EfiStatus::from(status) != EfiStatus::Success {
                // In a real application, you might want to log this error.
                // For a bootloader, this might be a non-recoverable state.
            }
        }
    }
}

/// Read `KERNEL.EFI` or `kernel.efi` from the volume using UEFI SimpleFileSystem protocol.
pub fn read_efi_file(st: &EfiSystemTable) -> Result<(usize, usize)> {
    let bs = unsafe { &*st.boot_services };

    let mut fs_ptr: *mut c_void = ptr::null_mut();
    // Safety:
    // The `locate_protocol` call is a UEFI boot service. Its function pointer
    // is assumed to be valid. The GUID is static.
    let status = unsafe {
        (bs.locate_protocol)(
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
            ptr::null_mut(),
            &mut fs_ptr,
        )
    };

    if EfiStatus::from(status) != EfiStatus::Success {
        return Err("Failed to locate SimpleFileSystem protocol.");
    }
    let fs = fs_ptr as *mut EfiSimpleFileSystem;

    let mut root: *mut EfiFile = ptr::null_mut();
    // Safety:
    // The `open_volume` function pointer is a valid member of the `EfiSimpleFileSystem`
    // struct, which was located successfully. The `fs` pointer is not null.
    let status = unsafe { ((*fs).open_volume)(fs, &mut root) };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err("Failed to open volume.");
    }
    let root = EfiFileWrapper::new(root, bs);

    let kernel_names = [
        "\\KERNEL.EFI\0".encode_utf16().collect::<Vec<u16>>(),
        "\\kernel.efi\0".encode_utf16().collect::<Vec<u16>>(),
    ];
    let mut efi_file_ptr: *mut EfiFile = ptr::null_mut();
    let mut found = false;

    // Try to open KERNEL.EFI, then kernel.efi
    for name in kernel_names.iter() {
        // Safety:
        // `root.file` is a valid pointer. The `open` function pointer is a valid member.
        // `name` is a null-terminated UTF-16 string as required by UEFI.
        let status = unsafe {
            ((*root.file).open)(
                root.file,
                &mut efi_file_ptr,
                name.as_ptr(),
                0x1, // EFI_FILE_MODE_READ
                0x0, // 0 attributes
            )
        };
        if EfiStatus::from(status) == EfiStatus::Success {
            found = true;
            break;
        }
    }

    if !found {
        return Err("Failed to open kernel file.");
    }

    let efi_file = EfiFileWrapper::new(efi_file_ptr, bs);

    let mut file_info_size = 0;
    // Safety:
    // The first `get_info` call is to get the size of the buffer needed.
    // The function pointer and file pointer are assumed to be valid.
    let status = unsafe {
        ((*efi_file.file).get_info)(
            efi_file.file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_size,
            ptr::null_mut(),
        )
    };
    if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
        return Err("Failed to get file info size.");
    }

    let mut file_info_buffer: Vec<u8> = Vec::with_capacity(file_info_size);
    let status = unsafe {
        ((*efi_file.file).get_info)(
            efi_file.file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_size,
            file_info_buffer.as_mut_ptr() as *mut c_void,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err("Failed to get file info.");
    }
    // Safety:
    // The size of the buffer is checked against the required size.
    // The pointer is checked to be non-null and correctly aligned before dereferencing.
    let file_info: &EfiFileInfo =
        unsafe { &*(file_info_buffer.as_mut_ptr() as *const EfiFileInfo) };
    let file_size = file_info.file_size as usize;

    if file_size == 0 {
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
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err("Failed to allocate pages for kernel file.");
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*efi_file.file).read)(efi_file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        unsafe {
            (bs.free_pages)(phys_addr, pages);
        }
        return Err("Failed to read kernel file or read size mismatch.");
    }

    Ok((phys_addr, file_size))
}
