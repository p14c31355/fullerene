// bellows/src/main.rs
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::alloc::Layout;
use core::ffi::c_void;
use core::slice;

mod loader;
mod uefi;

use crate::loader::{
    file::{read_efi_file},
    heap::init_heap,
    pe::{load_efi_image},
    exit_boot_services_and_jump,
};

use crate::uefi::{
    EfiGraphicsOutputProtocol,
    EfiSystemTable,
    uefi_print,
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
};


/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    panic!();
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    panic!();
}

fn init_gop(st: &EfiSystemTable) {
    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();
    let status = (unsafe {
        (bs.locate_protocol)(
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID as *const u8,
            core::ptr::null_mut(),
            &mut gop as *mut *mut EfiGraphicsOutputProtocol as *mut *mut c_void,
        )
    });

    if status != 0 || gop.is_null() {
        uefi_print(st, "bellows: GOP not found\n");
        return;
    }

    let gop = unsafe { &mut *gop };
    let mut info: *mut crate::uefi::EfiGraphicsOutputModeInformation = core::ptr::null_mut();
    let mut size_of_info: usize = 0;
    let mut best_mode = None;

    for i in 0..unsafe { (*gop.mode).max_mode } {
        if (gop.query_mode)(gop, i, &mut size_of_info, &mut info) == 0 {
            let info = unsafe { &*info };
            if info.horizontal_resolution == 1024 && info.vertical_resolution == 768 {
                best_mode = Some(i);
            }
        }
    }

    if let Some(mode) = best_mode {
        if (gop.set_mode)(gop, mode) == 0 {
            uefi_print(st, "bellows: Set mode 1024x768\n");
        } else {
            uefi_print(st, "bellows: Failed to set mode 1024x768\n");
        }
    } else {
        uefi_print(st, "bellows: Mode 1024x768 not found\n");
    }

    let mode = unsafe { &*gop.mode };
    let info = unsafe { &*mode.info };

    let s = format!(
        "bellows: GOP initialized\n    Resolution: {}x{}\n    Framebuffer base: {:#x}\n    Framebuffer size: {}\n",
        info.horizontal_resolution,
        info.vertical_resolution,
        mode.frame_buffer_base,
        mode.frame_buffer_size
    );
    uefi_print(st, &s);
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };
    uefi_print(st, "bellows: bootloader started\n");

    if let Err(msg) = init_heap(bs) {
        uefi_print(st, msg);
        panic!();
    }

    init_gop(st);

    let (efi_image_phys, efi_image_size) = match read_efi_file(st) {
        Ok(info) => info,
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            panic!();
        }
    };
    let efi_image_file = unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) };
    
    let entry = match load_efi_image(st, efi_image_file) {
        Ok(e) => e,
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            (unsafe { (bs.free_pages)(efi_image_phys, efi_image_size.div_ceil(4096)) });
            panic!();
        }
    };

    let file_pages = efi_image_size.div_ceil(4096);
    (unsafe { (bs.free_pages)(efi_image_phys, file_pages) });

    uefi_print(st, "bellows: Exiting Boot Services...\n");
    let Err(msg) = exit_boot_services_and_jump(image_handle, system_table, entry);
    uefi_print(st, msg);
    panic!();
}
