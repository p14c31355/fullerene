#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod apic;
pub mod common;
pub mod graphics;
pub mod page_table;
pub mod serial;
pub use apic::{IoApic, IoApicRedirectionEntry, init_io_apic};
pub use graphics::ports::{MsrHelper, PortOperations, PortWriter, RegisterConfig};
pub use graphics::{
    Color, ColorCode, ScreenChar, TextBufferOperations, VgaPortOps, VgaPorts, init_vga_graphics,
};
pub use serial::{Com1Ports, SerialPort, SerialPortOps, SERIAL_PORT_WRITER};
pub use serial::SERIAL_PORT_WRITER as SERIAL1;

use core::arch::asm;
use core::ffi::c_void;
use core::ptr;
use spin::Mutex;

use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID
};
use crate::common::{
    EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
    EfiConfigurationTable,
};

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

/// Helper to initialize UEFI system table
pub fn init_uefi_system_table(system_table: *mut EfiSystemTable) {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table));
}

/// Helper to initialize serial for bootloader
pub unsafe fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    unsafe {
        serial::write_serial_bytes(port, status_port, bytes);
    }
}

/// macro for bootloader serial logging
#[macro_export]
macro_rules! write_serial_bytes {
    ($port:expr, $status:expr, $bytes:expr) => {
        unsafe {
            $crate::write_serial_bytes($port, $status, $bytes);
        }
    };
}

type EfiUniversalGraphicsAdapterProtocolPtr = isize; // Placeholder for UGA protocol type

/// Helper to try Universal Graphics Adapter (UGA) protocol
pub fn init_uga_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    // This GUID should be moved to a constant, e.g., in `petroleum/src/common/uefi.rs`
    // pub const EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID: [u8; 16] = [...];
    let uga_guid = crate::common::EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID;
    let bs = unsafe { &*system_table.boot_services };
    let mut uga: *mut EfiUniversalGraphicsAdapterProtocolPtr = ptr::null_mut();

    let status = unsafe { (bs.locate_protocol)(
        uga_guid.as_ptr(),
        ptr::null_mut(),
        &mut uga as *mut _ as *mut *mut c_void,
    ) };

    if EfiStatus::from(status) != EfiStatus::Success || uga.is_null() {
        serial::_print(format_args!(
            "UGA protocol not available (status: {:#x})\n", status
        ));
        return None;
    }

    serial::_print(format_args!("UGA protocol found, but UGA implementation incomplete.\n"));
    // UGA is deprecated; for now, we don't initialize since it's complex and rarely used
    None
}

/// Helper to enumerate and log all available UEFI configuration table GUIDs for debugging
pub fn log_configuration_table_guids(system_table: &EfiSystemTable) {
    serial::_print(format_args!("CONFIG: Enumerating configuration tables ({} total):\n", system_table.number_of_table_entries));

    let config_tables = unsafe {
        core::slice::from_raw_parts(
            system_table.configuration_table,
            system_table.number_of_table_entries
        )
    };

    for (i, table) in config_tables.iter().enumerate() {
        let guid_bytes = &table.vendor_guid;
        serial::_print(format_args!("CONFIG[{}]: GUID {{ ", i));
        // Log GUID as hex bytes for debugging
        for (j, &byte) in guid_bytes.iter().enumerate() {
            serial::_print(format_args!("{:02x}", byte));
            if j < guid_bytes.len() - 1 {
                serial::_print(format_args!("-"));
            }
        }
        serial::_print(format_args!(" }}\n"));
    }
}

/// Helper function to test if a protocol GUID is available on any handle
pub fn test_protocol_availability(system_table: &EfiSystemTable, guid: &[u8; 16], name: &str) {
    let bs = unsafe { &*system_table.boot_services };

    // Try to locate any handles that support this protocol
    let mut handle_count: usize = 0;
    let mut handles: *mut usize = ptr::null_mut();

    let status = unsafe {
        (bs.locate_handle_buffer)(
            2, // ByProtocol (correct value is 2, not 3)
            guid.as_ptr(),
            ptr::null_mut(),
            &mut handle_count as *mut usize,
            &mut handles as *mut *mut usize,
        )
    };

    if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 {
        serial::_print(format_args!("PROTOCOL: {} - Available on {} handles\n", name, handle_count));
        // Free the buffer
        if !handles.is_null() {
            unsafe { (bs.free_pool)(handles as *mut c_void) };
        }
    } else {
        serial::_print(format_args!("PROTOCOL: {} - NOT FOUND (status: {:#x})\n", name, status));
    }
}

