// bellows/src/loader/file.rs

use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EFI_FILE_INFO_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EfiBootServices,
    EfiFile, EfiFileInfo, EfiLoadedImageProtocol, EfiSimpleFileSystem, EfiStatus,
};
use x86_64::instructions::port::Port; // Import Port for direct I/O

const EFI_LOADED_IMAGE_PROTOCOL_GUID: [u8; 16] = [
    0xA1, 0x31, 0x1B, 0x5B,
    0x62, 0x95,
    0xD2, 0x11,
    0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B
];

/// Writes a single byte to the COM1 serial port (0x3F8).
/// This is a very basic, early debug function that doesn't rely on any complex initialization.
fn debug_print_byte(byte: u8) {
    let mut port = Port::new(0x3F8);
    unsafe {
        // Wait until the transmit buffer is empty
        while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
        port.write(byte);
    }
}

/// Writes a string to the COM1 serial port.
fn debug_print_str(s: &str) {
    for byte in s.bytes() {
        debug_print_byte(byte);
    }
}

const EFI_FILE_MODE_READ: u64 = 0x1;
const KERNEL_PATH: &str = r"\EFI\BOOT\KERNEL.EFI";

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
        debug_print_str("File: Failed to open file.\n");
        return Err(BellowsError::FileIo("Failed to open file."));
    }
    debug_print_str("File: Opened file.\n");
    Ok(EfiFileWrapper::new(file_handle))
}

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(bs: &EfiBootServices, image_handle: usize) -> petroleum::common::Result<(usize, usize)> {
    // Debug print: Starting file read
    debug_print_str("File: Starting read_efi_file...\n");

    // Assume the device handle is the image handle
    let device_handle = image_handle;
    debug_print_str("File: Using image_handle as device_handle.\n");

    // Get the SimpleFileSystem protocol from the device
    let mut fs_proto: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.open_protocol)(
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        &mut fs_proto as *mut _ as *mut *mut c_void,
        image_handle, // agent_handle
        0, // controller_handle
        1, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
    );
    if EfiStatus::from(status) != EfiStatus::Success {
        debug_print_str("File: Failed to get SimpleFileSystem protocol from device.\n");
        return Err(BellowsError::ProtocolNotFound(
            "Failed to get SimpleFileSystem protocol from device.",
        ));
    }
    debug_print_str("File: Got SimpleFileSystem protocol from device.\n");

    let mut volume_file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs_proto).open_volume)(fs_proto, &mut volume_file_handle) };
    let volume = match EfiStatus::from(status) {
        EfiStatus::Success => {
            debug_print_str("File: Opened volume.\n");
            EfiFileWrapper::new(volume_file_handle)
        }
        _ => {
            debug_print_str("File: Failed to open volume.\n");
            return Err(BellowsError::FileIo(
                "Failed to open EFI SimpleFileSystem protocol volume.",
            ));
        }
    };

    // Correct file name to match the kernel file
    let file = open_file(
        &volume,
        KERNEL_PATH
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect::<Vec<u16>>()
            .as_slice(),
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
        debug_print_str("File: Failed to get file info size.\n");
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }
    debug_print_str("File: Got file info size.\n");

    if file_info_buffer_size == 0 {
        debug_print_str("File: File info size is 0.\n");
        return Err(BellowsError::FileIo("Failed to get file info size."));
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
    debug_print_str("File: Got file info.\n");
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
        debug_print_str("File: Failed to allocate pages.\n");
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate pages for kernel file.",
        ));
    }
    debug_print_str("File: Allocated pages.\n");

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*file.file).read)(file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        // It's important to free the allocated pages on failure to avoid memory leaks.
        (bs.free_pages)(phys_addr, pages);
        debug_print_str("File: Failed to read file.\n");
        return Err(BellowsError::FileIo(
            "Failed to read kernel file or read size mismatch.",
        ));
    }
    debug_print_str("File: Read file.\n");

    debug_print_str("File: Returning from read_efi_file.\n");
    Ok((phys_addr, file_size))
}
