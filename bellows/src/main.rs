//! Bellows UEFI bootloader

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(never_type)]
extern crate alloc;

petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

static KERNEL_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kernel.bin"));

mod loader;

use core::ffi::c_void;
use core::ptr;
use loader::{exit_boot_services_and_jump, init_heap, load_efi_image};
use petroleum::common::{
    EfiGraphicsPixelFormat, EfiSystemTable, EfiSimpleFileSystem, FullereneFramebufferConfig,
};
use petroleum::common::uefi::EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID;

// ── Full EFI_FILE_PROTOCOL (petroleum only has a minimal subset) ──

/// Complete EFI_FILE_PROTOCOL with `write` and `flush` (UEFI 2.x §12.5).
#[repr(C)]
struct EfiFileFull {
    _revision: u64,
    open: extern "efiapi" fn(*mut EfiFileFull, *mut *mut EfiFileFull, *const u16, u64, u64) -> usize,
    close: extern "efiapi" fn(*mut EfiFileFull) -> usize,
    _delete: usize,
    read: extern "efiapi" fn(*mut EfiFileFull, *mut u64, *mut u8) -> usize,
    write: extern "efiapi" fn(*mut EfiFileFull, *mut u64, *const u8) -> usize,
    _get_position: usize,
    _set_position: usize,
    get_info: extern "efiapi" fn(*mut EfiFileFull, *const u8, *mut usize, *mut c_void) -> usize,
    _set_info: usize,
    flush: extern "efiapi" fn(*mut EfiFileFull) -> usize,
}

/// Full EFI_SIMPLE_FILE_SYSTEM_PROTOCOL with write-compatible open_volume.
#[repr(C)]
struct EfiSimpleFileSystemFull {
    _revision: u64,
    open_volume:
        extern "efiapi" fn(*mut EfiSimpleFileSystemFull, *mut *mut EfiFileFull) -> usize,
}

/// Cast a `*mut EfiSimpleFileSystem` to `*mut EfiSimpleFileSystemFull`.
/// Both are `#[repr(C)]` with the same ABI layout; this is safe because
/// the real firmware provides the full protocol.
fn sfs_to_full(p: *mut EfiSimpleFileSystem) -> *mut EfiSimpleFileSystemFull {
    p as *mut EfiSimpleFileSystemFull
}

// ── Hardware report writer ─────────────────────────────────────────

