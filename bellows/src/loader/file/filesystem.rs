use core::ffi::c_void;
use core::ptr;
use petroleum::common::{BellowsError, EFI_FILE_INFO_GUID, EfiBootServices, EfiFile, EfiFileInfo, EfiStatus};
use super::super::debug::*;

const EFI_FILE_MODE_READ: u64 = 0x1;
const KERNEL_PATH: &str = r"EFI\BOOT\KERNEL.EFI";

/// Fixed UTF-16 encode for KERNEL_PATH (no alloc).
pub fn kernel_path_utf16() -> [u16; 32] {
    let mut buf = [0u16; 32];
    let mut i = 0;
    // KERNEL_PATH is a constant, so we can rely on its length being less than 31.
    for c in KERNEL_PATH.encode_utf16() {
        if i >= buf.len() - 1 {
            break; // Path too long, should not happen for a constant
        }
        buf[i] = c;
        i += 1;
    }
    // The rest of the buffer is zero-initialized, so buf[i] is already 0.
    buf
}

/// A RAII wrapper for EfiFile that automatically closes the file when it goes out of scope.
pub struct EfiFileWrapper {
    file: *mut EfiFile,
}

impl EfiFileWrapper {
    pub fn new(file: *mut EfiFile) -> Self {
        Self { file }
    }
}

impl Drop for EfiFileWrapper {
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

/// Helper function to open a file from a directory handle.
pub fn open_file(dir: &EfiFileWrapper, path: &[u16]) -> petroleum::common::Result<EfiFileWrapper> {
    let mut file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe {
        ((*dir.file).open)(
            dir.file,
            &mut file_handle,
            path.as_ptr(),
            EFI_FILE_MODE_READ,
            0,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        debug_print_str("File: Failed to open file.\n");
        return Err(BellowsError::FileIo("Failed to open file."));
    }
    debug_print_str("File: Opened file.\n");
    Ok(EfiFileWrapper::new(file_handle))
}

/// Read file content into memory
pub fn read_file_to_memory(
    bs: &EfiBootServices,
    file: &EfiFileWrapper,
) -> petroleum::common::Result<(usize, usize)> {
    let mut file_info_buffer_size = 0;

    let status = unsafe {
        ((*file.file).get_info)(
            file.file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_buffer_size,
            ptr::null_mut(),
        )
    };
    if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
        debug_print_str("File: Failed to get file info size.\n");
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }
    if file_info_buffer_size == 0 {
        debug_print_str("File: File info size is 0.\n");
        return Err(BellowsError::FileIo("File info size is 0."));
    }

    let mut file_info_buffer = alloc::vec![0u8; file_info_buffer_size];

    let status = unsafe {
        ((*file.file).get_info)(
            file.file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_buffer_size,
            file_info_buffer.as_mut_ptr() as *mut c_void,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        debug_print_str("File: Failed to get file info.\n");
        return Err(BellowsError::FileIo("Failed to get file info."));
    }

    let file_info: &EfiFileInfo = unsafe { &*(file_info_buffer.as_mut_ptr() as *const EfiFileInfo) };
    let file_size = file_info.file_size as usize;

    if file_size == 0 {
        return Err(BellowsError::FileIo("Kernel file is empty."));
    }

    let pages = file_size.div_ceil(4096);
    let mut phys_addr: usize = 0;

    let status = (bs.allocate_pages)(
        0usize,
        petroleum::common::EfiMemoryType::EfiLoaderData,
        pages,
        &mut phys_addr,
    );
    if EfiStatus::from(status) != EfiStatus::Success {
        debug_print_str("File: Failed to allocate pages.\n");
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate pages for kernel file.",
        ));
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*file.file).read)(file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        (bs.free_pages)(phys_addr, pages);
        debug_print_str("File: Failed to read file.\n");
        return Err(BellowsError::FileIo(
            "Failed to read kernel file or read size mismatch.",
        ));
    }

    Ok((phys_addr, file_size))
}