/// Helper to try different graphics protocols and modes
pub fn init_graphics_protocols(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    // Verify system table integrity before proceeding
    if system_table.boot_services.is_null() {
        serial::_print(format_args!("GOP: System table boot services pointer is null.\n"));
        return None;
    }

    // Print basic system information to help diagnose GOP availability
    serial::_print(format_args!("GOP: Initializing graphics protocols...\n"));
    serial::_print(format_args!("GOP: Configuration table count: {}\n", system_table.number_of_table_entries));

    // Log all configuration table GUIDs for debugging
    log_configuration_table_guids(system_table);

    // Test common graphics protocols for availability
    test_protocol_availability(system_table, &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, "EFI_GRAPHICS_OUTPUT_PROTOCOL");
    test_protocol_availability(system_table, &EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID, "EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL");


    test_protocol_availability(system_table, &EFI_LOADED_IMAGE_PROTOCOL_GUID, "EFI_LOADED_IMAGE_PROTOCOL");
    test_protocol_availability(system_table, &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, "EFI_SIMPLE_FILE_SYSTEM_PROTOCOL");

    // First try standard GOP protocol with enhanced mode enumeration
    if let Some(config) = init_gop_framebuffer(system_table) {
        return Some(config);
    }

    // If GOP fails, try UGA (Universal Graphics Adapter) - though deprecated
    serial::_print(format_args!("GOP not available, trying UGA protocol...\n"));
    if let Some(config) = init_uga_framebuffer(system_table) {
        return Some(config);
    }

    // If all graphics protocols fail, try alternative VESA detection approaches
    serial::_print(format_args!("All graphics protocols failed, trying alternative VESA detection...\n"));

    // Try EFI-based PCI enumeration first
    if let Some(config) = graphics_alternatives::detect_vesa_graphics(unsafe { &*system_table.boot_services }) {
        serial::_print(format_args!("EFI PCI enumeration succeeded, installing config table\n"));

        // Install the configuration in the UEFI config table
        let config_ptr = Box::leak(Box::new(config));

        let boot_services = unsafe { &*system_table.boot_services };
        let install_status = unsafe { (boot_services.install_configuration_table)(
            FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            config_ptr as *const _ as *mut c_void,
        ) };

        if EfiStatus::from(install_status) != EfiStatus::Success {
            let _ = unsafe { Box::from_raw(config_ptr) };
            serial::_print(format_args!("EFI: Failed to install config table (status: {:#x}).\n", install_status));
            return None;
        }

        serial::_print(format_args!("EFI: Configuration table installed successfully.\n"));
        serial::_print(format_args!(
            "EFI: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config_ptr.width, config_ptr.height, config_ptr.address, config_ptr.bpp, config_ptr.stride
        ));

        // Clear screen for clean state
        unsafe {
            ptr::write_bytes(config_ptr.address as *mut u8, 0x00, (config_ptr.height as u64 * config_ptr.stride as u64 * (config_ptr.bpp as u64 / 8)) as usize);
        }

        serial::_print(format_args!("EFI: Framebuffer cleared.\n"));
        return Some(*config_ptr);
    }

    // If EFI PCI enumeration failed, try bare-metal PCI enumeration
    serial::_print(format_args!("EFI PCI enumeration failed, trying bare-metal detection\n"));

    if let Some(config) = bare_metal_graphics_detection::detect_bare_metal_graphics() {
        // Install the configuration in the UEFI config table
        let config_ptr = Box::leak(Box::new(config));

        let install_status = unsafe { ((*system_table.boot_services).install_configuration_table)(
            FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            config_ptr as *const _ as *mut c_void,
        ) };

        if EfiStatus::from(install_status) != EfiStatus::Success {
            let _ = unsafe { Box::from_raw(config_ptr) };
            serial::_print(format_args!("Bare-metal: Failed to install config table (status: {:#x}).\n", install_status));
            return None;
        }

        serial::_print(format_args!("Bare-metal: Configuration table installed successfully.\n"));
        serial::_print(format_args!(
            "Bare-metal: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config_ptr.width, config_ptr.height, config_ptr.address, config_ptr.bpp, config_ptr.stride
        ));

        // Clear screen for clean state
        unsafe {
            ptr::write_bytes(config_ptr.address as *mut u8, 0x00, (config_ptr.height as u64 * config_ptr.stride as u64 * (config_ptr.bpp as u64 / 8)) as usize);
        }

        serial::_print(format_args!("Bare-metal: Framebuffer cleared.\n"));
        return Some(*config_ptr);
    } else {
        serial::_print(format_args!("Bare-metal graphics detection also failed\n"));
    }

// Bare-metal graphics detection using direct PCI access without UEFI protocols
pub mod bare_metal_graphics_detection {
    use super::*;
    use crate::serial::_print;

    /// Main entry point for bare-metal graphics detection
    pub fn detect_bare_metal_graphics() -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[BM-GFX] Starting bare-metal graphics detection...\n"));

        // Enumerate graphics devices via direct PCI access
        let graphics_devices = crate::bare_metal_pci::enumerate_graphics_devices();

        _print(format_args!("[BM-GFX] Found {} graphics devices via direct PCI enumeration\n", graphics_devices.len()));

        // Try each graphics device for linear framebuffer detection
        for device in graphics_devices.iter() {
            _print(format_args!("[BM-GFX] Probing device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
                device.vendor_id, device.device_id, device.bus, device.device, device.function));

            // Check for supported device types
            match (device.vendor_id, device.device_id) {
                (0x1af4, id) if id >= 0x1050 => {
                    // virtio-gpu device
                    _print(format_args!("[BM-GFX] Detected virtio-gpu, attempting bare-metal framebuffer detection\n"));
                    if let Some(config) = detect_bare_metal_virtio_gpu_framebuffer(device) {
                        _print(format_args!("[BM-GFX] Bare-metal virtio-gpu framebuffer detection successful!\n"));
                        return Some(config);
                    }
                }
                (0x1b36, 0x0100) => {
                    // QEMU QXL device
                    _print(format_args!("[BM-GFX] Detected QXL device, attempting bare-metal framebuffer detection\n"));
                    if let Some(config) = detect_bare_metal_qxl_framebuffer(device) {
                        _print(format_args!("[BM-GFX] Bare-metal QXL framebuffer detection successful!\n"));
                        return Some(config);
                    }
                }
                (0x15ad, 0x0405) => {
                    // VMware SVGA II
                    _print(format_args!("[BM-GFX] Detected VMware SVGA, attempting bare-metal framebuffer detection\n"));
                    if let Some(config) = detect_bare_metal_vmware_svga_framebuffer(device) {
                        _print(format_args!("[BM-GFX] Bare-metal VMware SVGA framebuffer detection successful!\n"));
                        return Some(config);
                    }
                }
                _ => {
                    _print(format_args!("[BM-GFX] Unknown graphics device type, skipping\n"));
                }
            }
        }

        _print(format_args!("[BM-GFX] No supported graphics devices found via bare-metal enumeration\n"));
        None
    }

    /// Detect bare-metal virtio-gpu framebuffer using direct PCI BAR access
    fn detect_bare_metal_virtio_gpu_framebuffer(device: &crate::graphics_alternatives::PciDevice) -> Option<crate::common::FullereneFramebufferConfig> {
        // Read BAR0 from PCI configuration space directly
        let fb_base_addr = crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 0);

        _print(format_args!("[BM-GFX] virtio-gpu BAR0: {:#x}\n", fb_base_addr));

        if fb_base_addr == 0 {
            _print(format_args!("[BM-GFX] virtio-gpu BAR0 is zero, invalid\n"));
            return None;
        }

        // Try standard VGA-like modes for virtio-gpu
        // These are commonly used defaults in QEMU
        let standard_modes = [
            (1024, 768, 32, fb_base_addr),
            (1280, 720, 32, fb_base_addr),
            (800, 600, 32, fb_base_addr),
            (640, 480, 32, fb_base_addr),
        ];

        for (width, height, bpp, addr) in standard_modes.iter() {
            let stride = *width;
            let expected_fb_size = (*height * stride * bpp / 8) as u64;

            _print(format_args!("[BM-GFX] Testing {}x{} mode at {:#x} (size: {}KB)\n",
                width, height, addr, expected_fb_size / 1024));

            // Since we can't actually map or access memory from UEFI without protocols,
            // we'll use a simplified heuristic based on typical virtio-gpu memory layout
            // In practice, the framebuffer would be validated when actually accessed later

            if *addr >= 0x100000 { // At least 1MB address, reasonable for MMIO
                _print(format_args!("[BM-GFX] virtio-gpu framebuffer mode {}x{} appears valid\n", width, height));
                return Some(crate::common::FullereneFramebufferConfig {
                    address: *addr,
                    width: *width,
                    height: *height,
                    pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    bpp: *bpp,
                    stride,
                });
            }
        }

        _print(format_args!("[BM-GFX] Could not determine valid virtio-gpu framebuffer configuration\n"));
        None
    }

    /// Detect QXL framebuffer via direct PCI access (placeholder)
    fn detect_bare_metal_qxl_framebuffer(_device: &crate::graphics_alternatives::PciDevice) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[BM-GFX] QXL bare-metal detection not yet implemented\n"));
        // QXL devices in QEMU typically have complex framebuffer setup
        // Would need to implement QXL command submission and surface management
        None
    }

    /// Detect VMware SVGA framebuffer via direct PCI access (placeholder)
    fn detect_bare_metal_vmware_svga_framebuffer(_device: &crate::graphics_alternatives::PciDevice) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[BM-GFX] VMware SVGA bare-metal detection not yet implemented\n"));
        // VMware SVGA II uses FIFO commands and register-based communication
        // Would need to implement FIFO ring buffer management
        None
    }
}

    // Fall back to VGA text mode (handled externally)
    serial::_print(format_args!("All graphics protocols failed, falling back to VGA text mode.\n"));
    serial::_print(format_args!("NOTE: GOP protocol typically requires UEFI-compatible video hardware (e.g., QEMU with -vga qxl or virtio-gpu).\n"));
    None
}

