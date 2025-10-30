// bellows/src/main.rs

#![no_std]
#![no_main]
// #![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

use core::{ffi::c_void, ptr};

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use petroleum::println;
    // Simple panic handler for UEFI bootloader
    unsafe {
        petroleum::volatile_write!(0xB8000 as *mut u16, 0x1F20); // White ' ' on blue
        petroleum::volatile_write!(0xB8002 as *mut u16, 0x1F50); // White 'P' on blue
        let panic_msg = b"anic";
        for (i, &char_code) in panic_msg.iter().enumerate() {
            petroleum::volatile_write!((0xB8004 as *mut u16).add(i), 0x1F00 | char_code as u16);
        }
    }
    println!("Kernel Panic: {}", info);
    loop {}
}

use log;

// Embedded kernel binary
static KERNEL_BINARY: &[u8] = include_bytes!("kernel.bin");
// Import Port for direct I/O

mod loader;

use loader::{exit_boot_services_and_jump, init_heap, load_efi_image};

use petroleum::common::{EfiGraphicsPixelFormat, EfiSystemTable, FullereneFramebufferConfig};

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: usize, system_table: *mut EfiSystemTable) -> ! {
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
    petroleum::serial::_print(format_args!("Heap initialized successfully.\n"));
    petroleum::println!("Bellows: Heap OK.");

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

/// Installs a basic VGA framebuffer configuration for UEFI environments when GOP is not available.
/// Provides a fallback framebuffer configuration that the kernel can use.
///

fn install_vga_framebuffer_config(st: &EfiSystemTable) {
    petroleum::println!("Installing VGA framebuffer config for UEFI...");

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

    // Save to global instead of installing config table to avoid hang
    petroleum::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| spin::Mutex::new(Some(config)));

    petroleum::println!("VGA framebuffer config saved globally successfully.");
}
