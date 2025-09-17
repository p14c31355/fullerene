// bellows/src/main.rs

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

use alloc::format;
use core::alloc::Layout;
use core::ffi::c_void;
use core::slice;

mod loader;
mod uefi;

use crate::loader::{
    exit_boot_services_and_jump, file::read_efi_file, heap::init_heap, pe::load_efi_image,
};

use crate::uefi::{
    uefi_print, EfiGraphicsOutputProtocol, EfiSystemTable,
    FullereneFramebufferConfig, EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
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
    const TARGET_WIDTH: u32 = 1024;
    const TARGET_HEIGHT: u32 = 768;

    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();
    let _ = (bs.locate_protocol)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
        core::ptr::null_mut(),
        &mut gop as *mut _ as *mut *mut c_void,
    );
    if !gop.is_null() {
        let _mode = unsafe { (*(*gop).mode).current_mode };
        let info = unsafe { &*(*(*gop).mode).info };
        uefi_print(
            st,
            &format!("gop info: {:?}\n", info),
        );
        let fb_addr = unsafe { (*(*gop).mode).frame_buffer_base };
        let fb_size = unsafe { (*(*gop).mode).frame_buffer_size } as usize;
        let fb_ptr = fb_addr as *mut u32;
        let _ = (bs.install_configuration_table)(
            FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            &FullereneFramebufferConfig {
                address: fb_addr as u64,
                width: info.horizontal_resolution,
                height: info.vertical_resolution,
                stride: info.pixels_per_scan_line,
                pixel_format: info.pixel_format,
            } as *const _ as *mut c_void,
        );
        for i in 0..fb_size {
            unsafe {
                *fb_ptr.add(i) = 0x000000;
            }
        }
    }
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

    // Read the kernel file before exiting boot services.
    let (efi_image_phys, efi_image_size) = match read_efi_file(st) {
        Ok(info) => info,
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            panic!();
        }
    };
    let efi_image_file = unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) };

    // Load the kernel and get its entry point.
    let entry = match load_efi_image(st, efi_image_file) {
        Ok(e) => e,
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            panic!();
        }
    };

    // Exit boot services and jump to the kernel.
    // The loader::exit_boot_services_and_jump function will handle the transition.
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => {
            // Should not be reached.
            uefi_print(st, "\nJump failed.\n");
            panic!();
        }
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            panic!();
        }
    }
}
