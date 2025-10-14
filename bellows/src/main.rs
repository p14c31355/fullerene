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
    EfiGraphicsPixelFormat, EfiStatus, EfiSystemTable, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
    FullereneFramebufferConfig,
};

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    petroleum::common_panic!(_info);
}

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
    petroleum::serial::_print(format_args!("Bellows: efi_main entered.\n"));

    debug_print_str("Main: image_handle=0x");
    debug_print_hex(image_handle);
    debug_print_str(", system_table=0x");
    debug_print_hex(system_table as usize);
    debug_print_str("\n");

    // Before setting UEFI_SYSTEM_TABLE
    if image_handle == 0 {
        panic!("Invalid image_handle");
    }

    petroleum::init_uefi_system_table(system_table);
    petroleum::serial::_print(format_args!("Bellows: UEFI_SYSTEM_TABLE initialized.\n"));
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    petroleum::serial::_print(format_args!(
        "Bellows: UEFI system table and boot services acquired.\n"
    ));

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

    // Initialize graphics protocols for framebuffer setup
    petroleum::serial::_print(format_args!(
        "Attempting to initialize graphics protocols...\n"
    ));
    match petroleum::init_graphics_protocols(st) {
        Some(config) => {
            petroleum::println!(
                "Bellows: Graphics framebuffer initialized at {:#x} ({}x{}).",
                config.address,
                config.width,
                config.height
            );
        }
        None => {
            petroleum::println!(
                "Bellows: No graphics protocols found, initializing VGA text mode."
            );
            init_basic_vga_text_mode();
            // For UEFI fallback, try to install a basic VGA framebuffer config for kernel use
            install_vga_framebuffer_config(st);
            petroleum::println!(
                "Bellows: VGA framebuffer config installed, continuing with kernel load."
            );
        }
    }
    petroleum::serial::_print(format_args!("Graphics initialization complete.\n"));
    petroleum::println!("Bellows: Graphics initialized."); // Debug print after graphics initialization

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
    petroleum::serial::_print(format_args!("Bellows: EFI image loaded.\n"));

    petroleum::println!("Bellows: Kernel loaded from embedded binary.");

    petroleum::serial::_print(format_args!(
        "Exiting boot services and jumping to kernel...\n"
    ));
    // Exit boot services and jump to the kernel.
    petroleum::println!("Bellows: About to exit boot services and jump to kernel."); // Debug print just before the call
    debug_print_str("Before match exit_boot_services_and_jump.\n");
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

fn init_basic_vga_text_mode() {
    petroleum::serial::_print(format_args!("Basic VGA text mode initialization...\n"));

    // Detect and initialize VGA graphics for Cirrus devices
    petroleum::graphics::detect_and_init_vga_graphics();

    petroleum::serial::_print(format_args!(
        "Basic VGA text mode initialized as fallback.\n"
    ));
}

/// Attempts to initialize the Universal Graphics Adapter (UGA) protocol as a fallback.
fn try_uga_protocol(st: &EfiSystemTable) -> bool {
    // UGA GUID: {982c298b-f4fa-41cb-b838-777ba2482113}
    let uga_guid = petroleum::common::EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID;

    let bs = unsafe { &*st.boot_services };
    let mut uga: *mut c_void = ptr::null_mut();

    let status = unsafe { (bs.locate_protocol)(uga_guid.as_ptr(), ptr::null_mut(), &mut uga) };

    if EfiStatus::from(status) != EfiStatus::Success || uga.is_null() {
        petroleum::serial::_print(format_args!(
            "UGA protocol not available (status: {:#x})\n",
            status
        ));
        return false;
    }

    petroleum::serial::_print(format_args!(
        "UGA protocol found, attempting to initialize...\n"
    ));

    // Note: UGA protocol is deprecated, but some older EFI implementations might support it
    // For now, we'll just return true if found, and let the kernel handle it
    // This is a placeholder for future UGA implementation if needed

    true
}

