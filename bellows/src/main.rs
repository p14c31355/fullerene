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
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiSystemTable,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig, uefi_print,
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
    loop {}
}

fn init_gop(st: &EfiSystemTable) {
    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();

    // Safety:
    // The GUID is a static global, its address is valid.
    // The function pointer is from the UEFI boot services table, assumed to be valid.
    let status = unsafe {
        (bs.locate_protocol)(
            EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
            core::ptr::null_mut(),
            &mut gop as *mut _ as *mut *mut c_void,
        )
    };

    if status != 0 || gop.is_null() {
        // Optionally print an error, but GOP is not critical for booting.
        return;
    }

    // Safety:
    // We have checked that `gop` is not null. The `mode` field and its `info` are
    // guaranteed to be valid by the UEFI specification after a successful `locate_protocol`.
    let (info, fb_addr, fb_size) = unsafe {
        let gop_ref = &*gop;
        let mode_ref = &*gop_ref.mode;
        let info_ref = &*mode_ref.info;
        (
            info_ref,
            mode_ref.frame_buffer_base,
            mode_ref.frame_buffer_size as usize,
        )
    };

    let fb_ptr = fb_addr as *mut u32;

    // Safety:
    // The GUID is a static global, its address is valid.
    // The function pointer is from the UEFI boot services table, assumed to be valid.
    // The `FullereneFramebufferConfig` struct is valid for the duration of the call.
    let status = unsafe {
        (bs.install_configuration_table)(
            FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            &FullereneFramebufferConfig {
                address: fb_addr as u64,
                width: info.horizontal_resolution,
                height: info.vertical_resolution,
                stride: info.pixels_per_scan_line,
                pixel_format: info.pixel_format,
            } as *const _ as *mut c_void,
        )
    };

    if status != 0 {
        uefi_print(st, "Failed to install framebuffer config table.\n");
        // Decide if this is a fatal error.
    }

    let num_pixels = fb_size / 4; // Assuming 32bpp
    if fb_ptr.is_null() {
        return; // Nothing to clear if framebuffer is not available.
    }
    // Safety:
    // We are writing to the framebuffer memory region.
    // We have verified the `fb_ptr` is not null and the `num_pixels` is calculated
    // from the size provided by the UEFI firmware. This is a reasonable assumption
    // for a bootloader and the only way to interact with the framebuffer.
    for i in 0..num_pixels {
        unsafe {
            let pixel_ptr = fb_ptr.add(i);
            // We use `write_volatile` to ensure the compiler doesn't optimize away
            // the writes to the memory-mapped I/O region.
            core::ptr::write_volatile(pixel_ptr, 0x00000000);
        }
    }
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[no_mangle]
pub unsafe extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
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

    let efi_image_file = {
        // Safety:
        // `efi_image_phys` and `efi_image_size` are returned by `read_efi_file`,
        // which allocates a valid memory region and reads the file into it.
        // The slice created from this memory is valid for the duration of this function.
        if efi_image_size == 0 {
            uefi_print(st, "Kernel file is empty.\n");
            panic!();
        }
        unsafe { slice::from_raw_parts(efi_image_phys as *const u8, efi_image_size) }
    };

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
