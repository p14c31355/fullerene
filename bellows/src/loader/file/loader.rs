use super::filesystem::{EfiFileWrapper, kernel_path_utf16, open_file, read_file_to_memory};
use super::protocols::{get_loaded_image_protocol, get_simple_file_system};
use core::ptr;
use petroleum::common::{BellowsError, EfiBootServices, EfiFile, EfiStatus};
use petroleum::serial::{debug_print_hex, debug_print_str_to_com1 as debug_print_str};

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<(usize, usize)> {
    debug_print_str("File: Starting read_efi_file...\n");

    let loaded_image = get_loaded_image_protocol(bs, image_handle)?;
    debug_print_str("File: Got loaded image protocol.\n");
    let device_handle = unsafe { (*loaded_image).device_handle };
    debug_print_str("Device handle: ");
    debug_print_hex(device_handle);
    debug_print_str("\n");

    let fs_proto = get_simple_file_system(bs, device_handle, image_handle)?;
    debug_print_str("File: Got SimpleFileSystem protocol.\n");

    let mut volume_file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs_proto).open_volume)(fs_proto, &mut volume_file_handle) };
    if EfiStatus::from(status) != EfiStatus::Success {
        debug_print_str("File: Failed to open volume.\n");
        return Err(BellowsError::FileIo(
            "Failed to open EFI SimpleFileSystem protocol volume.",
        ));
    }
    debug_print_str("File: Opened volume.\n");
    let volume = EfiFileWrapper::new(volume_file_handle);

    // Correct file name to match the kernel file
    let file = open_file(
        &volume,
        &kernel_path_utf16()[..], // Fixed slice
    )?;

    let (phys_addr, file_size) = read_file_to_memory(bs, &file)?;

    debug_print_str("File: Read file successfully.\n");
    Ok((phys_addr, file_size))
}
