//! Bellows UEFI bootloader

#![no_std]
#![no_main]
#![cfg_attr(target_os = "uefi", feature(alloc_error_handler))]
#![feature(never_type)]
extern crate alloc;

petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

static KERNEL_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kernel.bin"));

mod loader;

use loader::{exit_boot_services_and_jump, init_heap, load_efi_image};
use petroleum::common::EfiSystemTable;

#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
) -> ! {
    if image_handle == 0 {
        panic!("Invalid image_handle");
    }

    petroleum::init_uefi_system_table(system_table);
    petroleum::bootloader_log!("UEFI_SYSTEM_TABLE initialized.");
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };
    petroleum::bootloader_log!("UEFI system table and boot services acquired.");
    petroleum::serial::UEFI_WRITER.lock().init(st.con_out);
    petroleum::bootloader_log!("UEFI_WRITER initialized.");
    petroleum::bootloader_log!("Bellows UEFI Bootloader starting...");
    petroleum::bootloader_log!(
        "Image Handle: {:#x}, System Table: {:#p}",
        image_handle,
        system_table
    );

    petroleum::bootloader_log!("Attempting to initialize heap...");
    init_heap(bs).expect("Heap initialization failed");
    petroleum::bootloader_log!("Heap initialized successfully.");

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
            petroleum::bootloader_log!("No directly addressable GOP mode; continuing headless.");
        }
    }
    petroleum::bootloader_log!("Graphics initialization complete.");

    let efi_image_file = KERNEL_BINARY;
    let efi_image_size = KERNEL_BINARY.len();
    petroleum::bootloader_log!("Bellows: Kernel file size check: {} bytes", efi_image_size);
    if efi_image_size == 0 {
        panic!("Kernel file is empty.");
    }

    petroleum::println!("Bellows: Kernel file loaded. Size: {}", efi_image_size);
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
    petroleum::println!("Bellows: About to exit boot services and jump to kernel.");
    match exit_boot_services_and_jump(
        image_handle,
        system_table,
        kernel_phys_start,
        kernel_entry_phys,
        entry,
    ) {
        Ok(_) => unreachable!(),
        Err(err) => {
            petroleum::println!("Failed to exit boot services: {:?}", err);
            panic!("Failed to exit boot services.");
        }
    }
}