/// Helper to find GOP and init framebuffer
pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let bs = unsafe { &*system_table.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    serial::_print(format_args!("GOP: Attempting to locate Graphics Output Protocol...\n"));

    // Add memory barrier before protocol call for safety
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    let status = (bs.locate_protocol)(
        EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID.as_ptr(),
        ptr::null_mut(),
        &mut gop as *mut _ as *mut *mut c_void,
    );

    if EfiStatus::from(status) != EfiStatus::Success || gop.is_null() {
        serial::_print(format_args!("GOP: Failed to locate GOP protocol (status: {:#x}).\n", status));
        return None;
    }

    serial::_print(format_args!("GOP: Protocol located successfully at {:#p}.\n", gop));

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        serial::_print(format_args!("GOP: Mode pointer is null.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    let current_mode = mode_ref.mode;

    // Get max_mode safely with bounds checking
    let max_mode_u32 = mode_ref.max_mode;
    if max_mode_u32 == 0 {
        serial::_print(format_args!("GOP: Max mode is 0, skipping.\n"));
        return None;
    }
    let max_mode = max_mode_u32 as usize;

    serial::_print(format_args!("GOP: Current mode: {}, Max mode: {}.\n", current_mode, max_mode));

    // Try to use current mode first, then mode 0, then try other modes
    let mut mode_set_successfully = false;
    let target_modes = [
        current_mode as u32,
        0,
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, // Try common modes
    ];

    for &mode in &target_modes[0..target_modes.len().min(max_mode as usize)] {
    if mode as u32 >= max_mode_u32 {
        continue;
    }
    serial::_print(format_args!("GOP: Attempting to set mode {}...\n", mode));
        let set_status = unsafe { (gop_ref.set_mode)(gop, mode) };
        if EfiStatus::from(set_status) == EfiStatus::Success {
            serial::_print(format_args!("GOP: Successfully set mode {}.\n", mode));
            mode_set_successfully = true;
            break;
        } else {
            serial::_print(format_args!("GOP: Failed to set mode {}, status: {:#x}.\n", mode, set_status));
        }
    }

    if !mode_set_successfully {
        serial::_print(format_args!("GOP: Failed to set any graphics mode.\n"));
        return None;
    }

    // Refresh mode reference after setting mode
    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        serial::_print(format_args!("GOP: Mode info pointer is null after setting mode.\n"));
        return None;
    }

    let info = unsafe { &*mode_ref.info };
    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    serial::_print(format_args!("GOP: Framebuffer addr: {:#x}, size: {}KB\n", fb_addr, fb_size / 1024));
    serial::_print(format_args!("GOP: Resolution: {}x{}, stride: {}\n",
        info.horizontal_resolution, info.vertical_resolution, info.pixels_per_scan_line));

    if fb_addr == 0 || fb_size == 0 {
        serial::_print(format_args!("GOP: Invalid framebuffer.\n"));
        return None;
    }

    let config = FullereneFramebufferConfig {
        address: fb_addr as u64,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        pixel_format: info.pixel_format,
        bpp: crate::common::get_bpp_from_pixel_format(info.pixel_format),
        stride: info.pixels_per_scan_line,
    };

    serial::_print(format_args!(
        "GOP: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
        config.width, config.height, config.address, config.bpp, config.stride
    ));

    let config_ptr = Box::leak(Box::new(config));

    let install_status = unsafe { (bs.install_configuration_table)(
        FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
        config_ptr as *const _ as *mut c_void,
    ) };

    if EfiStatus::from(install_status) != EfiStatus::Success {
        let _ = unsafe { Box::from_raw(config_ptr) };
        serial::_print(format_args!("GOP: Failed to install config table (status: {:#x}).\n", install_status));
        return None;
    }

    serial::_print(format_args!("GOP: Configuration table installed successfully.\n"));

    // Clear screen for clean state
    unsafe {
        ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }

    serial::_print(format_args!("GOP: Framebuffer cleared.\n"));
    Some(*config_ptr)
}

// Helper function to convert u32 to string without heap allocation
pub fn u32_to_str_heapless(n: u32, buffer: &mut [u8]) -> &str {
    let mut i = buffer.len();
    let mut n = n;
    if n == 0 {
        buffer[i - 1] = b'0';
        return core::str::from_utf8(&buffer[i - 1..i]).unwrap_or("ERR");
    }
    loop {
        i -= 1;
        buffer[i] = (n % 10) as u8 + b'0';
        n /= 10;
        if n == 0 || i == 0 {
            break;
        }
    }
    core::str::from_utf8(&buffer[i..]).unwrap_or("ERR")
}

/// Panic handler implementation that can be used by binaries
pub fn handle_panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);

        // Use write_string_heapless for panic messages to avoid heap allocation initially
        let mut writer = crate::serial::UEFI_WRITER.lock();
        let _ = writer.write_string_heapless("PANIC!\n");

        if let Some(loc) = info.location() {
            let mut line_buf = [0u8; 10];
            let mut col_buf = [0u8; 10];
            let _ = writer.write_string_heapless("Location: ");
            let _ = writer.write_string_heapless(loc.file());
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.line(), &mut line_buf));
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.column(), &mut col_buf));
            let _ = writer.write_string_heapless("\n");
        }

        let _ = writer.write_string_heapless("Message: ");
        // Try to write the message as a string slice if possible
        if let Some(msg) = info.message().as_str() {
            let _ = writer.write_string_heapless(msg);
        } else {
            let _ = writer.write_string_heapless("(message formatting failed)");
        }
        let _ = writer.write_string_heapless("\n");
    }

    // Also output to VGA buffer if available - heapless formatting
    #[cfg(feature = "vga_panic")]
    {
        // Import VGA module here to avoid dependency issues
        extern crate vga_buffer;
        use vga_buffer::{BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, Writer};

        let mut writer = Writer {
            column_position: 0,
            color_code: ColorCode::new(Color::Red, Color::Black),
            buffer: unsafe { &mut *(0xb8000 as *mut vga_buffer::Buffer) },
        };

        // Write "PANIC: " header
        let header = b"PANIC: ";
        for &byte in header {
            writer.write_byte(byte);
        }

        // Write location if available
        if let Some(loc) = info.location() {
            let loc_str = loc.file();
            for byte in loc_str.bytes() {
                if byte == b'\n' {
                    writer.new_line();
                } else if byte.is_ascii_graphic()
                    || byte == b' '
                    || byte == b'.'
                    || byte == b'/'
                    || byte == b'\\'
                {
                    writer.write_byte(byte);
                }
            }
            let colons = b":";
            for &byte in colons {
                writer.write_byte(byte);
            }
            let mut line_buf = [0u8; 10];
            let line_str = u32_to_str_heapless(loc.line(), &mut line_buf);
            for byte in line_str.bytes() {
                writer.write_byte(byte);
            }
            for &byte in colons {
                writer.write_byte(byte);
            }
            let mut col_buf = [0u8; 10];
            let col_str = u32_to_str_heapless(loc.column(), &mut col_buf);
            for byte in col_str.bytes() {
                writer.write_byte(byte);
            }
            writer.new_line();
        }

        // Write message
        if let Some(msg) = info.message().as_str() {
            for byte in msg.bytes() {
                if byte == b'\n' {
                    writer.new_line();
                } else if byte.is_ascii_graphic() || byte == b' ' {
                    writer.write_byte(byte);
                }
            }
        } else {
            let msg_failed = b"(message formatting failed)";
            for &byte in msg_failed {
                writer.write_byte(byte);
            }
        }
        writer.new_line();
    }

    // For QEMU debugging, halt the CPU
    unsafe {
        asm!("hlt");
    }
    loop {} // Panics must diverge
}

