// bellows/src/main.rs

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use core::alloc::Layout;
use core::ffi::c_void;
use core::mem;
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
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputProtocol, EfiGraphicsOutputProtocolMode,
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
    // Use `try_lock` to avoid deadlocking if the panic occurred while the lock was held.
    if let Some(guard) = UEFI_SYSTEM_TABLE.try_lock() {
        if let Some(UefiSystemTablePtr(st_ptr)) = *guard {
            if !st_ptr.is_null() {
                let st = unsafe { &*st_ptr };
                uefi_print(st, &format!("Panicked at: {}\n", info));
            }
        }
    }
    loop {}
}

fn init_gop(st: &EfiSystemTable) {
    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    // Safety:
    // The GUID is a static global, its address is valid.
    // The function pointer is from the UEFI boot services table, assumed to be valid.
    let status = unsafe {
        (bs.locate_protocol)(
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID as *const _ as *const u8,
            ptr::null_mut(),
            &mut gop as *mut _ as *mut *mut c_void,
        )
    };

    if status != 0 || gop.is_null() {
        uefi_print(
            st,
            "Failed to locate GOP protocol, continuing without it.\n",
        );
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
            mode_ref.frame_buffer_size,
        )
    };

    let fb_ptr = fb_addr as *mut u32;

    let config = Box::new(FullereneFramebufferConfig {
        address: fb_addr as u64,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        stride: info.pixels_per_scan_line,
        pixel_format: info.pixel_format,
    });

    // Leak the box to prevent the memory from being deallocated.
    // The pointer will be valid for the kernel to use.
    let config_ptr = Box::leak(config);

    // Safety:
    // The GUID is a static global, its address is valid.
    // The function pointer is from the UEFI boot services table, assumed to be valid.
    // The `FullereneFramebufferConfig` struct is valid for the duration of the call.
    let status = unsafe {
        (bs.install_configuration_table)(
            &FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID as *const _ as *const u8,
            config_ptr as *const _ as *mut c_void,
        )
    };

    if status != 0 {
        uefi_print(st, "Failed to install framebuffer config table.\n");
    }

    let num_pixels = fb_size / mem::size_of::<u32>();
    if fb_ptr.is_null() || num_pixels == 0 {
        return;
    }

    // Safety:
    // We are writing to the framebuffer memory region.
    // We have verified the `fb_ptr` is not null and the `num_pixels` is calculated
    // from the size provided by the UEFI firmware. `write_volatile` is used to
    // prevent the compiler from optimizing the memory-mapped I/O writes away.
    unsafe {
        ptr::write_bytes(fb_ptr as *mut u8, 0x00, fb_size);
    }
}

/// Entry point for UEFI. Note: name and calling convention are critical.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    let st = unsafe { &*system_table };
    // In efi_main, after getting the system_table pointer
    *UEFI_SYSTEM_TABLE.lock() = Some(UefiSystemTablePtr(system_table));
    let bs = unsafe { &*st.boot_services };
    uefi_print(st, "bellows: bootloader started\n");

    if let Err(msg) = init_heap(bs) {
        uefi_print(st, msg);
        panic!("Failed to initialize heap.");
    }

    init_gop(st);

    // Read the kernel file before exiting boot services.
    let (efi_image_phys, efi_image_size) = match read_efi_file(st) {
        Ok(info) => info,
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
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
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
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
            // Should not be reached.
            uefi_print(st, "\nJump failed.\n");
            panic!("Jump failed.");
        }
        Err(err) => {
            uefi_print(st, err);
            uefi_print(st, "\nHalting.\n");
            panic!("Failed to exit boot services and jump.");
        }
    }
}
