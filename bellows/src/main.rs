// bellows/src/main.rs

#![no_std]
#![no_main]
// #![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

use alloc::boxed::Box;

use core::{ffi::c_void, ptr};

// Embedded kernel binary
static KERNEL_BINARY: &[u8] = include_bytes!("kernel.bin");
// Import Port for direct I/O

mod loader;

use loader::{exit_boot_services_and_jump, heap::init_heap, pe::load_efi_image};
use petroleum::serial::{debug_print_hex, debug_print_str_to_com1 as debug_print_str};

use petroleum::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EfiGraphicsOutputModeInformation, EfiGraphicsOutputProtocol,
    EfiStatus, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, FullereneFramebufferConfig,
};

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    petroleum::println!("Bellows: efi_main entered."); // Early debug print

    debug_print_str("Main: image_handle=0x");
    debug_print_hex(image_handle);
    debug_print_str(", system_table=0x");
    debug_print_hex(system_table as usize);
    debug_print_str("\n");

    // Before setting UEFI_SYSTEM_TABLE
    if image_handle == 0 {
        panic!("Invalid image_handle");
    }

    let _ = petroleum::UEFI_SYSTEM_TABLE
        .lock()
        .insert(petroleum::UefiSystemTablePtr(system_table));
    petroleum::println!("Bellows: UEFI_SYSTEM_TABLE initialized."); // Debug print after initialization
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    petroleum::println!("Bellows: UEFI system table and boot services acquired."); // Early debug print

    // Initialize the serial writer with the console output pointer.
    petroleum::serial::UEFI_WRITER.lock().init(st.con_out);
    petroleum::println!("Bellows: UEFI_WRITER initialized."); // Debug print after UEFI_WRITER init

    petroleum::println!("Bellows UEFI Bootloader starting...");
    petroleum::println!("Bellows: 'Bellows UEFI Bootloader starting...' printed."); // Debug print after println!
    petroleum::serial::_print(format_args!("Attempting to initialize GOP...\n"));
    petroleum::println!("Image Handle: {:#x}", image_handle);
    petroleum::println!("System Table: {:#p}", system_table);
    // Initialize heap
    petroleum::serial::_print(format_args!("Attempting to initialize heap...\n"));
    init_heap(bs).expect("Heap initialization failed");
    debug_print_str("Main: Heap init returned OK.\n");
    petroleum::serial::_print(format_args!("Heap initialized successfully.\n"));
    debug_print_str("Main: After Heap initialized print.\n");
    petroleum::println!("Bellows: Heap OK.");
    debug_print_str("Main: After Heap OK println.\n");
    debug_print_str("Main: About to call init_gop.\n");
    init_gop(st);
    debug_print_str("Main: init_gop returned.\n");
    petroleum::serial::_print(format_args!("GOP initialized successfully.\n"));
    petroleum::println!("Bellows: GOP initialized."); // Debug print after GOP initialization

    petroleum::println!("Bellows: Reading kernel from embedded binary...");
    let efi_image_file = KERNEL_BINARY;
    let efi_image_size = KERNEL_BINARY.len();

    if efi_image_size == 0 {
        petroleum::println!("Bellows: Kernel file is empty!");
        petroleum::println!("Kernel file is empty.");
        panic!("Kernel file is empty.");
    }

    petroleum::println!("Bellows: Kernel file loaded.");
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
            panic!("Failed to load EFI image.");
        }
    };
    petroleum::println!("Bellows: EFI image loaded."); // Debug print after load_efi_image

    petroleum::println!("Bellows: Kernel loaded from embedded binary.");

    petroleum::serial::_print(format_args!(
        "Exiting boot services and jumping to kernel...\n"
    ));
    // Exit boot services and jump to the kernel.
    petroleum::println!("Bellows: About to exit boot services and jump to kernel."); // Debug print just before the call
    match exit_boot_services_and_jump(image_handle, system_table, entry) {
        Ok(_) => {
            unreachable!(); // This branch should never be reached if the function returns '!'
        }
        Err(err) => {
            petroleum::println!("Failed to exit boot services: {:?}", err);
            panic!("Failed to exit boot services.");
        }
    }
}

