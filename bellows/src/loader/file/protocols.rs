use core::ffi::c_void;
use core::ptr;
use petroleum::common::{
    BellowsError, EFI_LOADED_IMAGE_PROTOCOL_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
    EfiBootServices, EfiLoadedImageProtocol, EfiSimpleFileSystem, EfiStatus,
};

use petroleum::serial::{debug_print_hex, debug_print_str_to_com1 as debug_print_str};

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
        debug_print_str("File: Failed to open protocol. Status: ");
        debug_print_hex(status);
        debug_print_str("\n");
        return Err(BellowsError::ProtocolNotFound("Failed to open protocol."));
    }
    debug_print_str("File: Opened protocol.\n");
    Ok(proto)
}

pub fn get_loaded_image_protocol(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<*mut EfiLoadedImageProtocol> {
    let mut loaded_image: *mut EfiLoadedImageProtocol = ptr::null_mut();
    debug_print_str("File: Getting loaded image protocol for handle=");
    debug_print_hex(image_handle);
    debug_print_str("\n");

    // Try handle_protocol first
    debug_print_str("File: Trying handle_protocol\n");
    let status_h = (bs.handle_protocol)(
        image_handle,
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        &mut loaded_image as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(status_h) == EfiStatus::Success {
        debug_print_str("File: Loaded image protocol found via handle_protocol\n");
        return Ok(loaded_image);
    }
    debug_print_str("File: handle_protocol failed (");

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
    debug_print_str("File: Trying global locate_protocol for LoadedImage\n");
    let mut global_loaded: *mut EfiLoadedImageProtocol = ptr::null_mut();
    let loc_status = (bs.locate_protocol)(
        &EFI_LOADED_IMAGE_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut global_loaded as *mut _ as *mut *mut core::ffi::c_void,
    );
    debug_print_str("File: global locate_protocol status=");
    debug_print_hex(loc_status);
    debug_print_str("\n");
    if EfiStatus::from(loc_status) == EfiStatus::Success && !global_loaded.is_null() {
        debug_print_str("File: Global locate_protocol succeeded\n");
        return Ok(global_loaded);
    }

    Err(BellowsError::ProtocolNotFound(
        "All LoadedImage methods failed.",
    ))
}

pub fn get_simple_file_system(
    bs: &EfiBootServices,
    device_handle: usize,
    image_handle: usize,
) -> petroleum::common::Result<*mut EfiSimpleFileSystem> {
    debug_print_str("File: Getting SimpleFileSystem, device_handle=");
    debug_print_hex(device_handle);
    debug_print_str(", image_handle=");
    debug_print_hex(image_handle);
    debug_print_str("\n");

    // First try locate_protocol (global)
    let mut fs_proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.locate_protocol)(
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut fs_proto_ptr as *mut _ as *mut *mut c_void,
    );
    if EfiStatus::from(status) == EfiStatus::Success && !fs_proto_ptr.is_null() {
        return Ok(fs_proto_ptr);
    }

    // Try locate_handle_buffer to get any SimpleFileSystem
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
    debug_print_str(", handle_count=");
    debug_print_hex(handle_count);
    debug_print_str("\n");
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

    // Try handle_protocol on device_handle
    let mut proto_ptr: *mut EfiSimpleFileSystem = ptr::null_mut();
    let status = (bs.handle_protocol)(
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        &mut proto_ptr as *mut _ as *mut *mut c_void,
    );
    debug_print_str("File: device_handle protocol status=");
    debug_print_hex(status);
    debug_print_str("\n");
    if EfiStatus::from(status) == EfiStatus::Success && !proto_ptr.is_null() {
        return Ok(proto_ptr);
    }

    // Try open_protocol on device_handle
    if let Ok(proto) = open_protocol::<EfiSimpleFileSystem>(
        bs,
        device_handle,
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID as *const _ as *const u8,
        image_handle,
        1,
    ) {
        return Ok(proto);
    }

    Err(BellowsError::ProtocolNotFound(
        "No SimpleFileSystem handles found.",
    ))
}