/// Installs a basic VGA framebuffer configuration for UEFI environments when GOP is not available.
/// Provides a fallback framebuffer configuration that the kernel can use.
fn install_vga_framebuffer_config(st: &EfiSystemTable) {
    // Create a debug logging macro to clean up unsafe blocks
    macro_rules! vga_debug_log {
        ($message:expr, $($args:tt)*) => {
            #[cfg(debug_assertions)]
            petroleum::serial::_print(format_args!($message, $($args)*));
        };
        ($message:expr) => {
            #[cfg(debug_assertions)]
            petroleum::serial::_print(format_args!($message));
        };
    }

    petroleum::println!("Installing VGA framebuffer config table for UEFI...");
    vga_debug_log!("VGA: About to create config...\n");

    // Create an improved VGA-compatible framebuffer config
    // Use higher resolution VGA modes for better compatibility and to prevent logo scattering
    let config = FullereneFramebufferConfig {
        address: 0xA0000,                                     // Standard VGA memory address
        width: 800,  // Higher resolution to prevent logo scattering
        height: 600, // Higher resolution for better display
        pixel_format: EfiGraphicsPixelFormat::PixelFormatMax, // Special marker for VGA mode
        bpp: 8,
        stride: 800, // Match width for VGA modes
    };

    #[cfg(debug_assertions)]
    petroleum::serial::_print(format_args!(
        "VGA: Created config - address: {:#x}, width: {}, height: {}, bpp: {}\n",
        config.address, config.width, config.height, config.bpp
    ));

    let config_ptr = Box::leak(Box::new(config));

    #[cfg(debug_assertions)]
    petroleum::serial::_print(format_args!("VGA: Config boxed and leaked\n"));

    let bs = unsafe { &*st.boot_services };
    #[cfg(debug_assertions)]
    petroleum::serial::_print(format_args!("VGA: Got boot services\n"));

    let status = unsafe {
        (bs.install_configuration_table)(
            FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            config_ptr as *const _ as *mut c_void,
        )
    };

    if EfiStatus::from(status) == EfiStatus::Success {
        petroleum::println!("VGA framebuffer config table installed successfully.");
    } else {
        petroleum::serial::_print(format_args!(
            "VGA: Installation failed, recovering memory\n"
        ));
        let _ = unsafe { Box::from_raw(config_ptr) };
        petroleum::println!(
            "Failed to install VGA framebuffer config table (status: {:#x})",
            status
        );
    }
}

// Note: This function is unused as GOP initialization is now handled by petroleum::init_graphics_protocols
// Removing to reduce dead code and improve maintainability
/*
fn init_gop(st: &EfiSystemTable) {
    let bs = unsafe { &*st.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    let status = (bs.locate_protocol)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut gop as *mut _ as *mut *mut c_void,
    );

    debug_print_str("GOP locate_protocol status: 0x");
    debug_print_hex(status as usize);
    debug_print_str("\n");

    if EfiStatus::from(status) != EfiStatus::Success || gop.is_null() {
        petroleum::serial::_print(format_args!(
            "Failed to locate GOP protocol (status: {:#x}), trying alternative methods.\n", status
        ));

        // Try alternative graphics protocols or better GOP detection
        petroleum::serial::_print(format_args!("GOP not found, trying alternative protocols...\n"));

        // Attempt to find UGA (Universal Graphics Adapter) protocol as fallback
        if try_uga_protocol(st) {
            petroleum::serial::_print(format_args!("UGA protocol found and initialized.\n"));
            return;
        }

        // If no graphics protocols available, fall back to basic VGA text mode
        petroleum::serial::_print(format_args!("No graphics protocols available, using VGA text mode...\n"));
        init_basic_vga_text_mode();

        petroleum::serial::_print(format_args!("Basic VGA text mode initialized as fallback.\n"));
        return;
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        petroleum::serial::_print(format_args!("GOP mode pointer is null, skipping.\n"));
        return;
    }

    let mode_ref = unsafe { &*gop_ref.mode };

    // Set GOP to graphics mode if not already
    // Try mode 0 (typically 1024x768 or similar graphics mode)
    let target_mode = 0;
    let current_mode = mode_ref.mode as usize;
    if target_mode != current_mode {
        let modes_to_try = [target_mode as u32, 1];
        let mut mode_set_successfully = false;

        for &mode in &modes_to_try {
            petroleum::serial::_print(format_args!(
                "GOP: Attempting to set mode {} (graphics mode) (currently {})
",
                mode, current_mode
            ));
            let status = (gop_ref.set_mode)(gop, mode);
            if EfiStatus::from(status) == EfiStatus::Success {
                petroleum::serial::_print(format_args!("GOP: Mode {} set successfully.\n", mode));
                mode_set_successfully = true;
                break;
            } else {
                petroleum::serial::_print(format_args!(
                    "GOP: Failed to set mode {}, status: {:#x}.\n",
                    mode, status
                ));
            }
        }

        if !mode_set_successfully {
            petroleum::serial::_print(format_args!(
                "GOP: Failed to set any graphics mode, skipping GOP initialization.\n"
            ));
            return;
        }
    } else {
        petroleum::serial::_print(format_args!("GOP: Mode {} already set\n", target_mode));
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
        pixel_format: info.pixel_format,
        bpp: 32, // Assuming 32bpp for supported modes. More robust handling is needed for other formats.
        stride: info.pixels_per_scan_line,
    });

    let config_ptr = Box::leak(config);

    let status = (bs.install_configuration_table)(
        FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
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

    petroleum::serial::serial_log(format_args!(
        "GOP set: {}x{} @ {:#x}",
        info.horizontal_resolution, info.vertical_resolution, fb_addr
    ));
    petroleum::serial::_print(format_args!("GOP: Framebuffer initialized and cleared\n"));
}
*/
