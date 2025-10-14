#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod apic;
pub mod bare_metal_graphics_detection;
pub mod bare_metal_pci;
pub mod common;
pub mod graphics;
pub mod graphics_alternatives;
pub mod page_table;
pub mod serial;
pub mod uefi_helpers;
pub use apic::{IoApic, IoApicRedirectionEntry, init_io_apic};
pub use graphics::ports::{MsrHelper, PortOperations, PortWriter, RegisterConfig};
pub use graphics::{
    Color, ColorCode, ScreenChar, TextBufferOperations, VgaPortOps, VgaPorts, init_vga_graphics,
};
pub use serial::SERIAL_PORT_WRITER as SERIAL1;
pub use serial::{Com1Ports, SERIAL_PORT_WRITER, SerialPort, SerialPortOps};
pub use uefi_helpers::handle_panic;

/// Generic framebuffer buffer clear operation
pub unsafe fn clear_buffer_pixels<T: Copy>(
    address: u64,
    stride: u32,
    height: u32,
    bg_color: T,
) {
    let fb_ptr = address as *mut T;
    let count = (stride * height) as usize;
    core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color);
}

/// Generic framebuffer buffer scroll up operation
pub unsafe fn scroll_buffer_pixels<T: Copy>(
    address: u64,
    stride: u32,
    height: u32,
    bg_color: T,
) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let bytes_per_line = stride * bytes_per_pixel;
    let shift_bytes = 8u64 * bytes_per_line as u64;
    let fb_ptr = address as *mut u8;
    let total_bytes = height as u64 * bytes_per_line as u64;
    core::ptr::copy(
        fb_ptr.add(shift_bytes as usize),
        fb_ptr,
        (total_bytes - shift_bytes) as usize,
    );
    // Clear last 8 lines
    let clear_offset = ((height - 8) as u32 * bytes_per_line) as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let clear_count = 8 * stride as usize;
    core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color);
}

use alloc::boxed::Box;
use core::arch::asm;
use core::ffi::c_void;
use core::ptr;
use spin::Mutex;

use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
    FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID,
};
use crate::common::{
    EfiConfigurationTable, EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable,
    FullereneFramebufferConfig,
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

        let status = unsafe {
            (bs.locate_protocol)(
                self.guid.as_ptr(),
                ptr::null_mut(),
                &mut protocol,
            )
        };

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
struct FramebufferInstaller<'a> {
    system_table: &'a EfiSystemTable,
}

impl<'a> FramebufferInstaller<'a> {
    fn new(system_table: &'a EfiSystemTable) -> Self {
        Self { system_table }
    }

    fn install(&self, config: FullereneFramebufferConfig) -> Result<(), EfiStatus> {
        let config_ptr = Box::leak(Box::new(config));
        let bs = unsafe { &*self.system_table.boot_services };

        let status = unsafe {
            (bs.install_configuration_table)(
                FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
                config_ptr as *const _ as *mut c_void,
            )
        };

        let efi_status = EfiStatus::from(status);
        if efi_status != EfiStatus::Success {
            let _ = unsafe { Box::from_raw(config_ptr) };
            Err(efi_status)
        } else {
            Ok(())
        }
    }

    fn install_and_clear(&self, mut config: FullereneFramebufferConfig) -> Result<FullereneFramebufferConfig, EfiStatus> {
        self.install(config)?;

        // Clear screen for clean state
        unsafe {
            ptr::write_bytes(
                config.address as *mut u8,
                0x00,
                (config.height as u64 * config.stride as u64 * (config.bpp as u64 / 8)) as usize,
            );
        }

        Ok(config)
    }
}

/// Generic framebuffer buffer operations
pub trait FramebufferOps<T> {
    unsafe fn scroll_up(&self, address: u64, stride: u32, height: u32, bg_color: T);
    unsafe fn clear(&self, address: u64, stride: u32, height: u32, bg_color: T);
}

impl<T: Copy> FramebufferOps<T> for () {
    unsafe fn scroll_up(&self, address: u64, stride: u32, height: u32, bg_color: T) {
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

    unsafe fn clear(&self, address: u64, stride: u32, height: u32, bg_color: T) {
        let fb_ptr = address as *mut T;
        let count = (stride * height) as usize;
        unsafe {
            core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color);
        }
    }
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
            serial::_print(format_args!(" }}\n"));
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

        let status = unsafe {
            (bs.locate_handle_buffer)(
                2, // ByProtocol
                guid.as_ptr(),
                ptr::null_mut(),
                &mut handle_count,
                &mut handles,
            )
        };