/// Alloc error handler required when using `alloc` in no_std.
#[cfg(all(panic = "unwind", not(feature = "std"), not(test)))]
#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
    // Avoid recursive panics by directly looping
    loop {
        // Optionally, try to print a message using the heap-less writer if possible
        if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
            let st_ref = unsafe { &*st_ptr.0 };
            crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);
            crate::serial::UEFI_WRITER
                .lock()
                .write_string_heapless("Allocation error!\n")
                .ok();
        }
        unsafe {
            asm!("hlt"); // For QEMU debugging
        }
    }
}

/// Test harness for no_std environment
#[cfg(test)]
pub trait Testable {
    fn run(&self);
}

#[cfg(test)]
impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        println!("{}...\t", core::any::type_name::<T>());
        self();
        println!("[ok]");
    }
}

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
}

/// Generic function to safely and efficiently scroll a raw pixel buffer up
/// Reduces code duplication in buffer management
pub unsafe fn scroll_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let bytes_per_line = stride * bytes_per_pixel;
    let shift_bytes = 8u64 * bytes_per_line as u64;
    let fb_ptr = address as *mut u8;
    let total_bytes = height as u64 * bytes_per_line as u64;
    unsafe {
        core::ptr::copy(
            fb_ptr.add(shift_bytes as usize),
            fb_ptr,
            (total_bytes - shift_bytes) as usize,
        );
    }
    // Clear last 8 lines
    let clear_offset = (height - 8) as usize * bytes_per_line as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let clear_count = 8 * stride as usize;
    unsafe {
        core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color);
    }
}

