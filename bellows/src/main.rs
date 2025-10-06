// bellows/src/main.rs

#![no_std]
#![no_main]
// #![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

use alloc::boxed::Box;
use core::{ffi::c_void, ptr, slice};
use x86_64::instructions::port::Port; // Import Port for direct I/O

mod loader;

use loader::{
    debug::*, exit_boot_services_and_jump, file::read_efi_file, heap::init_heap, pe::load_efi_image,
};

use petroleum::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
};

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    debug_print_str("Bellows: efi_main entered.\n"); // Early debug print

    let _ = petroleum::UEFI_SYSTEM_TABLE
        .lock()
        .insert(petroleum::UefiSystemTablePtr(system_table));
    debug_print_str("Bellows: UEFI_SYSTEM_TABLE initialized.\n"); // Debug print after initialization
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    debug_print_str("Bellows: UEFI system table and boot services acquired.\n"); // Early debug print

    // Initialize the serial writer with the console output pointer.
    petroleum::serial::UEFI_WRITER.lock().init(st.con_out);
    debug_print_str("Bellows: UEFI_WRITER initialized.\n"); // Debug print after UEFI_WRITER init

    petroleum::println!("Bellows UEFI Bootloader starting...");
    debug_print_str("Bellows: 'Bellows UEFI Bootloader starting...' printed.\n"); // Debug print after println!
    petroleum::serial::_print(format_args!("Attempting to initialize GOP...\n"));
    petroleum::println!("Image Handle: {:#x}", image_handle);
    petroleum::println!("System Table: {:#p}", system_table);
    // Initialize heap
    petroleum::serial::_print(format_args!("Attempting to initialize heap...\n"));
    match init_heap(bs) {
        Ok(()) => {
            petroleum::serial::_print(format_args!("Heap initialized successfully.\n"));
            debug_print_str("Bellows: Heap OK.\n");
        }
        Err(e) => {
            petroleum::serial::_print(format_args!("Heap failed (OK for minimal boot): {:?}\n", e));
            debug_print_str("Bellows: Heap skipped.\n");
            // Continue without heap; use fixed buffers in PE loader
        }
    }
    init_gop(st);
    petroleum::serial::_print(format_args!("GOP initialized successfully.\n"));
    debug_print_str("Bellows: GOP initialized.\n"); // Debug print after GOP initialization

    debug_print_str("Bellows: Reading kernel from file...\n");
    // Read the kernel from the file system
    let (addr, size) = match read_efi_file(bs, image_handle) {
        Ok(data) => data,
        Err(err) => {
            petroleum::println!("Failed to read kernel file: {:?}", err);
            panic!("Failed to read kernel file.");
        }
    };
    let efi_image_file = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let efi_image_size = size;

    if efi_image_size == 0 {
        debug_print_str("Bellows: Kernel file is empty!\n");
        petroleum::println!("Kernel file is empty.");
        panic!("Kernel file is empty.");
    }

    debug_print_str("Bellows: Kernel file loaded.\n");
    petroleum::serial::_print(format_args!(
        "Kernel file loaded. Size: {}\n",
        efi_image_size
    ));

    petroleum::serial::_print(format_args!("Attempting to load EFI image...\n"));

    // Load the kernel and get its entry point.
    let entry = match load_efi_image(st, efi_image_file) {
        Ok(e) => {
            petroleum::serial::_print(format_args!(
                "EFI image loaded successfully. Entry point: {:#p}\n",
                e as *const ()
            ));
            e
        }
        Err(err) => {
            petroleum::println!("Failed to load EFI image: {:?}", err);
            let file_pages = efi_image_size.div_ceil(4096);
            unsafe {
                (bs.free_pages)(addr, file_pages);
            }
            panic!("Failed to load EFI image.");
        }
    };
    debug_print_str("Bellows: EFI image loaded.\n"); // Debug print after load_efi_image

    // Free the memory that was used to hold the kernel file contents
    let file_pages = efi_image_size.div_ceil(4096);
    unsafe {
        (bs.free_pages)(addr, file_pages);
    }

    debug_print_str("Bellows: Kernel loaded into allocated memory.\n");

    petroleum::serial::_print(format_args!(
        "Exiting boot services and jumping to kernel...\n"
    ));
    // Exit boot services and jump to the kernel.
    debug_print_str("Bellows: About to exit boot services and jump to kernel.\n"); // Debug print just before the call
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => {
            unreachable!(); // This branch should never be reached if the function returns '!'
        }
        Err(err) => {
            petroleum::println!("Failed to exit boot services: {:?}", err);
            panic!("Failed to exit boot services.");
        }
    }
    debug_print_str("Bellows: Exited boot services and jumped to kernel.\n"); // Debug print after exit_boot_services_and_jump
}

/// Initializes the Graphics Output Protocol (GOP) for framebuffer access.
fn init_gop(st: &EfiSystemTable) {
    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    let status = (bs.locate_protocol)(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut gop as *mut _ as *mut *mut c_void,
    );

    if EfiStatus::from(status) != EfiStatus::Success || gop.is_null() {
        petroleum::serial::_print(format_args!(
            "Failed to locate GOP protocol, continuing without it.\n"
        ));
        return;
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        petroleum::serial::_print(format_args!("GOP mode pointer is null, skipping.\n"));
        return;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        petroleum::serial::_print(format_args!("GOP mode info pointer is null, skipping.\n"));
        return;
    }

    let info_ref = unsafe { &*mode_ref.info };

    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;
    let info = info_ref;

    if fb_addr == 0 || fb_size == 0 {
        petroleum::serial::_print(format_args!("GOP framebuffer info is invalid, skipping.\n"));
        return;
    }

    let config = Box::new(FullereneFramebufferConfig {
        address: fb_addr as u64,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        stride: info.pixels_per_scan_line,
        pixel_format: info.pixel_format,
    });

    let config_ptr = Box::leak(config);

    let status = (bs.install_configuration_table)(
        &FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID as *const _ as *const u8,
        config_ptr as *const _ as *mut c_void,
    );

    if EfiStatus::from(status) != EfiStatus::Success {
        petroleum::serial::_print(format_args!(
            "Failed to install framebuffer config table, recovering memory.\n"
        ));
        let _ = unsafe { Box::from_raw(config_ptr) };
        petroleum::serial::_print(format_args!(
            "Failed to install framebuffer config table.\n"
        ));
        return;
    }

    unsafe {
        core::ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }
}

