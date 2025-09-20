// bellows/src/main.rs

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

use alloc::{boxed::Box, format};
use core::alloc::Layout;
use core::ffi::c_void;
use core::ptr;
use core::slice;

use spin::Mutex;

#[derive(Clone, Copy)]
struct UefiSystemTablePtr(*mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

mod loader;
mod uefi;

use crate::loader::{
    exit_boot_services_and_jump, file::read_efi_file, heap::init_heap, pe::load_efi_image, serial,
};

use crate::uefi::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
};

/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    panic!("Allocation error");
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Print the panic message using the refactored serial module.
        if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        // Initialize the writer to ensure panic messages can be printed.
        unsafe {
            serial::UEFI_WRITER.init(st_ref.con_out);
        }
        // We use the same `uefi_print` here, but it's now a different function that uses `_print`.
                if let Some(location) = info.location() {
            println!(
                "Panic at {}:{}:{} - {}",
                location.file(),
                location.line(),
                location.column(),
                info.message().unwrap_or(&format_args!("no message"))
            );
        } else {
            println!("Panic: {}", info.message().unwrap_or(&format_args!("no message")));
        }
    }

    loop {}
}

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table));
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    // Initialize the serial writer with the console output pointer.
    unsafe {
        serial::UEFI_WRITER.init(st.con_out);
    }
    
    println!("Bellows UEFI Bootloader starting...");

    println!("Attempting to initialize heap...");
    if let Err(e) = init_heap(bs) {
        println!("Failed to initialize heap: {:?}", e);
        panic!("Failed to initialize heap.");
    }
    println!("Heap initialized successfully.");
    println!("Attempting to initialize GOP...");
    init_gop(st);
    serial::_print(format_args!("GOP initialized successfully.\n"));

    serial::_print(format_args!("Attempting to read kernel EFI file...\n"));
    // Read the kernel file before exiting boot services.
    let (efi_image_phys, efi_image_size) = match read_efi_file(bs) {
        Ok(t) => t,
        Err(err) => {
            serial::_print(format_args!("Failed to read EFI file: {:?}\n", err));
            panic!("Failed to read EFI file.");
        }
    };
    serial::_print(format_args!(
        "Kernel EFI file read. Physical address: {:#x}, size: {}\n",
        efi_image_phys,
        efi_image_size
    ));

    let efi_image_file = {
        // Safety:
        // `efi_image_phys` and `efi_image_size` are returned by `read_efi_file`,
        // which allocates a valid memory region and reads the file into it.
        if efi_image_size == 0 {
            serial::_print(format_args!("Kernel file is empty.\n"));
            panic!("Kernel file is empty.");
        }
        unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) }
    };

    // Load the kernel and get its entry point.
    let entry = match load_efi_image(st, efi_image_file) {
        Ok(e) => e,
        Err(err) => {
            serial::_print(format_args!("Failed to load EFI image: {:?}\n", err));
            let file_pages = efi_image_size.div_ceil(4096);
            (bs.free_pages)(efi_image_phys, file_pages);
            panic!("Failed to load EFI image.");
        }
    };

    let file_pages = efi_image_size.div_ceil(4096);
    (bs.free_pages)(efi_image_phys, file_pages);

    // Exit boot services and jump to the kernel.
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => {
            unreachable!(); // This branch should never be reached if the function returns '!'
        }
        Err(err) => {
            serial::_print(format_args!("Failed to exit boot services: {:?}\n", err));
            panic!("Failed to exit boot services.");
        }
    }
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
        serial::_print(format_args!(
            "Failed to locate GOP protocol, continuing without it.\n"
        ));
        return;
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        serial::_print(format_args!("GOP mode pointer is null, skipping.\n"));
        return;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        serial::_print(format_args!("GOP mode info pointer is null, skipping.\n"));
        return;
    }

    let info_ref = unsafe { &*mode_ref.info };

    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;
    let info = info_ref;

    if fb_addr == 0 || fb_size == 0 {
        serial::_print(format_args!(
            "GOP framebuffer info is invalid, skipping.\n"
        ));
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
        let _ = unsafe { Box::from_raw(config_ptr) };
        serial::_print(format_args!(
            "Failed to install framebuffer config table.\n"
        ));
        return;
    }

    unsafe {
        core::ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }
}