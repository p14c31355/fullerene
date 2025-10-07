// bellows/src/loader/file.rs

use alloc::{format, vec::Vec};
use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EFI_FILE_INFO_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EfiBootServices, EfiFile, EfiFileInfo,
    EfiLoadedImageProtocol, EfiSimpleFileSystem, EfiStatus,
};

use super::debug::*;

// Macro to reduce repetitive debug prints
macro_rules! file_debug {
    ($msg:expr) => {
        debug_print_str(concat!("File: ", $msg, "\n"));
    };
}

// Helper function to open a protocol
fn open_protocol<T>(
    bs: &EfiBootServices,
    handle: usize,
    guid: *const u8,
    agent_handle: usize,
    attributes: u32,
) -> petroleum::common::Result<*mut T> {
    let mut proto: *mut T = ptr::null_mut();
    let status = (bs.open_protocol)(
        handle,
        guid,
        &mut proto as *mut _ as *mut *mut c_void,
        agent_handle,
        0,
        attributes,
    );
    if EfiStatus::from(status) != EfiStatus::Success {
        // It's useful to know which status was returned for debugging.
        debug_print_str("File: Failed to open protocol. Status: ");
        debug_print_hex(status);
        debug_print_str("\n");
        return Err(BellowsError::ProtocolNotFound("Failed to open protocol."));
    }
    file_debug!("Opened protocol.");
    Ok(proto)
}

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
        file_debug!("Failed to open file.");
        return Err(BellowsError::FileIo("Failed to open file."));
    }
    file_debug!("Opened file.");
    Ok(EfiFileWrapper::new(file_handle))
}

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<(usize, usize)> {
    file_debug!("Starting read_efi_file...");
    debug_print_str("File: image_handle=0x");
    debug_print_hex(image_handle);
    debug_print_str("\n");

    // Get the device handle from the LoadedImageProtocol
    let mut loaded_image: *mut EfiLoadedImageProtocol = ptr::null_mut();

    // Try handle_protocol first (preferred for LoadedImage)
    debug_print_str("File: Trying handle_protocol for LoadedImage...\n");
    let status_h = (bs.handle_protocol)(
        image_handle,
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        &mut loaded_image as *mut _ as *mut *mut c_void,
    );
    let status_h_efi = EfiStatus::from(status_h);
    debug_print_str("File: handle_protocol status=");
    debug_print_hex(status_h);
    debug_print_str(" (");
    match status_h_efi {
        EfiStatus::Success => debug_print_str("Success"),
        EfiStatus::InvalidParameter => debug_print_str("InvalidParameter"),
        _ => debug_print_str("Other"),
    }
    debug_print_str(")\n");

    if status_h_efi != EfiStatus::Success {
        // Fallback: try open_protocol
        debug_print_str("File: Trying open_protocol fallback...\n");
        let status = (bs.open_protocol)(
            image_handle,
            &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
            &mut loaded_image as *mut *mut _ as *mut *mut c_void,
            image_handle,
            0,
            1, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
        );
        let status_efi = EfiStatus::from(status);
        debug_print_str("File: open_protocol status=");
        debug_print_hex(status);
        debug_print_str(" (");
        match status_efi {
            EfiStatus::Success => debug_print_str("Success"),
            EfiStatus::InvalidParameter => debug_print_str("InvalidParameter"),
            _ => debug_print_str("Other"),
        }
        debug_print_str(")\n");
        if status_efi != EfiStatus::Success {
            return Err(BellowsError::ProtocolNotFound(
                "Both handle/open_protocol failed for LoadedImage.",
            ));
        }
    }

    if loaded_image.is_null() {
        return Err(BellowsError::ProtocolNotFound(
            "LoadedImage protocol is null.",
        ));
    }

    let loaded_image_ref = unsafe { &*loaded_image };
    file_debug!("Success getting LoadedImageProtocol.");
    let revision = loaded_image_ref.revision;
    debug_print_str("File: LoadedImageProtocol revision: ");
    debug_print_hex(revision as usize);
    debug_print_str("\n");
    let device_handle = loaded_image_ref.device_handle;
    debug_print_str("File: Got device_handle from LoadedImageProtocol. Handle: ");
    debug_print_hex(device_handle);
    debug_print_str("\n");

    // Try multiple methods to find SimpleFileSystem protocol
    let fs_proto: *mut EfiSimpleFileSystem = {
        // Try locate_protocol first
        debug_print_str("File: Trying locate_protocol for SimpleFileSystem...\n");
        let mut fs_proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
        let status = (bs.locate_protocol)(
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
            ptr::null_mut(),
            &mut fs_proto_ptr as *mut _ as *mut *mut c_void,
        );
        debug_print_str("File: locate_protocol status=");
        debug_print_hex(status);
        debug_print_str("\n");
        if EfiStatus::from(status) == EfiStatus::Success && !fs_proto_ptr.is_null() {
            debug_print_str("File: Got SimpleFileSystem via locate_protocol.\n");
            fs_proto_ptr
        } else {
            debug_print_str("File: locate_protocol failed, trying handle_protocol on device_handle.\n");
            let mut proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
            let status = (bs.handle_protocol)(
                device_handle,
                &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
                &mut proto_ptr as *mut _ as *mut *mut c_void,
            );
            debug_print_str("File: handle_protocol on device_handle status=");
            debug_print_hex(status);
            debug_print_str("\n");
            if EfiStatus::from(status) == EfiStatus::Success && !proto_ptr.is_null() {
                debug_print_str("File: Got SimpleFileSystem via handle_protocol on device_handle.\n");
                proto_ptr
            } else {
                debug_print_str("File: handle_protocol on device_handle failed, trying open_protocol.\n");
                let open_res = open_protocol::<EfiSimpleFileSystem>(
                    bs,
                    device_handle,
                    &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
                    image_handle,
                    2, // EFI_OPEN_PROTOCOL_GET_PROTOCOL
                );
                if let Ok(proto) = open_res {
                    debug_print_str("File: Got SimpleFileSystem on device_handle.\n");
                    proto
                } else {
                    debug_print_str("File: Open on device_handle failed, trying locate_handle_buffer.\n");
                    // Locate SimpleFileSystem handles
                    let mut handle_count = 0;
                    let mut handles: *mut usize = ptr::null_mut();
                    let status = (bs.locate_handle_buffer)(
                        2, // ByProtocol
                        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
                        ptr::null_mut(),
                        &mut handle_count,
                        &mut handles,
                    );
                    debug_print_str("File: locate_handle_buffer status=");
                    debug_print_hex(status);
                    debug_print_str("\nFile: handle_count=");
                    debug_print_hex(handle_count);
                    debug_print_str("\n");
                    if EfiStatus::from(status) != EfiStatus::Success || handle_count == 0 || handles.is_null() {
                        file_debug!("Failed to locate SimpleFileSystem handles.");
                        return Err(BellowsError::ProtocolNotFound(
                            "No SimpleFileSystem handles found.",
                        ));
                    }
                    file_debug!("Located SimpleFileSystem handles.");
                    // Use the first handle
                    let fs_handle = unsafe { *handles };
                    (bs.free_pool)(handles as *mut c_void);
                    let proto = open_protocol::<EfiSimpleFileSystem>(
                        bs,
                        fs_handle,
                        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
                        image_handle,
                        2, // EFI_OPEN_PROTOCOL_GET_PROTOCOL
                    )?;
                    proto
                }
            }
        }
    };
    file_debug!("Got SimpleFileSystem protocol.");

    let mut volume_file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs_proto).open_volume)(fs_proto, &mut volume_file_handle) };
    let volume = match EfiStatus::from(status) {
        EfiStatus::Success => {
            file_debug!("Opened volume.");
            EfiFileWrapper::new(volume_file_handle)
        }
        _ => {
            file_debug!("Failed to open volume.");
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
        file_debug!("Failed to get file info size.");
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }
    file_debug!("Got file info size.");

    if file_info_buffer_size == 0 {
        file_debug!("File info size is 0.");
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }

    // Use a page-sized buffer on the stack, which is safer for variable-sized info.
    let mut file_info_buffer = [0u8; 4096];
    if file_info_buffer_size > file_info_buffer.len() {
        file_debug!("File info buffer too large for fixed buffer.");
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
        file_debug!("Failed to get file info.");
        return Err(BellowsError::FileIo("Failed to get file info."));
    }
    file_debug!("Got file info.");
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
        file_debug!("Failed to allocate pages.");
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate pages for kernel file.",
        ));
    }
    file_debug!("Allocated pages.");

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*file.file).read)(file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        // It's important to free the allocated pages on failure to avoid memory leaks.
        (bs.free_pages)(phys_addr, pages);
        file_debug!("Failed to read file.");
        return Err(BellowsError::FileIo(
            "Failed to read kernel file or read size mismatch.",
        ));
    }
    file_debug!("Read file.");

    file_debug!("Returning from read_efi_file.");
    Ok((phys_addr, file_size))
}
