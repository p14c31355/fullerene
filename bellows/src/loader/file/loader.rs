use core::ptr;
use petroleum::common::{EfiBootServices, EfiFile, EfiStatus, BellowsError};
use super::protocols::{get_loaded_image_protocol, get_simple_file_system};
use super::filesystem::{EfiFileWrapper, open_file, kernel_path_utf16, read_file_to_memory};
use super::super::debug::*;

macro_rules! file_debug {
    ($msg:expr) => {
        debug_print_str(concat!("File: ", $msg, "\n"));
    };
}

/// Read `fullerene-kernel.efi` from the volume.
pub fn read_efi_file(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<(usize, usize)> {
    file_debug!("Starting read_efi_file...");

    let loaded_image = get_loaded_image_protocol(bs, image_handle)?;
    file_debug!("Got loaded image protocol.");
    let device_handle = unsafe { (*loaded_image).device_handle };
    debug_print_str("Device handle: ");
    debug_print_hex(device_handle);
    debug_print_str("\n");

    let fs_proto = get_simple_file_system(bs, device_handle, image_handle)?;
    file_debug!("Got SimpleFileSystem protocol.");

    let mut volume_file_handle: *mut EfiFile = ptr::null_mut();
    let status = unsafe { ((*fs_proto).open_volume)(fs_proto, &mut volume_file_handle) };
    if EfiStatus::from(status) != EfiStatus::Success {
        file_debug!("Failed to open volume.");
        return Err(BellowsError::FileIo(
            "Failed to open EFI SimpleFileSystem protocol volume.",
        ));
    }
    file_debug!("Opened volume.");
    let volume = EfiFileWrapper::new(volume_file_handle);

    // Correct file name to match the kernel file
    let file = open_file(
        &volume,
        &kernel_path_utf16()[..], // Fixed slice
    )?;

    let (phys_addr, file_size) = read_file_to_memory(bs, &file)?;

    file_debug!("Read file successfully.");
    Ok((phys_addr, file_size))
}