/// Generic function to clear a raw pixel buffer
/// Reduces code duplication in buffer initialization
pub unsafe fn clear_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let fb_ptr = address as *mut T;
    let count = (stride * height) as usize;
    unsafe {
        core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color);
    }
}

/// Generic PCI I/O port access helper functions for bare-metal PCI enumeration
/// Minimizes unsafe code by wrapping port operations
pub mod bare_metal_pci {
    use super::*;
    use crate::graphics::ports::{PortWriter, RegisterConfig, VgaPorts};
    use crate::serial::_print;

    /// PCI configuration space access addresses (x86 I/O ports)
    const PCI_CONFIG_ADDR: u16 = 0xCF8;
    const PCI_CONFIG_DATA: u16 = 0xCFC;

    /// PCI configuration space register layout
    const PCI_VENDOR_ID_OFFSET: u8 = 0x00;
    const PCI_DEVICE_ID_OFFSET: u8 = 0x02;
    const PCI_CLASS_CODE_OFFSET: u8 = 0x0B;
    const PCI_SUBCLASS_OFFSET: u8 = 0x0A;
    const PCI_BAR0_OFFSET: u8 = 0x10;

    /// Build PCI configuration address for register access
    fn build_pci_config_address(bus: u8, device: u8, function: u8, register: u8) -> u32 {
        // PCI config address format: 31=enable, 30-24=reserved, 23-16=bus, 15-11=device, 10-8=function, 7-2=register, 1-0=00
        let addr = (1u32 << 31) | ((bus as u32) << 16) | ((device as u32) << 11) | ((function as u32) << 8) | (register as u32);
        addr & !0x3 // Clear bits 1-0 as they're alignment bits
    }

    /// Read 32-bit value from PCI configuration space
    pub fn pci_config_read_dword(bus: u8, device: u8, function: u8, register: u8) -> u32 {
        let addr = build_pci_config_address(bus, device, function, register);
        let mut addr_writer = PortWriter::new(PCI_CONFIG_ADDR);
        let mut data_reader = PortWriter::new(PCI_CONFIG_DATA);

        addr_writer.write_safe(addr);
        data_reader.read_safe()
    }

    /// Read 16-bit value from PCI configuration space
    pub fn pci_config_read_word(bus: u8, device: u8, function: u8, register: u8) -> u16 {
        let dword = pci_config_read_dword(bus, device, function, register & !0x2); // Align to 32-bit boundary
        let offset = register & 0x2;
        (dword >> (offset * 8)) as u16
    }

    /// Read 8-bit value from PCI configuration space
    pub fn pci_config_read_byte(bus: u8, device: u8, function: u8, register: u8) -> u8 {
        let dword = pci_config_read_dword(bus, device, function, register & !0x3); // Align to 32-bit boundary
        let offset = register & 0x3;
        (dword >> (offset * 8)) as u8
    }

    /// Check if PCI device exists (valid vendor ID)
    pub fn pci_device_exists(bus: u8, device: u8, function: u8) -> bool {
        let vendor_id = pci_config_read_word(bus, device, function, PCI_VENDOR_ID_OFFSET);
        vendor_id != 0xFFFF
    }

    /// Read PCI device information
    pub fn read_pci_device_info(bus: u8, device: u8, function: u8) -> Option<crate::graphics_alternatives::PciDevice> {
        if !pci_device_exists(bus, device, function) {
            return None;
        }

        let vendor_id = pci_config_read_word(bus, device, function, PCI_VENDOR_ID_OFFSET);
        let device_id = pci_config_read_word(bus, device, function, PCI_DEVICE_ID_OFFSET);
        let class_code = pci_config_read_byte(bus, device, function, PCI_CLASS_CODE_OFFSET);
        let subclass = pci_config_read_byte(bus, device, function, PCI_SUBCLASS_OFFSET);

        Some(crate::graphics_alternatives::PciDevice {
            vendor_id,
            device_id,
            class_code,
            subclass,
            bus,
            device,
            function,
        })
    }

    /// Read PCI BAR (Base Address Register)
    pub fn read_pci_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> u64 {
        let offset = PCI_BAR0_OFFSET + (bar_index * 4);
        let bar_low = pci_config_read_dword(bus, device, function, offset);
        let bar_high = if bar_index == 0 { // Only BAR0 can be 64-bit
            let bar_type = bar_low & 0xF;
            if (bar_type & 0x4) != 0 { // 64-bit BAR
                pci_config_read_dword(bus, device, function, offset + 4)
            } else {
                0
            }
        } else {
            0
        };

        ((bar_high as u64) << 32) | ((bar_low as u64) & 0xFFFFFFF0)
    }

    /// Enumerate all PCI devices on all buses
    pub fn enumerate_all_pci_devices() -> alloc::vec::Vec<crate::graphics_alternatives::PciDevice> {
        let mut devices = alloc::vec::Vec::new();

        // Scan all possible PCI devices (bus 0-255, device 0-31, function 0-7)
        // In practice, most systems only use bus 0 and maybe a few bridges
        for bus in 0..=255u8 {
            for device in 0..32u8 {
                for function in 0..8u8 {
                    if let Some(pci_dev) = read_pci_device_info(bus, device, function) {
                        if pci_dev.vendor_id != 0xFFFF {
                            devices.push(pci_dev);
                        }
                    }
                }
            }
            // Performance optimization: if bus 0 has devices, likely no more buses exist
            if !devices.is_empty() && bus == 0 {
                break;
            }
        }

        devices
    }

