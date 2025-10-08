use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EFI_LOADED_IMAGE_PROTOCOL_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
    EfiBootServices, EfiLoadedImageProtocol, EfiSimpleFileSystem, EfiStatus,
};

use super::super::debug::*;

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
        return Err(BellowsError::ProtocolNotFound("Failed to open protocol."));
    }
    file_debug!("Opened protocol.");
    Ok(proto)
}

pub fn get_loaded_image_protocol(
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

pub fn get_simple_file_system(
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