        if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 {
            serial::_print(format_args!(
                "PROTOCOL: {} - Available on {} handles\n",
                name, handle_count
            ));
            if !handles.is_null() {
                unsafe { (bs.free_pool)(handles as *mut c_void) };
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

/// Alternative GOP detection for QEMU environments
#[derive(Clone, Copy)]
struct QemuConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

fn init_gop_framebuffer_alternative(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    const MAX_FRAMEBUFFER_SIZE: u64 = 0x10000000; // 256MB limit - named constant

    serial::_print(format_args!("GOP: Trying alternative detection methods for QEMU...\n"));

    // Try to detect QEMU-specific framebuffer configurations
    // QEMU often provides a linear framebuffer even when GOP is not properly detected

    // Try standard QEMU framebuffer addresses and configurations
    const QEMU_CONFIGS: [QemuConfig; 5] = [
        // Standard QEMU std-vga framebuffer
        QemuConfig { address: 0xE0000000, width: 1024, height: 768, bpp: 32 }, // Common QEMU std-vga mode
        QemuConfig { address: 0xF0000000, width: 1024, height: 768, bpp: 32 }, // Alternative QEMU framebuffer
        QemuConfig { address: 0xFD000000, width: 1024, height: 768, bpp: 32 }, // High memory framebuffer
        QemuConfig { address: 0xE0000000, width: 800, height: 600, bpp: 32 },  // 800x600 mode
        QemuConfig { address: 0xF0000000, width: 800, height: 600, bpp: 32 },  // Alternative 800x600
    ];

    for config in QEMU_CONFIGS.iter() {
        let QemuConfig { address, width, height, bpp } = *config;
        serial::_print(format_args!(
            "GOP: Testing QEMU framebuffer at {:#x}, {}x{}, {} BPP\n",
            address, width, height, bpp
        ));

        // Check if framebuffer memory is accessible (basic validation)
        let framebuffer_size = (height as u64) * (width as u64) * (bpp as u64 / 8);

        if address == 0 || framebuffer_size > MAX_FRAMEBUFFER_SIZE {
            continue;
        }

        // Create framebuffer configuration for QEMU
        let config = FullereneFramebufferConfig {
            address,
            width,
            height,
            pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            bpp,
            stride: width, // Assume stride equals width for QEMU
        };

        serial::_print(format_args!(
            "GOP: QEMU framebuffer candidate: {}x{} @ {:#x}\n",
            config.width, config.height, config.address
        ));

        // Try to install and validate the configuration
        let installer = FramebufferInstaller::new(system_table);
        match installer.install_and_clear(config) {
            Ok(final_config) => {
                serial::_print(format_args!(
                    "GOP: Successfully initialized QEMU framebuffer: {}x{} @ {:#x}\n",
                    final_config.width, final_config.height, final_config.address
                ));
                return Some(final_config);
            }
            Err(status) => {
                serial::_print(format_args!(
                    "GOP: Failed to install QEMU framebuffer config (status: {:#x})\n",
                    status as u32
                ));
                continue;
            }
        }
    }

    serial::_print(format_args!("GOP: No working QEMU framebuffer configurations found\n"));
    None
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

    let installer = FramebufferInstaller::new(system_table);
    match installer.install_and_clear(config) {
        Ok(final_config) => {
            serial::_print(format_args!(
                "GOP: Configuration table installed successfully.\n"
            ));
            Some(final_config)
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
    tester.test_availability(
        &EFI_LOADED_IMAGE_PROTOCOL_GUID,
        "EFI_LOADED_IMAGE_PROTOCOL",
    );
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

    if let Some(config) = graphics_alternatives::detect_vesa_graphics(unsafe { &*system_table.boot_services }) {
        serial::_print(format_args!(
            "EFI PCI enumeration succeeded, installing config table\n"
        ));

        let config_ptr = Box::leak(Box::new(config));

        let boot_services = unsafe { &*system_table.boot_services };
        let install_status = unsafe {
            (boot_services.install_configuration_table)(
                FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
                config_ptr as *const _ as *mut c_void,
            )
        };

        if EfiStatus::from(install_status) != EfiStatus::Success {
            let _ = unsafe { Box::from_raw(config_ptr) };
            serial::_print(format_args!(
                "EFI: Failed to install config table (status: {:#x}).\n",
                install_status
            ));
            return None;
        }

        serial::_print(format_args!(
            "EFI: Configuration table installed successfully.\n"
        ));
        serial::_print(format_args!(
            "EFI: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config_ptr.width,
            config_ptr.height,
            config_ptr.address,
            config_ptr.bpp,
            config_ptr.stride
        ));

        unsafe {
            ptr::write_bytes(
                config_ptr.address as *mut u8,
                0x00,
                (config_ptr.height as u64 * config_ptr.stride as u64 * (config_ptr.bpp as u64 / 8))
                    as usize,
            );
        }

        serial::_print(format_args!("EFI: Framebuffer cleared.\n"));
        return Some(*config_ptr);
    }

    serial::_print(format_args!(
        "EFI PCI enumeration failed, trying bare-metal detection\n"
    ));

    if let Some(config) = bare_metal_graphics_detection::detect_bare_metal_graphics() {
        let config_ptr = Box::leak(Box::new(config));

        let install_status = unsafe {
            ((*system_table.boot_services).install_configuration_table)(
                FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID.as_ptr(),
                config_ptr as *const _ as *mut c_void,
            )
        };

        if EfiStatus::from(install_status) != EfiStatus::Success {
            let _ = unsafe { Box::from_raw(config_ptr) };
            serial::_print(format_args!(
                "Bare-metal: Failed to install config table (status: {:#x}).\n",
                install_status
            ));
            return None;
        }

        serial::_print(format_args!(
            "Bare-metal: Configuration table installed successfully.\n"
        ));
        serial::_print(format_args!(
            "Bare-metal: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config_ptr.width,
            config_ptr.height,
            config_ptr.address,
            config_ptr.bpp,
            config_ptr.stride
        ));

        unsafe {
            ptr::write_bytes(
                config_ptr.address as *mut u8,
                0x00,
                (config_ptr.height as u64 * config_ptr.stride as u64 * (config_ptr.bpp as u64 / 8))
                    as usize,
            );
        }

        serial::_print(format_args!("Bare-metal: Framebuffer cleared.\n"));
        return Some(*config_ptr);
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
