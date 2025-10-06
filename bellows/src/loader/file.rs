// bellows/src/loader/file.rs

use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EFI_FILE_INFO_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EfiBootServices, EfiFile, EfiFileInfo,
    EfiLoadedImageProtocol, EfiSimpleFileSystem, EfiStatus,
};

use petroleum;

const EFI_FILE_MODE_READ: u64 = 0x1;
const KERNEL_PATH: &str = r"\EFI\BOOT\KERNEL.EFI";

/// Fixed UTF-16 encode for KERNEL_PATH (no alloc).
fn kernel_path_utf16() -> [u16; 32] {
    // Enough for path + null
    let path = KERNEL_PATH.encode_utf16().chain(core::iter::once(0u16));
    let mut buf = [0u16; 32];
    let mut i = 0;
    for c in path {
        if i < buf.len() - 1 {
            buf[i] = c;
            i += 1;
        } else {
            break;
        }
    }
    buf[i] = 0; // Ensure null-term
    buf
}

/// A RAII wrapper for EfiFile that automatically closes the file when it goes out of scope.
struct EfiFileWrapper {
    file: *mut EfiFile,
}

impl EfiFileWrapper {
    fn new(file: *mut EfiFile) -> Self {
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
fn open_file(dir: &EfiFileWrapper, path: &[u16]) -> petroleum::common::Result<EfiFileWrapper> {
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
        return Err(BellowsError::FileIo("Failed to open file."));
    }
    Ok(EfiFileWrapper::new(file_handle))
}

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<(usize, usize)> {
    // Get the device handle from the LoadedImageProtocol
    let mut loaded_image: *mut EfiLoadedImageProtocol = ptr::null_mut();
    let status = (bs.open_protocol)(
        image_handle,
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        &mut loaded_image as *mut _ as *mut *mut c_void,
        image_handle,
        0,
        1, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
    );
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::ProtocolNotFound(
            "Failed to get LoadedImageProtocol.",
        ));
    }
    let device_handle = unsafe { (*loaded_image).device_handle };

    // Get the SimpleFileSystem protocol from the device
    let mut fs_proto: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.open_protocol)(
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        &mut fs_proto as *mut _ as *mut *mut c_void,
        image_handle, // agent_handle
        0,            // controller_handle
        1,            // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
    );
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::ProtocolNotFound(
            "Failed to get SimpleFileSystem protocol from device.",
        ));
    }

    let mut volume_file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs_proto).open_volume)(fs_proto, &mut volume_file_handle) };
    let volume = match EfiStatus::from(status) {
        EfiStatus::Success => EfiFileWrapper::new(volume_file_handle),
        _ => {
            return Err(BellowsError::FileIo(
                "Failed to open EFI SimpleFileSystem protocol volume.",
            ));
        }
    };

    // Correct file name to match the kernel file
    let file = open_file(
        &volume,
        &kernel_path_utf16()[..], // Fixed slice
    )?;

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
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }

    if file_info_buffer_size == 0 {
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }

    // Use a page-sized buffer on the stack, which is safer for variable-sized info.
    let mut file_info_buffer = [0u8; 4096];
    if file_info_buffer_size > file_info_buffer.len() {
        return Err(BellowsError::FileIo("File info buffer too large."));
    }

    let status = unsafe {
        ((*file.file).get_info)(
            file.file,
            &EFI_FILE_INFO_GUID as *const _ as *const u8,
            &mut file_info_buffer_size,
            file_info_buffer.as_mut_ptr() as *mut c_void,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::FileIo("Failed to get file info."));
    }
    // Safety:
    // The size of the buffer is checked against the required size.
    // The pointer is checked to be non-null and correctly aligned before dereferencing.
    let file_info: &EfiFileInfo =
        unsafe { &*(file_info_buffer.as_mut_ptr() as *const EfiFileInfo) };
    let file_size = file_info.file_size as usize;

    if file_size == 0 {
        return Err(BellowsError::FileIo("Kernel file is empty."));
    }

    let pages = file_size.div_ceil(4096);
    let mut phys_addr: usize = 0;

    let status = {
        (bs.allocate_pages)(
            0usize,
            petroleum::common::EfiMemoryType::EfiLoaderData,
            pages,
            &mut phys_addr,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate pages for kernel file.",
        ));
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*file.file).read)(file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        // It's important to free the allocated pages on failure to avoid memory leaks.
        (bs.free_pages)(phys_addr, pages);
        return Err(BellowsError::FileIo(
            "Failed to read kernel file or read size mismatch.",
        ));
    }
    Ok((phys_addr, file_size))
}
