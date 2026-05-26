//! Bellows UEFI bootloader
//!
//! This crate implements the UEFI bootloader phase of the Fullerene OS.
//! It is responsible for:
//! - Initializing UEFI boot services and graphics
//! - Loading the kernel ELF binary embedded in the bootloader
//! - Setting up page tables and jumping to the kernel
//! - Providing a VGA fallback when GOP is unavailable

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

// Define panic and alloc error handlers using petroleum's macros
petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

// Embedded kernel binary
static KERNEL_BINARY: &[u8] = include_bytes!("kernel_final.bin");
// Import Port for direct I/O

mod loader;

use loader::{exit_boot_services_and_jump, init_heap, load_efi_image};

use petroleum::common::{EfiGraphicsPixelFormat, EfiSystemTable, FullereneFramebufferConfig};

/// Main entry point of the bootloader.
///
/// This function is the `start` attribute as defined in the `Cargo.toml`.
///
/// # Safety
///
/// This function is called by the UEFI firmware and must adhere to the UEFI calling convention.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
) -> ! {
    // Before setting UEFI_SYSTEM_TABLE
    if image_handle == 0 {
        panic!("Invalid image_handle");
    }

    petroleum::init_uefi_system_table(system_table);
    petroleum::bootloader_log!("UEFI_SYSTEM_TABLE initialized.");
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };

    petroleum::bootloader_log!("UEFI system table and boot services acquired.");

    // Initialize the serial writer with the console output pointer.
    petroleum::serial::UEFI_WRITER.lock().init(st.con_out);
    petroleum::bootloader_log!("UEFI_WRITER initialized.");

    petroleum::bootloader_log!("Bellows UEFI Bootloader starting...");
    petroleum::bootloader_log!(
        "Image Handle: {:#x}, System Table: {:#p}",
        image_handle,
        system_table
    );
    // Initialize heap
    petroleum::bootloader_log!("Attempting to initialize heap...");
    init_heap(bs).expect("Heap initialization failed");
    petroleum::bootloader_log!("Heap initialized successfully.");

    // Initialize graphics protocols for framebuffer setup
    petroleum::bootloader_log!("Attempting to initialize graphics protocols...");
    match petroleum::init_graphics_protocols(st) {
        Some(config) => {
            petroleum::bootloader_log!(
                "Graphics framebuffer initialized at {:#x} ({}x{}).",
                config.address,
                config.width,
                config.height
            );
        }
        None => {
            petroleum::bootloader_log!("No graphics protocols found, initializing VGA text mode.");
            init_basic_vga_text_mode();
            // For UEFI fallback, try to install a basic VGA framebuffer config for kernel use
            install_vga_framebuffer_config(st);
            petroleum::bootloader_log!(
                "VGA framebuffer config installed, continuing with kernel load."
            );
        }
    }
    petroleum::bootloader_log!("Graphics initialization complete.");

    let efi_image_file = KERNEL_BINARY;
    let efi_image_size = KERNEL_BINARY.len();

    petroleum::bootloader_log!("Bellows: Kernel file size check: {} bytes", efi_image_size);
    if efi_image_size > 0 {
        let first_bytes = &KERNEL_BINARY[..core::cmp::min(efi_image_size, 4)];
        petroleum::bootloader_log!(
            "Bellows: First 4 bytes: {:02x?}, {:02x?}, {:02x?}, {:02x?}",
            first_bytes[0],
            first_bytes[1],
            first_bytes[2],
            first_bytes[3]
        );
    }
    if efi_image_size == 0 {
        petroleum::bootloader_log!("Bellows: Kernel file is empty!");
        panic!("Kernel file is empty.");
    }

    petroleum::println!("Bellows: Kernel file loaded. Size: {}", efi_image_size);
    petroleum::println!("Attempting to load EFI image...");
    // Load the kernel and get its entry point.
    let (kernel_phys_start, kernel_entry_phys, entry) = match load_efi_image(
        st,
        efi_image_file,
        petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64() as usize,
    ) {
        Ok((phys, phys_entry, e)) => {
            petroleum::println!(
                "EFI image loaded successfully. Entry point: {:#p}, Phys entry: {:#x}, Phys base: {:#x}",
                e as *const (),
                phys_entry,
                phys.as_u64()
            );
            (phys, phys_entry, e)
        }
        Err(err) => {
            petroleum::println!("Failed to load EFI image: {:?}", err);
            panic!("Failed to load EFI image.");
        }
    };
    petroleum::println!("Bellows: EFI image loaded.");
    petroleum::println!("Bellows: Kernel loaded from embedded binary.");
    petroleum::println!("Exiting boot services and jumping to kernel...");
    // Exit boot services and jump to the kernel.
    petroleum::println!("Bellows: About to exit boot services and jump to kernel."); // Debug print just before the call
    match exit_boot_services_and_jump(
        image_handle,
        system_table,
        kernel_phys_start,
        kernel_entry_phys,
        entry,
    ) {
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
    petroleum::println!("Basic VGA text mode initialization...");

    // Detect and initialize VGA graphics for Cirrus devices
    petroleum::graphics::detect_and_init_vga_graphics();

    petroleum::println!("Basic VGA text mode initialized as fallback.");
}

// Standard VGA memory address for legacy VGA framebuffer
const VGA_MEMORY_ADDRESS: u64 = 0xA0000;
// VGA fallback resolution width
const VGA_FALLBACK_WIDTH: u32 = 800;
// VGA fallback resolution height
const VGA_FALLBACK_HEIGHT: u32 = 600;
// VGA fallback bits per pixel
const VGA_FALLBACK_BPP: u32 = 8;

/// Installs a basic VGA framebuffer configuration for UEFI environments when GOP is not available.
/// Provides a fallback framebuffer configuration that the kernel can use.
fn install_vga_framebuffer_config(_st: &EfiSystemTable) {
    petroleum::println!("Installing VGA framebuffer config for UEFI...");

    // Create an improved VGA-compatible framebuffer config
    // Use higher resolution VGA modes for better compatibility and to prevent logo scattering
    let config = FullereneFramebufferConfig {
        address: VGA_MEMORY_ADDRESS,
        width: VGA_FALLBACK_WIDTH,
        height: VGA_FALLBACK_HEIGHT,
        pixel_format: EfiGraphicsPixelFormat::PixelFormatMax,
        bpp: VGA_FALLBACK_BPP,
        stride: VGA_FALLBACK_WIDTH,
    };

    #[cfg(debug_assertions)]
    petroleum::serial::_print(format_args!(
        "VGA: Created config - address: {:#x}, width: {}, height: {}, bpp: {}\n",
        config.address, config.width, config.height, config.bpp
    ));

    // Save to global instead of installing config table to avoid hang
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: Bellows: Initializing framebuffer config static...\n"
    );
    petroleum::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| spin::Mutex::new(Some(config)));
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: Bellows: Framebuffer config static initialized.\n"
    );

    petroleum::println!("VGA framebuffer config saved globally successfully.");
}
