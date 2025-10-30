#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

// Fallback heap start address constant for when no suitable memory is found
pub const FALLBACK_HEAP_START_ADDR: u64 = 0x100000;

pub mod apic;
pub mod bare_metal_graphics_detection;
pub mod bare_metal_pci;
#[macro_use]
pub mod common;
pub mod debug;
pub mod filesystem;
pub mod graphics;
pub mod graphics_alternatives;
pub mod hardware;
pub mod initializer;
pub mod page_table;
pub mod serial;
pub mod uefi_helpers;
pub use apic::{IoApic, IoApicRedirectionEntry, init_io_apic};
// Macros with #[macro_export] are automatically available at root, no need to re-export
pub use common::logging::{SystemError, SystemResult};
pub use common::memory::*;
pub use common::syscall::*;
pub use common::{check_memory_initialized, set_memory_initialized};
pub use graphics::ports::{MsrHelper, PortOperations, PortWriter, RegisterConfig};
pub use graphics::*;
pub use graphics::{
    Color, ColorCode, HardwarePorts, ScreenChar, TextBufferOperations, VgaPortOps,
    color::{self},
    init_vga_graphics,
};
pub use serial::SERIAL_PORT_WRITER as SERIAL1;
pub use serial::{Com1Ports, SERIAL_PORT_WRITER, SerialPort, SerialPortOps};
// Heap allocation exports
pub use page_table::ALLOCATOR;
pub use page_table::allocate_heap_from_map;
pub use page_table::init_global_heap;
pub use page_table::{bitmap_allocator, BitmapFrameAllocator};
// Removed reinit_page_table export - implemented in higher-level crates
// UEFI helper exports
pub use uefi_helpers::{initialize_graphics_with_config, kernel_fallback_framebuffer_detection};

/// Generic framebuffer buffer clear operation
/// stride is in bytes per line
pub unsafe fn clear_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let fb_ptr = address as *mut T;
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let elements_per_line = (stride / bytes_per_pixel) as usize;
    let count = elements_per_line * height as usize;
    unsafe { core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color) };
}

/// Generic framebuffer buffer scroll up operation
/// stride is in bytes per line
pub unsafe fn scroll_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let shift_bytes = 8u64 * stride as u64;
    let fb_ptr = address as *mut u8;
    let total_bytes = height as u64 * stride as u64;
    unsafe {
        core::ptr::copy(
            fb_ptr.add(shift_bytes as usize),
            fb_ptr,
            (total_bytes - shift_bytes) as usize,
        );
    }
    // Clear last 8 lines
    let clear_offset = ((height - 8) as u32 * stride) as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let elements_per_line = (stride / bytes_per_pixel) as usize;
    let clear_count = 8 * elements_per_line;
    unsafe { core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color) };
}

use core::ffi::c_void;
use core::ptr;
use spin::{Mutex, Once};

use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
};
use crate::common::{
    EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
};

/// Wrapper for Local APIC address pointer to make it Send/Sync
#[derive(Clone, Copy)]
pub struct LocalApicAddress(pub *mut u32);

unsafe impl Send for LocalApicAddress {}
unsafe impl Sync for LocalApicAddress {}

/// Global storage for Local APIC address
pub static LOCAL_APIC_ADDRESS: Mutex<LocalApicAddress> =
    Mutex::new(LocalApicAddress(core::ptr::null_mut()));

/// Global framebuffer config storage for kernel use after exit_boot_services
pub static FULLERENE_FRAMEBUFFER_CONFIG: Once<Mutex<Option<FullereneFramebufferConfig>>> =
    Once::new();