/// Write a hardware report to the first available FAT volume on USB.
///
/// This is called BEFORE `ExitBootServices`, so we can use UEFI
/// `SimpleFileSystemProtocol` to write directly to the USB stick.
fn write_hardware_report_to_usb(st: &EfiSystemTable) {
    use core::fmt::Write;

    let bs = unsafe { &*st.boot_services };
    let mut handles_buf: *mut usize = ptr::null_mut();
    let mut handle_count: usize = 0;

    // Locate all handles that support SimpleFileSystemProtocol
    let status = (bs.locate_handle_buffer)(
        2u32, // ByProtocol
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut handle_count,
        &mut handles_buf,
    );
    if status != 0 {
        petroleum::bootloader_log!("HWReport: locate_handle_buffer failed (status={:#x})", status);
        return;
    }

    petroleum::bootloader_log!(
        "HWReport: found {} handle(s) with SimpleFileSystemProtocol",
        handle_count
    );

    for i in 0..handle_count {
        let handle = unsafe { *(handles_buf.add(i)) };

        let mut sfs: *mut EfiSimpleFileSystemFull = ptr::null_mut();
        let status = (bs.handle_protocol)(
            handle,
            EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
            &mut sfs as *mut *mut _ as *mut *mut c_void,
        );
        if status != 0 || sfs.is_null() {
            continue;
        }

        // Open the root directory of the volume
        let mut root: *mut EfiFileFull = ptr::null_mut();
        let status = unsafe { ((*sfs).open_volume)(sfs, &mut root) };
        if status != 0 || root.is_null() {
            continue;
        }

        // Create or overwrite /hardware_report.txt
        // We need a UTF-16 path: "hardware_report.txt"
        let path_utf16: [u16; 32] = {
            let name = "hardware_report.txt";
            let mut buf = [0u16; 32];
            for (i, c) in name.encode_utf16().enumerate() {
                if i < buf.len() - 1 {
                    buf[i] = c;
                }
            }
            buf
        };

        let mut file: *mut EfiFileFull = ptr::null_mut();
        let open_status = unsafe {
            ((*root).open)(
                root,
                &mut file,
                path_utf16.as_ptr(),
                0x00000000_00000003u64, // EFI_FILE_MODE_READ | EFI_FILE_MODE_WRITE | EFI_FILE_MODE_CREATE
                0,
            )
        };
        unsafe {
            let _ = ((*root).close)(root);
        }
        if open_status != 0 || file.is_null() {
            continue;
        }

        // Build the report
        let mut report = alloc::string::String::with_capacity(4096);
        let _ = writeln!(report, "=== Fullerene Hardware Report ===");
        let _ = writeln!(report, "");

        // GOP / framebuffer info
        if let Some(fb) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|m| *m.lock())
        {
            let _ = writeln!(report, "[GOP]");
            let _ = writeln!(report, "address=0x{:016x}", fb.address);
            let _ = writeln!(report, "width={}", fb.width);
            let _ = writeln!(report, "height={}", fb.height);
            let _ = writeln!(report, "bpp={}", fb.bpp);
            let _ = writeln!(report, "stride={}", fb.stride);
            let _ = writeln!(report, "pixel_format={:?}", fb.pixel_format);
            let _ = writeln!(report, "");
        } else {
            let _ = writeln!(report, "[GOP] (not available)");
            let _ = writeln!(report, "");
        }

        // ACPI — read from system table configuration table entries
        // (Simplified: we report what's available via known GUIDs.)
        let _ = writeln!(report, "[ACPI]");
        let _ = writeln!(report, "See RSDT/XSDT from kernel logs.");
        let _ = writeln!(report, "");

        // PCI — attempt a basic scan via EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL
        // (We skip the full scan here; the kernel does a proper one.
        //  This is just a placeholder that can be expanded later.)
        let _ = writeln!(report, "[PCI]");
        let _ = writeln!(report, "Full PCI scan is delegated to kernel (see bootlog).");
        let _ = writeln!(report, "");

        let _ = writeln!(report, "=== End Hardware Report ===");
        let _ = writeln!(report, "");

        // Write to file
        let report_bytes = report.as_bytes();
        let mut write_size = report_bytes.len() as u64;
        let write_status = unsafe { ((*file).write)(file, &mut write_size, report_bytes.as_ptr()) };
        if write_status == 0 {
            let _ = unsafe { ((*file).flush)(file) };
            petroleum::bootloader_log!(
                "HWReport: wrote {} bytes to hardware_report.txt on USB",
                write_size
            );
        } else {
            petroleum::bootloader_log!(
                "HWReport: write failed (status={:#x})",
                write_status
            );
        }
        unsafe {
            let _ = ((*file).close)(file);
        }
        break; // One report is enough — use the first writeable volume.
    }

    // Free the handle buffer
    if !handles_buf.is_null() {
        unsafe {
            let _ = (bs.free_pool)(handles_buf as *mut c_void);
        }
    }
}

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
            petroleum::bootloader_log!("No graphics protocols found, initializing VGA text mode.");
            init_basic_vga_text_mode();
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

    // ── Write hardware report to USB before ExitBootServices ────
    petroleum::bootloader_log!("Writing hardware report to USB...");
    write_hardware_report_to_usb(st);

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

fn init_basic_vga_text_mode() {
    petroleum::println!("Basic VGA text mode initialization...");
    petroleum::graphics::detect_and_init_vga_graphics();
    petroleum::println!("Basic VGA text mode initialized as fallback.");
}

fn install_vga_framebuffer_config(_st: &EfiSystemTable) {
    petroleum::println!("Installing VGA framebuffer config for UEFI...");
    let config = FullereneFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        pixel_format: EfiGraphicsPixelFormat::PixelFormatMax,
        bpp: 8,
        stride: 320,
    };
    petroleum::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| spin::Mutex::new(Some(config)));
    petroleum::println!("VGA framebuffer config saved globally successfully.");
}
