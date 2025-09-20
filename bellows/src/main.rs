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
    exit_boot_services_and_jump, file::read_efi_file, heap::init_heap, pe::load_efi_image,
};

use crate::uefi::{
    BellowsError, EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiStatus,
    EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
    uefi_print,
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
    // Print the panic message if available.
    if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        if let Some(location) = info.location() {
            let msg = format!(
                "Panic at {}:{}:{} - {}\n",
                location.file(),
                location.line(),
                location.column(),
                info.message()
            );
            uefi_print(st_ref, &msg);
        } else {
            let msg = format!("Panic: {}\n", info.message());
            uefi_print(st_ref, &msg);
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

    uefi_print(st, "Bellows UEFI Bootloader starting...\n");
    uefi_print(st, "Initializing heap...\n");
    if let Err(e) = init_heap(bs) {
        uefi_print(st, &format!("Failed to initialize heap: {:?}\n", e));
        panic!("Failed to initialize heap.");
    }
    uefi_print(st, "Heap initialized.\n");

    uefi_print(st, "Initializing GOP...\n");
    init_gop(st);
    uefi_print(st, "GOP initialized.\n");

    // Read the kernel file before exiting boot services.
    let (efi_image_phys, efi_image_size) = match read_efi_file(st) {
        Ok(t) => t,
        Err(err) => {
            uefi_print(st, &format!("Failed to read EFI file: {:?}\n", err));
            panic!("Failed to read EFI file.");
        }
    };

    let efi_image_file = {
        // Safety:
        // `efi_image_phys` and `efi_image_size` are returned by `read_efi_file`,
        // which allocates a valid memory region and reads the file into it.
        if efi_image_size == 0 {
            uefi_print(st, "Kernel file is empty.\n");
            panic!("Kernel file is empty.");
        }
        unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) }
    };

    // Load the kernel and get its entry point.
    let entry = match load_efi_image(st, efi_image_file) {
        Ok(e) => e,
        Err(err) => {
            uefi_print(st, &format!("Failed to load EFI image: {:?}\n", err));
            let file_pages = efi_image_size.div_ceil(4096);
            unsafe {
                (bs.free_pages)(efi_image_phys, file_pages);
            }
            panic!("Failed to load EFI image.");
        }
    };

    let file_pages = efi_image_size.div_ceil(4096);
    unsafe {
        (bs.free_pages)(efi_image_phys, file_pages);
    }

    // Exit boot services and jump to the kernel.
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => {
            unreachable!(); // This branch should never be reached if the function returns '!'
        }
        Err(err) => {
            uefi_print(st, &format!("Failed to exit boot services: {:?}\n", err));
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
        uefi_print(
            st,
            "Failed to locate GOP protocol, continuing without it.\n",
        );
        return;
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        uefi_print(st, "GOP mode pointer is null, skipping.\n");
        return;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        uefi_print(st, "GOP mode info pointer is null, skipping.\n");
        return;
    }

    let info_ref = unsafe { &*mode_ref.info };

    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;
    let info = info_ref;

    if fb_addr == 0 || fb_size == 0 {
        uefi_print(st, "GOP framebuffer info is invalid, skipping.\n");
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
        uefi_print(st, "Failed to install framebuffer config table.\n");
        return;
    }

    unsafe {
        core::ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }
}