pub const QEMU_CONFIGS: [QemuConfig; 8] = [
    // Cirrus VGA specific addresses (common with -vga cirrus) - start with successfully tested ones
    QemuConfig {
        address: 0x40000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Verified working config from debug output
    QemuConfig {
        address: 0x40000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // Cirrus 800x600 alternative
    // Standard QEMU std-vga framebuffer
    QemuConfig {
        address: 0xE0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Common QEMU std-vga mode
    QemuConfig {
        address: 0xF0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Alternative QEMU framebuffer
    QemuConfig {
        address: 0xFD000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // High memory framebuffer
    QemuConfig {
        address: 0xE0000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // 800x600 mode
    QemuConfig {
        address: 0xF0000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // Alternative 800x600
    QemuConfig {
        address: 0x80000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Alternative Cirrus address
];

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

pub fn halt_loop() -> ! {
    loop {
        // Use pause instruction which is more QEMU-friendly than hlt
        cpu_pause();
    }
}

/// Helper function to pause CPU for brief moment (used for busy waits and yielding)
#[inline(always)]
pub fn cpu_pause() {
    crate::pause!();
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

/// Generic protocol locator for UEFI protocols
struct ProtocolLocator<'a> {
    guid: &'a [u8; 16],
    system_table: &'a EfiSystemTable,
}

impl<'a> ProtocolLocator<'a> {
    fn new(guid: &'a [u8; 16], system_table: &'a EfiSystemTable) -> Self {
        Self { guid, system_table }
    }

    fn locate<T>(&self, protocol_out: &mut *mut T) -> Result<(), EfiStatus> {
        let bs = unsafe { &*self.system_table.boot_services };
        let mut protocol: *mut c_void = ptr::null_mut();

        let status = (bs.locate_protocol)(self.guid.as_ptr(), ptr::null_mut(), &mut protocol);

        let efi_status = EfiStatus::from(status);
        if efi_status != EfiStatus::Success || protocol.is_null() {
            *protocol_out = ptr::null_mut();
            Err(efi_status)
        } else {
            *protocol_out = protocol as *mut T;
            Ok(())
        }
    }
}

/// Framebuffer configuration installer
struct FramebufferInstaller;

impl FramebufferInstaller {
    fn new() -> Self {
        Self
    }

    fn install(&self, config: FullereneFramebufferConfig) -> Result<(), EfiStatus> {
        // Save to global instead of installing config table to avoid hang
        FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
        serial::_print(format_args!(
            "FramebufferInstaller::install saved config globally\n"
        ));
        Ok(())
    }

    fn clear_framebuffer(&self, config: &FullereneFramebufferConfig) {
        unsafe {
            ptr::write_bytes(
                config.address as *mut u8,
                0x00,
                (config.height as u64 * config.stride as u64) as usize,
            );
        }
    }
}

/// Generic helper for detecting standard framebuffer modes
pub fn detect_standard_modes(
    device_type: &str,
    modes: &[(u32, u32, u32, u64)],
) -> Option<crate::common::FullereneFramebufferConfig> {
    for (width, height, bpp, addr) in modes.iter() {
        let expected_fb_size = (*height * *width * bpp / 8) as u64;
        serial::_print(format_args!(
            "[BM-GFX] Testing {}x{} mode at {:#x} (size: {}KB)\n",
            width,
            height,
            addr,
            expected_fb_size / 1024
        ));

        if *addr >= 0x100000 {
            serial::_print(format_args!(
                "[BM-GFX] {} framebuffer mode {}x{} appears valid\n",
                device_type, width, height
            ));
            return Some(crate::common::memory::create_framebuffer_config(
                *addr,
                *width,
                *height,
                crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                *bpp,
                *width * (*bpp / 8),
            ));
        }
    }
    None
}

/// Configuration table GUID logger
struct ConfigTableLogger<'a> {
    system_table: &'a EfiSystemTable,
}

impl<'a> ConfigTableLogger<'a> {
    fn new(system_table: &'a EfiSystemTable) -> Self {
        Self { system_table }
    }

    fn log_all(&self) {
        serial::_print(format_args!(
            "CONFIG: Enumerating configuration tables ({} total):\n",
            self.system_table.number_of_table_entries
        ));

        let config_tables = unsafe {
            core::slice::from_raw_parts(
                self.system_table.configuration_table,
                self.system_table.number_of_table_entries,
            )
        };

        for (i, table) in config_tables.iter().enumerate() {
            let guid_bytes = &table.vendor_guid;
            serial::_print(format_args!("CONFIG[{}]: GUID {{ ", i));
            for (j, &byte) in guid_bytes.iter().enumerate() {
                serial::_print(format_args!("{:02x}", byte));
                if j < guid_bytes.len() - 1 {
                    serial::_print(format_args!("-"));
                }
            }
            serial::_print(format_args!(" }}"));
        }
    }
}

/// Protocol availability tester
struct ProtocolTester<'a> {
    system_table: &'a EfiSystemTable,
}

impl<'a> ProtocolTester<'a> {
    fn new(system_table: &'a EfiSystemTable) -> Self {
        Self { system_table }
    }

    fn test_availability(&self, guid: &[u8; 16], name: &str) {
        let bs = unsafe { &*self.system_table.boot_services };

        let mut handle_count: usize = 0;
        let mut handles: *mut usize = ptr::null_mut();

        let status = (bs.locate_handle_buffer)(
            2, // ByProtocol
            guid.as_ptr(),
            ptr::null_mut(),
            &mut handle_count,
            &mut handles,
        );

        if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 {
            serial::_print(format_args!(
                "PROTOCOL: {} - Available on {} handles\n",
                name, handle_count
            ));
            if !handles.is_null() {
                (bs.free_pool)(handles as *mut c_void);
            }
        } else {
            serial::_print(format_args!(
                "PROTOCOL: {} - NOT FOUND (status: {:#x})\n",
                name, status
            ));
        }
    }
}

/// Helper to try Universal Graphics Adapter (UGA) protocol
pub fn init_uga_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let locator = ProtocolLocator::new(&EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID, system_table);
    let mut uga: *mut EfiUniversalGraphicsAdapterProtocolPtr = ptr::null_mut();

    match locator.locate(&mut uga) {
        Ok(_) => {
            serial::_print(format_args!(
                "UGA protocol found, but UGA implementation incomplete.\n"
            ));
            None
        }
        Err(status) => {
            serial::_print(format_args!(
                "UGA protocol not available (status: {:#x})\n",
                status as u32
            ));
            None
        }
    }
}

/// Shared struct for QEMU configuration testing
#[derive(Clone, Copy)]
pub struct QemuConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

/// Test a QEMU framebuffer configuration for accessibility
pub fn test_qemu_framebuffer_access(address: u64) -> bool {
    // Check basic constraints
    if address == 0 {
        return false;
    }

    let test_ptr = address as *mut u32;
    if test_ptr.is_null() {
        return false;
    }

    // Try a simple validation write to test if the address is accessible
    // This will help catch invalid framebuffer addresses early
    unsafe {
        // Store original value for restoration if test succeeds
        let original_value = test_ptr.read_volatile();

        // Write a test pattern
        test_ptr.write_volatile(0x12345678);

        // Read back to verify write was successful
        let readback_value = test_ptr.read_volatile();

        if readback_value == 0x12345678 {
            // Restore original value and return success
            test_ptr.write_volatile(original_value);
            true
        } else {
            false
        }
    }
}

/// Generic helper to test QEMU framebuffer configurations
/// Returns the first working configuration from the provided configs
pub fn find_working_qemu_config(configs: &[QemuConfig]) -> Option<FullereneFramebufferConfig> {
    const MAX_FRAMEBUFFER_SIZE: u64 = 0x10000000; // 256MB limit

    for config in configs.iter() {
        let QemuConfig {
            address,
            width,
            height,
            bpp,
        } = *config;

        serial::_print(format_args!(
            "Testing QEMU config at {:#x}, {}x{}, {} BPP\n",
            address, width, height, bpp
        ));

        let framebuffer_size = (height as u64) * (width as u64) * (bpp as u64 / 8);
        if address == 0 || framebuffer_size > MAX_FRAMEBUFFER_SIZE {
            continue;
        }

        if test_qemu_framebuffer_access(address) {
            serial::_print(format_args!(
                "QEMU framebuffer address {:#x} is accessible\n",
                address
            ));

            let fb_config = crate::common::memory::create_framebuffer_config(
                address,
                width,
                height,
                crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp,
                width * (bpp / 8),
            );

            serial::_print(format_args!(
                "QEMU framebuffer candidate: {}x{} @ {:#x}\n",
                fb_config.width, fb_config.height, fb_config.address
            ));

            return Some(fb_config);
        }
    }

    serial::_print(format_args!("No working QEMU configurations found\n"));
    None
}

/// Detect virtualized framebuffer for QEMU/VirtualBox environments
/// This consolidates the duplicated logic between bootloader and kernel
pub fn detect_qemu_framebuffer(
    standard_configs: &[QemuConfig],
) -> Option<FullereneFramebufferConfig> {
    serial::_print(format_args!("Testing QEMU framebuffer configurations...\n"));
    find_working_qemu_config(standard_configs)
}

/// Alternative GOP detection for QEMU environments
pub fn init_gop_framebuffer_alternative(
    _system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    serial::_print(format_args!(
        "GOP: Trying alternative detection methods for QEMU...\n"
    ));

    if let Some(fb_config) = find_working_qemu_config(&QEMU_CONFIGS) {
        serial::_print(format_args!(
            "GOP: Attempting to install framebuffer config table...\n"
        ));

        let installer = FramebufferInstaller::new();
        match installer.install(fb_config) {
            Ok(_) => {
                serial::_print(format_args!(
                    "GOP: Config table installed successfully, clearing framebuffer...\n"
                ));
                let _ = installer.clear_framebuffer(&fb_config);
                serial::_print(format_args!(
                    "GOP: Successfully initialized QEMU framebuffer: {}x{} @ {:#x}\n",
                    fb_config.width, fb_config.height, fb_config.address
                ));
                Some(fb_config)
            }
            Err(status) => {
                serial::_print(format_args!(
                    "GOP: Failed to install framebuffer config table (status: {:#x})\n",
                    status as u32
                ));
                None
            }
        }
    } else {
        serial::_print(format_args!(
            "GOP: No QEMU framebuffer configurations succeeded\n"
        ));
        None
    }
}

/// Helper to initialize GOP and framebuffer
pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let locator = ProtocolLocator::new(&EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, system_table);
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    serial::_print(format_args!(
        "GOP: Attempting to locate Graphics Output Protocol...\n"
    ));

    match locator.locate(&mut gop) {
        Err(status) => {
            serial::_print(format_args!(
                "GOP: Failed to locate GOP protocol (status: {:#x}).\n",
                status as u32
            ));

            // Try alternative GOP detection for QEMU environments
            serial::_print(format_args!("GOP: Trying alternative GOP detection...\n"));
            return init_gop_framebuffer_alternative(system_table);
        }
        Ok(_) => {
            serial::_print(format_args!(
                "GOP: Protocol located successfully at {:#p}.\n",
                gop
            ));
        }
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        serial::_print(format_args!("GOP: Mode pointer is null.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    let current_mode = mode_ref.mode;

    let max_mode_u32 = mode_ref.max_mode;
    if max_mode_u32 == 0 {
        serial::_print(format_args!("GOP: Max mode is 0, skipping.\n"));
        return None;
    }
    let max_mode = max_mode_u32 as usize;

    serial::_print(format_args!(
        "GOP: Current mode: {}, Max mode: {}.\n",
        current_mode, max_mode
    ));

    let mode_setter = GopModeSetter::new(gop);
    let target_modes = [
        current_mode as u32,
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
    ];

    if mode_setter.try_modes(&target_modes, max_mode_u32).is_err() {
        serial::_print(format_args!("GOP: Failed to set any graphics mode.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        serial::_print(format_args!(
            "GOP: Mode info pointer is null after setting mode.\n"
        ));
        return None;
    }

    let info = unsafe { &*mode_ref.info };
    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    serial::_print(format_args!(
        "GOP: Framebuffer addr: {:#x}, size: {}KB\n",
        fb_addr,
        fb_size / 1024
    ));
    serial::_print(format_args!(
        "GOP: Resolution: {}x{}, stride: {}\n",
        info.horizontal_resolution, info.vertical_resolution, info.pixels_per_scan_line
    ));

    if fb_addr == 0 || fb_size == 0 {
        serial::_print(format_args!("GOP: Invalid framebuffer.\n"));
        return None;
    }

    let config = crate::common::memory::create_framebuffer_config(
        fb_addr as u64,
        info.horizontal_resolution,
        info.vertical_resolution,
        info.pixel_format,
        crate::common::get_bpp_from_pixel_format(info.pixel_format),
        info.pixels_per_scan_line,
    );

    serial::_print(format_args!(
        "GOP: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
        config.width, config.height, config.address, config.bpp, config.stride
    ));

    let installer = FramebufferInstaller::new();
    match installer.install(config) {
        Ok(_) => {
            let _ = installer.clear_framebuffer(&config);
            serial::_print(format_args!(
                "GOP: Configuration table installed successfully.\n"
            ));
            Some(config)
        }
        Err(status) => {
            serial::_print(format_args!(
                "GOP: Failed to install config table (status: {:#x}).\n",
                status as u32
            ));
            None
        }
    }
}

/// Main entry point for graphics protocol initialization
pub fn init_graphics_protocols(
    system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    if system_table.boot_services.is_null() {
        serial::_print(format_args!(
            "GOP: System table boot services pointer is null.\n"
        ));
        return None;
    }

    serial::_print(format_args!("GOP: Initializing graphics protocols...\n"));
    serial::_print(format_args!(
        "GOP: Configuration table count: {}\n",
        system_table.number_of_table_entries
    ));

    let logger = ConfigTableLogger::new(system_table);
    logger.log_all();

    let tester = ProtocolTester::new(system_table);
    tester.test_availability(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
        "EFI_GRAPHICS_OUTPUT_PROTOCOL",
    );
    tester.test_availability(
        &EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
        "EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL",
    );
    tester.test_availability(&EFI_LOADED_IMAGE_PROTOCOL_GUID, "EFI_LOADED_IMAGE_PROTOCOL");
    tester.test_availability(
        &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
        "EFI_SIMPLE_FILE_SYSTEM_PROTOCOL",
    );

    if let Some(config) = init_gop_framebuffer(system_table) {
        return Some(config);
    }

    serial::_print(format_args!("GOP not available, trying UGA protocol...\n"));
    if let Some(config) = init_uga_framebuffer(system_table) {
        return Some(config);
    }

    serial::_print(format_args!(
        "All graphics protocols failed, trying alternative VESA detection...\n"
    ));

    if let Some(config) =
        graphics_alternatives::detect_vesa_graphics(unsafe { &*system_table.boot_services })
    {
        serial::_print(format_args!(
            "EFI PCI enumeration succeeded, saving config globally\n"
        ));

        // Save to global instead of installing config table
        FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));

        serial::_print(format_args!("EFI: Config saved globally successfully.\n"));
        serial::_print(format_args!(
            "EFI: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config.width, config.height, config.address, config.bpp, config.stride
        ));

        unsafe {
            ptr::write_bytes(
                config.address as *mut u8,
                0x00,
                (config.height as u64 * config.stride as u64) as usize,
            );
        }

        serial::_print(format_args!("EFI: Framebuffer cleared.\n"));
        return Some(config);
    }

    serial::_print(format_args!(
        "EFI PCI enumeration failed, trying bare-metal detection\n"
    ));

    if let Some(config) = bare_metal_graphics_detection::detect_bare_metal_graphics() {
        serial::_print(format_args!(
            "Bare-metal: Config detected, saving globally\n"
        ));

        // Save to global instead of installing config table
        FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));

        serial::_print(format_args!(
            "Bare-metal: Config saved globally successfully.\n"
        ));
        serial::_print(format_args!(
            "Bare-metal: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config.width, config.height, config.address, config.bpp, config.stride
        ));

        unsafe {
            ptr::write_bytes(
                config.address as *mut u8,
                0x00,
                (config.height as u64 * config.stride as u64) as usize,
            );
        }

        serial::_print(format_args!("Bare-metal: Framebuffer cleared.\n"));
        return Some(config);
    } else {
        serial::_print(format_args!("Bare-metal graphics detection also failed\n"));
    }

    serial::_print(format_args!(
        "All graphics protocols failed, falling back to VGA text mode.\n"
    ));
    serial::_print(format_args!(
        "NOTE: GOP protocol typically requires UEFI-compatible video hardware (e.g., QEMU with -vga qxl or virtio-gpu).\n"
    ));
    None
}

/// Generic GOP mode setter
struct GopModeSetter<'a> {
    gop: *mut EfiGraphicsOutputProtocol,
    _phantom: core::marker::PhantomData<&'a ()>,
}

impl<'a> GopModeSetter<'a> {
    fn new(gop: *mut EfiGraphicsOutputProtocol) -> Self {
        Self {
            gop,
            _phantom: core::marker::PhantomData,
        }
    }

    fn try_modes(&self, target_modes: &[u32], max_mode: u32) -> Result<(), ()> {
        for &mode in target_modes {
            if mode >= max_mode {
                continue;
            }
            serial::_print(format_args!("GOP: Attempting to set mode {}...\n", mode));
            let set_status = unsafe { ((*self.gop).set_mode)(self.gop, mode) };
            if EfiStatus::from(set_status) == EfiStatus::Success {
                serial::_print(format_args!("GOP: Successfully set mode {}.\n", mode));
                return Ok(());
            } else {
                serial::_print(format_args!(
                    "GOP: Failed to set mode {}, status: {:#x}.\n",
                    mode, set_status
                ));
            }
        }
        Err(())
    }
}
