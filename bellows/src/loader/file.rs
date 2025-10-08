// bellows/src/loader/file.rs

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

fn get_loaded_image_protocol(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<*mut EfiLoadedImageProtocol> {
    let mut loaded_image: *mut EfiLoadedImageProtocol = ptr::null_mut();

    // Try handle_protocol first
    let status_h = (bs.handle_protocol)(
        image_handle,
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        &mut loaded_image as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(status_h) == EfiStatus::Success {
        return Ok(loaded_image);
    }

    // Try locate_handle_buffer
    let mut handle_count = 0;
    let mut handles: *mut usize = ptr::null_mut();
    let status = (bs.locate_handle_buffer)(
        2, // ByProtocol
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut handle_count,
        &mut handles,
    );
    if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 && !handles.is_null() {
        let loaded_handle = unsafe { *handles };
        (bs.free_pool)(handles as *mut c_void);
        let status = (bs.open_protocol)(
            loaded_handle,
            &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
            &mut loaded_image as *mut _ as *mut *mut c_void,
            0,
            0,
            1,
        );
        if EfiStatus::from(status) == EfiStatus::Success {
            return Ok(loaded_image);
        }
    }

    // LocateProtocol fallback
    let mut global_loaded: *mut EfiLoadedImageProtocol = ptr::null_mut();
    let loc_status = (bs.locate_protocol)(
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut global_loaded as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(loc_status) == EfiStatus::Success && !global_loaded.is_null() {
        return Ok(global_loaded);
    }

    Err(BellowsError::ProtocolNotFound("All LoadedImage methods failed."))
}

fn get_simple_file_system(
    bs: &EfiBootServices,
    device_handle: usize,
    image_handle: usize,
) -> petroleum::common::Result<*mut EfiSimpleFileSystem> {
    // Try locate_protocol first
    let mut fs_proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.locate_protocol)(
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut fs_proto_ptr as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(status) == EfiStatus::Success && !fs_proto_ptr.is_null() {
        return Ok(fs_proto_ptr);
    }

    // Try handle_protocol on device_handle
    let mut proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.handle_protocol)(
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        &mut proto_ptr as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(status) == EfiStatus::Success && !proto_ptr.is_null() {
        return Ok(proto_ptr);
    }

    // Try open_protocol
    if let Ok(proto) = open_protocol::<EfiSimpleFileSystem>(
        bs,
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        image_handle,
        1,
    ) {
        return Ok(proto);
    }

    // Try locate_handle_buffer
    let mut handle_count = 0;
    let mut handles: *mut usize = ptr::null_mut();
    let status = (bs.locate_handle_buffer)(
        2,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut handle_count,
        &mut handles,
    );
    if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 && !handles.is_null() {
        let fs_handle = unsafe { *handles };
        (bs.free_pool)(handles as *mut c_void);
        let proto = open_protocol::<EfiSimpleFileSystem>(
            bs,
            fs_handle,
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
            image_handle,
            1,
        )?;
        return Ok(proto);
    }

    Err(BellowsError::ProtocolNotFound("No SimpleFileSystem handles found."))
}

// Function to read file content into memory

fn read_file_to_memory(
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
    if EfiStatus::from(status) != EfiStatus::BufferTooSmall || file_info_buffer_size == 0 {
        file_debug!("Failed to get file info size.");
        return Err(BellowsError::FileIo("Failed to get file info size."));
    }

    let mut file_info_buffer = [0u8; 4096];
    if file_info_buffer_size > file_info_buffer.len() {
        file_debug!("File info buffer too large.");
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
        file_debug!("Failed to allocate pages.");
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate pages for kernel file.",
        ));
    }

    let buf_ptr = phys_addr as *mut u8;
    let mut read_size = file_size as u64;

    let status = unsafe { ((*file.file).read)(file.file, &mut read_size, buf_ptr) };
    if EfiStatus::from(status) != EfiStatus::Success || read_size as usize != file_size {
        (bs.free_pages)(phys_addr, pages);
        file_debug!("Failed to read file.");
        return Err(BellowsError::FileIo(
            "Failed to read kernel file or read size mismatch.",
        ));
    }

    Ok((phys_addr, file_size))
}

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(
    bs: &EfiBootServices,
    image_handle: usize,
    _system_table: *mut petroleum::common::EfiSystemTable,
) -> petroleum::common::Result<(usize, usize)> {
    file_debug!("Starting read_efi_file...");

    let loaded_image = get_loaded_image_protocol(bs, image_handle)?;
    let device_handle = unsafe { (*loaded_image).device_handle };

    let fs_proto = get_simple_file_system(bs, device_handle, image_handle)?;
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

    let (phys_addr, file_size) = read_file_to_memory(bs, &file)?;

    file_debug!("Read file successfully.");
    Ok((phys_addr, file_size))
}