/// Initializes the Graphics Output Protocol (GOP) for framebuffer access.
fn init_gop(st: &EfiSystemTable) {
    debug_print_str("GOP: init_gop entered.\n");
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

    // Try to set a preferred graphics mode (1024x768 or highest available)
    let max_mode = mode_ref.max_mode;
    petroleum::serial::_print(format_args!(
        "GOP: Max modes: {}, Current mode: {}\n",
        max_mode, mode_ref.mode as usize
    ));

        let mut target_mode: Option<usize> = None;
    let mut best_resolution: u64 = 0;
    for mode_num in 0..max_mode {
        let mut size = 0;
        let status = (gop_ref.query_mode)(gop, mode_num, &mut size, ptr::null_mut());
        if EfiStatus::from(status) != EfiStatus::BufferTooSmall {
            continue;
        }

        let mut mode_info = alloc::vec![0u8; size];
        let status = (gop_ref.query_mode)(
            gop,
            mode_num,
            &mut size,
            mode_info.as_mut_ptr() as *mut c_void,
        );
        if EfiStatus::from(status) == EfiStatus::Success {
            let info: &EfiGraphicsOutputModeInformation =
                unsafe { &*(mode_info.as_ptr() as *const EfiGraphicsOutputModeInformation) };
            petroleum::serial::_print(format_args!(
                "GOP: Mode {}: {}x{}, format: {}\n",
                mode_num,
                info.horizontal_resolution,
                info.vertical_resolution,
                info.pixel_format as u32
            ));

            // Prefer 1024x768, or highest resolution if not available
            if info.horizontal_resolution == 1024 && info.vertical_resolution == 768 {
                target_mode = Some(mode_num as usize);
                break;
            }
            if info.horizontal_resolution >= 1024 && info.vertical_resolution >= 768 {
                let resolution = info.horizontal_resolution as u64 * info.vertical_resolution as u64;
                if resolution > best_resolution {
                    best_resolution = resolution;
                    target_mode = Some(mode_num as usize);
                }
            }
        }
    }

    // Set the target mode if different from current
    if let Some(mode_num) = target_mode {
        let current_mode = mode_ref.mode as usize;
        if mode_num != current_mode {
            petroleum::serial::_print(format_args!(
                "GOP: Setting mode {} (currently {})\n",
                mode_num, current_mode
            ));
            let status = (gop_ref.set_mode)(gop, mode_num as u32);
            if EfiStatus::from(status) != EfiStatus::Success {
                petroleum::serial::_print(format_args!(
                    "GOP: Failed to set mode, status: {:#x}\n",
                    status
                ));
            } else {
                petroleum::serial::_print(format_args!("GOP: Mode set successfully\n"));
            }
        } else {
            petroleum::serial::_print(format_args!("GOP: Mode {} already set\n", mode_num));
        }
    } else {
        petroleum::serial::_print(format_args!("GOP: No suitable mode found\n"));
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        petroleum::serial::_print(format_args!("GOP mode info pointer is null, skipping.\n"));
        return;
    }

    let info = unsafe { &*mode_ref.info };

    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    if fb_addr == 0 || fb_size == 0 {
        petroleum::serial::_print(format_args!("GOP framebuffer info is invalid, skipping.\n"));
        return;
    }

    petroleum::serial::_print(format_args!(
        "GOP: Framebuffer at {:#x}, size: {}KB, resolution: {}x{}, stride: {}\n",
        fb_addr,
        fb_size / 1024,
        info.horizontal_resolution,
        info.vertical_resolution,
        info.pixels_per_scan_line
    ));

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

    // Clear screen to black for better visibility
    unsafe {
        core::ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }

    petroleum::serial::_print(format_args!("GOP: Framebuffer initialized and cleared\n"));
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    petroleum::handle_panic(info)
}
