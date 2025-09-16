// bellows/src/main.rs
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

use core::alloc::Layout;
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
    EfiSystemTable,
    uefi_print,
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
            unsafe { (bs.free_pages)(efi_image_phys, efi_image_size.div_ceil(4096)); }
            panic!();
        }
    };

    let file_pages = efi_image_size.div_ceil(4096);
    unsafe { (bs.free_pages)(efi_image_phys, file_pages); }

    uefi_print(st, "bellows: Exiting Boot Services...\n");
    if let Err(msg) = exit_boot_services_and_jump(image_handle, system_table, entry) {
        uefi_print(st, msg);
        panic!();
    }
    unreachable!();
}