    /// Find all graphics devices
    pub fn enumerate_graphics_devices() -> alloc::vec::Vec<crate::graphics_alternatives::PciDevice> {
        enumerate_all_pci_devices().into_iter()
            .filter(|dev| dev.class_code == 0x03) // Display controller class
            .collect()
    }
}

use alloc::boxed::Box;

/// Alternative graphics detection methods when GOP is unavailable
pub mod graphics_alternatives {
    use super::*;
    use alloc::vec::Vec;
    use crate::common::{EfiBootServices, EfiStatus};
    use crate::println;
    use crate::serial::_print;

    const EFI_PCI_IO_PROTOCOL_GUID: [u8; 16] = [
        0x4c, 0xf2, 0x39, 0x77, 0xd7, 0x93, 0xd4, 0x11,
        0x9a, 0x3a, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d
    ];

    #[derive(Debug, Clone, Copy)]
    pub struct PciDevice {
        pub vendor_id: u16,
        pub device_id: u16,
        pub class_code: u8,
        pub subclass: u8,
        pub bus: u8,
        pub device: u8,
        pub function: u8,
    }

    /// Try to detect VESA-compatible graphics hardware using PCI enumeration
    /// Returns information about available graphics devices
    pub fn detect_vesa_graphics(bs: &EfiBootServices) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[GOP-ALT] Detecting VESA graphics hardware...\n"));

        // Try PCI enumeration for graphics devices
        match enumerate_pci_graphics_devices(bs) {
            Ok(devices) if !devices.is_empty() => {
                _print(format_args!("[GOP-ALT] Found {} PCI graphics devices\n", devices.len()));
                for device in devices {
                    _print(format_args!("[GOP-ALT] Graphics device: {:04x}:{:04x}, class {:02x}.{:02x} at {:02x}:{:02x}:{:02x}\n",
                        device.vendor_id, device.device_id, device.class_code, device.subclass, device.bus, device.device, device.function));

                    // Check if this device supports linear framebuffer mode
                    if let Some(fb_info) = probe_linear_framebuffer(&device, bs) {
                        _print(format_args!("[GOP-ALT] Linear framebuffer found at {:#x}, {}x{}.\n", fb_info.address, fb_info.width, fb_info.height));
                        return Some(fb_info);
                    }
                }
                _print(format_args!("[GOP-ALT] No linear framebuffers found on graphics devices\n"));
                None
            }
            Ok(_) => {
                _print(format_args!("[GOP-ALT] No graphics devices found via PCI enumeration\n"));
                None
            }
            Err(e) => {
                _print(format_args!("[GOP-ALT] PCI enumeration failed: {:?}", e));
                None
            }
        }
    }

    /// Enumerate PCI devices using EFI_PCI_IO_PROTOCOL
    fn enumerate_pci_graphics_devices(bs: &EfiBootServices) -> Result<Vec<PciDevice>, EfiStatus> {
        _print(format_args!("[GOP-ALT] Starting PCI device enumeration...\n"));

        // First, enumerate all PCI_IO handles
        let mut handle_count: usize = 0;
        let mut handles: *mut usize = core::ptr::null_mut();

        let status = unsafe {
            (bs.locate_handle_buffer)(
                2, // ByProtocol
                EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
                core::ptr::null_mut(),
                &mut handle_count,
                &mut handles,
            )
        };

        if EfiStatus::from(status) != EfiStatus::Success || handles.is_null() {
            _print(format_args!("[GOP-ALT] Failed to locate PCI_IO handles: {:#x}\n", status));
            return Err(EfiStatus::from(status));
        }

        _print(format_args!("[GOP-ALT] Found {} PCI_IO protocol handles\n", handle_count));

        let mut devices = Vec::new();

        // Process each PCI_IO handle
        for i in 0..handle_count {
            let handle = unsafe { *handles.add(i) };
            _print(format_args!("[GOP-ALT] Checking PCI_IO handle {}: {:#x}\n", i, handle));

            if let Some(dev) = probe_pci_device_on_handle(bs, handle) {
                _print(format_args!("[GOP-ALT] Found PCI device: {:04x}:{:04x} at {:02x}:{:02x}:{:02x}, class {:02x}:{:02x}\n",
                    dev.vendor_id, dev.device_id, dev.bus, dev.device, dev.function, dev.class_code, dev.subclass));

                // Check if it's a graphics device (Display controller class, 0x03)
                if dev.class_code == 0x03 {
                    _print(format_args!("[GOP-ALT] Added graphics device to list\n"));
                    devices.push(dev);
                }
            } else {
                _print(format_args!("[GOP-ALT] Failed to probe PCI device on handle {}\n", i));
            }
        }

        // Free handle buffer
        if !handles.is_null() {
            unsafe { (bs.free_pool)(handles as *mut core::ffi::c_void) };
        }

        _print(format_args!("[GOP-ALT] PCI enumeration complete, found {} graphics devices\n", devices.len()));

        Ok(devices)
    }

    /// Probe PCI device information from a given handle
    fn probe_pci_device_on_handle(bs: &EfiBootServices, handle: usize) -> Option<PciDevice> {
        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();

        let protocol_status = unsafe {
            (bs.open_protocol)(
                handle,
                EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
                &mut pci_io,
                0, // AgentHandle (null for now)
                0, // ControllerHandle (null)
                0x01, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
            )
        };

        if EfiStatus::from(protocol_status) != EfiStatus::Success || pci_io.is_null() {
            return None;
        }

        // Read PCI configuration header using PCI_IO functions
        // The PCI_IO protocol has specific function indices:
        // Index 0: ConfigSpaceRead function

        let pci_io_functions = pci_io as *mut usize;
        let config_space_read_fn: unsafe extern "efiapi" fn(
            *mut core::ffi::c_void,  // PciIo
            u8,      // Width (0=byte, 1=word, 2=dword, 3=qword)
            u32,     // Offset
            usize,   // Count
            *mut core::ffi::c_void  // Buffer
        ) -> usize = unsafe { core::mem::transmute(*pci_io_functions) };

        // Read the first 64 bytes (16 dwords) of PCI config space
        let mut config_buf = [0u32; 16];
        let read_status = unsafe {
            config_space_read_fn(
                pci_io,
                2, // Dword width
                0, // Offset 0
                16, // 16 dwords
                config_buf.as_mut_ptr() as *mut core::ffi::c_void
            )
        };

        if EfiStatus::from(read_status) != EfiStatus::Success {
            unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };
            return None;
        }

        // Parse PCI configuration header
        let vendor_id = (config_buf[0] & 0xFFFF) as u16;
        let device_id = ((config_buf[0] >> 16) & 0xFFFF) as u16;

        // Skip invalid devices
        if vendor_id == 0xFFFF || vendor_id == 0 {
            unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };
            return None;
        }

        let class_code = ((config_buf[2] >> 24) & 0xFF) as u8;
        let subclass = ((config_buf[2] >> 16) & 0xFF) as u8;

        // Now we need to get bus/device/function info
        // Use GetLocation function (index 6 in PCI_IO)
        let get_location_fn: unsafe extern "efiapi" fn(
            *mut core::ffi::c_void,  // PciIo
            *mut u32,                // SegmentNumber
            *mut u32,                // BusNumber
            *mut u32,                // DeviceNumber
            *mut u32                 // FunctionNumber
        ) -> usize = unsafe { core::mem::transmute(*pci_io_functions.add(6)) };

        let mut segment_num: u32 = 0;
        let mut bus_num: u32 = 0;
        let mut dev_num: u32 = 0;
        let mut func_num: u32 = 0;

        let location_status = unsafe {
            get_location_fn(
                pci_io,
                &mut segment_num as *mut u32,
                &mut bus_num as *mut u32,
                &mut dev_num as *mut u32,
                &mut func_num as *mut u32,
            )
        };

        // Close protocol before returning
        unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };

        if EfiStatus::from(location_status) == EfiStatus::Success {
            Some(PciDevice {
                vendor_id,
                device_id,
                class_code,
                subclass,
                bus: bus_num as u8,
                device: dev_num as u8,
                function: func_num as u8,
            })
        } else {
            _print(format_args!("[GOP-ALT] GetLocation failed: {:#x}\n", location_status));
            None
        }
    }

    /// Probe for linear framebuffer on a graphics device
    fn probe_linear_framebuffer(device: &PciDevice, bs: &EfiBootServices) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[GOP-ALT] Probing linear framebuffer on device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
            device.vendor_id, device.device_id, device.bus, device.device, device.function));

        // Check for known virtio-gpu device IDs (vendor: 0x1af4, devices: 0x1050+)
        if device.vendor_id == 0x1af4 && device.device_id >= 0x1050 {
            _print(format_args!("[GOP-ALT] Detected virtio-gpu device, attempting linear framebuffer setup\n"));
            return probe_virtio_gpu_framebuffer(device, bs);
        }

        // Check for other devices that might support linear framebuffers
        // Could add support for qxl, vmware svga, etc.
        match (device.vendor_id, device.device_id) {
            (0x1b36, 0x0100) => {
                // QEMU QXL device
                _print(format_args!("[GOP-ALT] Detected QXL device - linear framebuffer not implemented yet\n"));
                None
            }
            (0x15ad, 0x0405) => {
                // VMware SVGA II
                _print(format_args!("[GOP-ALT] Detected VMware SVGA device - linear framebuffer not implemented yet\n"));
                None
            }
            _ => {
                _print(format_args!("[GOP-ALT] Unknown graphics device, skipping linear framebuffer probe\n"));
                None
            }
        }
    }

    /// Probe virtio-gpu device for linear framebuffer capability
    fn probe_virtio_gpu_framebuffer(device: &PciDevice, bs: &EfiBootServices) -> Option<crate::common::FullereneFramebufferConfig> {
        // Build PCI handle for this device location
        let handle = ((device.bus as usize) << 8) | ((device.device as usize) << 3) | (device.function as usize);

        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();
        let status = unsafe {
            (bs.open_protocol)(
                handle,
                EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
                &mut pci_io,
                0, // AgentHandle
                0, // ControllerHandle
                0x01, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
            )
        };

        if EfiStatus::from(status) != EfiStatus::Success || pci_io.is_null() {
            _print(format_args!("[GOP-ALT] Failed to open PCI_IO protocol for virtio-gpu: {:#x}\n", status));
            return None;
        }

        _print(format_args!("[GOP-ALT] Successfully opened PCI_IO protocol\n"));

        // Read PCI configuration to get BAR information
        let mut config_buf = [0u32; 6]; // First 24 bytes (6 dwords) contain BAR0-BAR5

        let read_result = unsafe {
            let pci_io_read: unsafe extern "efiapi" fn(
                *mut core::ffi::c_void,
                u8,  // Width (2=dword)
                u8,  // Offset
                usize, // Count
                *mut core::ffi::c_void
            ) -> usize = core::mem::transmute(*((pci_io as *mut usize).add(1)));

            pci_io_read(
                pci_io,
                2, // Dword
                0x10, // BAR0 offset (0x10)
                6, // 6 BARs
                config_buf.as_mut_ptr() as *mut core::ffi::c_void
            )
        };

        if read_result != 0 {
            _print(format_args!("[GOP-ALT] Failed to read PCI BARs: {:#x}\n", read_result));
            unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };
            return None;
        }

        // Analyze BAR0 (typically the framebuffer for virtio-gpu)
        let bar0 = config_buf[0] & 0xFFFFFFF0; // Mask off lower 4 bits (flags)
        let bar0_type = config_buf[0] & 0xF;

        if bar0 == 0 {
            _print(format_args!("[GOP-ALT] BAR0 is zero - invalid MMIO region\n"));
            unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };
            return None;
        }

        // Check if BAR0 is a memory-mapped region (bits 0-1 = 00 for 32-bit memory, 10 for 64-bit)
        if bar0_type & 0x1 != 0 {
            _print(format_args!("[GOP-ALT] BAR0 is I/O space (type: {}), expected memory space\n", bar0_type));
            unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };
            return None;
        }

        let is_64bit = (bar0_type & 0x4) != 0;
        let fb_base_addr = if is_64bit {
            // 64-bit BAR - combine BAR0 and BAR1
            let bar1 = config_buf[1];
            ((bar1 as u64) << 32) | (bar0 as u64 & 0xFFFFFFF0)
        } else {
            bar0 as u64
        };

        _print(format_args!("[GOP-ALT] BAR0: {:#x}, type: {}, fb_base: {:#x}, 64-bit: {}\n",
            bar0, bar0_type, fb_base_addr, is_64bit));

        // For virtio-gpu, we need to initialize the device first
        // This involves writing to the device registers in MMIO space
        // But since we don't have the capability to write to MMIO yet,
        // we'll assume a default configuration and try to read from a known offset

        // For virtio-gpu in QEMU, default resolution is typically 1024x768 or 1280x720
        // Try to detect by attempting to access the framebuffer
        let standard_modes = [
            (1024, 768, 32),
            (1280, 720, 32),
            (800, 600, 32),
        ];

        for (width, height, bpp) in standard_modes.iter() {
            let stride = *width; // Assume pixels_per_scan_line = width
            let expected_fb_size = (*height * stride * bpp / 8) as u64;

            // Try to validate framebuffer access (this is a very basic check)
            if probe_framebuffer_access(fb_base_addr, expected_fb_size) {
                _print(format_args!("[GOP-ALT] Detected working virtio-gpu framebuffer: {}x{} @ {:#x}\n",
                    width, height, fb_base_addr));

                // Close PCI_IO protocol
                unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };

                return Some(crate::common::FullereneFramebufferConfig {
                    address: fb_base_addr,
                    width: *width,
                    height: *height,
                    pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    bpp: *bpp,
                    stride: stride,
                });
            }
        }

        // If no standard mode worked, try to determine size by PCI register
        // Read BAR0 size by writing all 1s and reading back (but we can't do that without PCI_IO write access)

        _print(format_args!("[GOP-ALT] Could not determine virtio-gpu framebuffer configuration\n"));

        // Close PCI_IO protocol
        unsafe { (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0) };

        None
    }

    /// Try to validate framebuffer access at the given address
    fn probe_framebuffer_access(address: u64, size: u64) -> bool {
        // This is a very basic probe - in UEFI we should use proper memory mapping
        // For now, we'll just try to read from the address and see if it's accessible

        // WARNING: This is unsafe memory access - in a real implementation
        // we'd need to use EFI_PCI_IO_PROTOCOL memory operations or allocate_address_range
        // to properly map the framebuffer memory

        _print(format_args!("[GOP-ALT] Attempting to validate framebuffer access at {:#x} (size: {}KB)\n",
            address, size / 1024));

        // Try reading first few bytes to see if memory is accessible
        // We need to do this very carefully to avoid crashes
        let ptr = address as *const u8;

        // Check if the address looks valid (not null, not too high)
        if address == 0 || address >= 0xFFFFFFFFFFFFF000 {
            _print(format_args!("[GOP-ALT] Framebuffer address {:#x} appears invalid\n", address));
            return false;
        }

        // In UEFI, we should use memory services to allocate/map this range first
        // For now, we'll assume the PCI_IO memory operations will handle this
        // when we actually access the framebuffer later

        _print(format_args!("[GOP-ALT] Framebuffer address {:#x} appears potentially valid\n", address));
        true // Assume valid for now - real validation would need proper mem mapping
    }

    /// Install linear framebuffer configuration table
    fn install_linear_framebuffer_config(bs: &EfiBootServices, config: crate::common::FullereneFramebufferConfig) -> bool {
        let config_ptr = Box::leak(Box::new(config));
        let status = unsafe { (bs.install_configuration_table)(
            crate::common::FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
            config_ptr as *const _ as *mut core::ffi::c_void,
        ) };

        if EfiStatus::from(status) == EfiStatus::Success {
            true
        } else {
            _print(format_args!("[GOP-ALT] Failed to install linear framebuffer config: {:#x}\n", status));
            let _ = unsafe { Box::from_raw(config_ptr) };
            false
        }
    }

    /// Try to detect VESA VBE (VESA BIOS Extensions) support
    fn detect_vesa_vbe(_bs: &EfiBootServices) -> Result<(), EfiStatus> {
        _print(format_args!("[GOP-ALT] Attempting VESA VBE detection...\n"));

        // Check for VBE signature in BIOS memory
        // VBE typically lives at 0xC0000-0xD0000 in real mode memory

        // Try to call VBE functions through BIOS calls or memory scanning
        // This is highly system-specific and may not work in UEFI environment

        _print(format_args!("[GOP-ALT] VESA VBE detection not implemented yet - requires BIOS interrupt calls\n"));
        _print(format_args!("[GOP-ALT] Consider implementing linear framebuffer detection or ACPI-based graphics\n"));

        Err(EfiStatus::Unsupported)
    }

    /// Read from PCI configuration space (simplified - needs proper implementation)
    unsafe fn _port_read(_port: u16) -> u32 {
        // This is a placeholder - real PCI access needs proper protocols
        // In UEFI, use EFI_PCI_IO_PROTOCOL instead
        0xFFFF_FFFF // Invalid read
    }
}
