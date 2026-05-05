use core::ffi::c_void;
use core::ptr;
use spin::Mutex;
use crate::common::{
    EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
};
use crate::common::memory::create_framebuffer_config;

/// Protocol locator for UEFI protocols
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
        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
        crate::serial::_print(format_args!(
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
) -> Option<FullereneFramebufferConfig> {
    for (width, height, bpp, addr) in modes.iter() {
        let expected_fb_size = (*height * *width * bpp / 8) as u64;
        crate::serial::_print(format_args!(
            "[BM-GFX] Testing {}x{} mode at {:#x} (size: {}KB)\n",
            width, height, addr, expected_fb_size / 1024
        ));

        if *addr >= 0x100000 {
            crate::serial::_print(format_args!(
                "[BM-GFX] {} framebuffer mode {}x{} appears valid\n",
                device_type, width, height
            ));
            return Some(create_framebuffer_config(
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

/// Test a QEMU framebuffer configuration for accessibility
pub fn test_qemu_framebuffer_access(address: u64) -> bool {
    if address == 0 {
        return false;
    }

    let test_ptr = address as *mut u32;
    if test_ptr.is_null() {
        return false;
    }

    unsafe {
        let original_value = test_ptr.read_volatile();
        test_ptr.write_volatile(0x12345678);
        let readback_value = test_ptr.read_volatile();

        if readback_value == 0x12345678 {
            test_ptr.write_volatile(original_value);
            true
        } else {
            false
        }
    }
}

/// Generic helper to test QEMU framebuffer configurations
pub fn find_working_qemu_config(configs: &[crate::QemuConfig]) -> Option<FullereneFramebufferConfig> {
    const MAX_FRAMEBUFFER_SIZE: u64 = 0x10000000; // 256MB limit

    for config in configs.iter() {
        let crate::QemuConfig {
            address,
            width,
            height,
            bpp,
        } = *config;

        crate::serial::_print(format_args!(
            "Testing QEMU config at {:#x}, {}x{}, {} BPP\n",
            address, width, height, bpp
        ));

        let framebuffer_size = (height as u64) * (width as u64) * (bpp as u64 / 8);
        if address == 0 || framebuffer_size > MAX_FRAMEBUFFER_SIZE {
            continue;
        }

        if test_qemu_framebuffer_access(address) {
            crate::serial::_print(format_args!(
                "QEMU framebuffer address {:#x} is accessible\n",
                address
            ));

            let fb_config = create_framebuffer_config(
                address,
                width,
                height,
                crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp,
                width * (bpp / 8),
            );

            crate::serial::_print(format_args!(
                "QEMU framebuffer candidate: {}x{} @ {:#x}\n",
                fb_config.width, fb_config.height, fb_config.address
            ));

            return Some(fb_config);
        }
    }
    crate::serial::_print(format_args!("No working QEMU configurations found\n"));
    None
}

/// Detect virtualized framebuffer for QEMU/VirtualBox environments
pub fn detect_qemu_framebuffer(
    standard_configs: &[crate::QemuConfig],
) -> Option<FullereneFramebufferConfig> {
    crate::serial::_print(format_args!("Testing QEMU framebuffer configurations...\n"));
    find_working_qemu_config(standard_configs)
}

/// Alternative GOP detection for QEMU environments
pub fn init_gop_framebuffer_alternative(
    _system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    crate::serial::_print(format_args!(
        "GOP: Trying alternative detection methods for QEMU...\n"
    ));

    if let Some(fb_config) = find_working_qemu_config(&crate::QEMU_CONFIGS) {
        crate::serial::_print(format_args!(
            "GOP: Attempting to install framebuffer config table...\n"
        ));

        let installer = FramebufferInstaller::new();
        match installer.install(fb_config) {
            Ok(_) => {
                crate::serial::_print(format_args!(
                    "GOP: Config table installed successfully, clearing framebuffer...\n"
                ));
                let _ = installer.clear_framebuffer(&fb_config);
                crate::serial::_print(format_args!(
                    "GOP: Successfully initialized QEMU framebuffer: {}x{} @ {:#x}\n",
                    fb_config.width, fb_config.height, fb_config.address
                ));
                Some(fb_config)
            }
            Err(status) => {
                crate::serial::_print(format_args!(
                    "GOP: Failed to install config table (status: {:#x}).\n",
                    status as u32
                ));
                None
            }
        }
    } else {
        crate::serial::_print(format_args!(
            "GOP: No QEMU framebuffer configurations succeeded\n"
        ));
        None
    }
}

/// Helper to initialize GOP and framebuffer
pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let locator = ProtocolLocator::new(&EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, system_table);
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    crate::serial::_print(format_args!(
        "GOP: Attempting to locate Graphics Output Protocol...\n"
    ));

    match locator.locate(&mut gop) {
        Err(status) => {
            crate::serial::_print(format_args!(
                "GOP: Failed to locate GOP protocol (status: {:#x}).\n",
                status as u32
            ));

            crate::serial::_print(format_args!("GOP: Trying alternative GOP detection...\n"));
            return init_gop_framebuffer_alternative(system_table);
        }
        Ok(_) => {
            crate::serial::_print(format_args!(
                "GOP: Protocol located successfully at {:#p}.\n",
                gop
            ));
        }
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        crate::serial::_print(format_args!("GOP: Mode pointer is null.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    let current_mode = mode_ref.mode;

    let max_mode_u32 = mode_ref.max_mode;
    if max_mode_u32 == 0 {
        crate::serial::_print(format_args!("GOP: Max mode is 0, skipping.\n"));
        return None;
    }
    let max_mode = max_mode_u32 as usize;

    crate::serial::_print(format_args!(
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
        crate::serial::_print(format_args!("GOP: Failed to set any graphics mode.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        crate::serial::_print(format_args!(
            "GOP: Mode info pointer is null after setting mode.\n"
        ));
        return None;
    }

    let info = unsafe { &*mode_ref.info };
    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    crate::serial::_print(format_args!(
        "GOP: Framebuffer addr: {:#x}, size: {}KB\n",
        fb_addr,
        fb_size / 1024
    ));
    crate::serial::_print(format_args!(
        "GOP: Resolution: {}x{}, stride: {}\n",
        info.horizontal_resolution, info.vertical_resolution, info.pixels_per_scan_line
    ));

    if fb_addr == 0 || fb_size == 0 {
        crate::serial::_print(format_args!("GOP: Invalid framebuffer.\n"));
        return None;
    }

    let config = create_framebuffer_config(
        fb_addr as u64,
        info.horizontal_resolution,
        info.vertical_resolution,
        info.pixel_format,
        crate::common::get_bpp_from_pixel_format(info.pixel_format),
        info.pixels_per_scan_line,
    );

    crate::serial::_print(format_args!(
        "GOP: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
        config.width, config.height, config.address, config.bpp, config.stride
    ));

    let installer = FramebufferInstaller::new();
    match installer.install(config) {
        Ok(_) => {
            let _ = installer.clear_framebuffer(&config);
            crate::serial::_print(format_args!(
                "GOP: Configuration table installed successfully.\n"
            ));
            Some(config)
        }
        Err(status) => {
            crate::serial::_print(format_args!(
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
        crate::serial::_print(format_args!(
            "GOP: System table boot services pointer is null.\n"
        ));
        return None;
    }

    crate::serial::_print(format_args!("GOP: Initializing graphics protocols...\n"));
    crate::serial::_print(format_args!(
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
    tester.test_availability(&crate::common::EFI_LOADED_IMAGE_PROTOCOL_GUID, "EFI_LOADED_IMAGE_PROTOCOL");
    tester.test_availability(
        &crate::common::EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
        "EFI_SIMPLE_FILE_SYSTEM_PROTOCOL",
    );

    if let Some(config) = init_gop_framebuffer(system_table) {
        return Some(config);
    }

    crate::serial::_print(format_args!("GOP not available, trying UGA protocol...\n"));
    if let Some(config) = crate::init_uga_framebuffer(system_table) {
        return Some(config);
    }

    crate::serial::_print(format_args!(
        "All graphics protocols failed, trying alternative VESA detection...\n"
    ));

    if let Some(config) =
        crate::graphics_alternatives::detect_vesa_graphics(unsafe { &*system_table.boot_services })
    {
        crate::serial::_print(format_args!(
            "EFI PCI enumeration succeeded, saving config globally\n"
        ));

        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));

        crate::serial::_print(format_args!("EFI: Config saved globally successfully.\n"));
        crate::serial::_print(format_args!(
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

        crate::serial::_print(format_args!("EFI: Framebuffer cleared.\n"));
        return Some(config);
    }

    crate::serial::_print(format_args!(
        "EFI PCI enumeration failed, trying bare-metal detection\n"
    ));

    if let Some(config) = crate::bare_metal_graphics_detection::detect_bare_metal_graphics() {
        crate::serial::_print(format_args!(
            "Bare-metal: Config detected, saving globally\n"
        ));

        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));

        crate::serial::_print(format_args!(
            "Bare-metal: Config saved globally successfully.\n"
        ));
        crate::serial::_print(format_args!(
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

        crate::serial::_print(format_args!("Bare-metal: Framebuffer cleared.\n"));
        return Some(config);
    } else {
        crate::serial::_print(format_args!("Bare-metal graphics detection also failed\n"));
    }

    crate::serial::_print(format_args!(
        "All graphics protocols failed, falling back to VGA text mode.\n"
    ));
    crate::serial::_print(format_args!(
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
            crate::serial::_print(format_args!("GOP: Attempting to set mode {}...\n", mode));
            let set_status = unsafe { ((*self.gop).set_mode)(self.gop, mode) };
            if EfiStatus::from(set_status) == EfiStatus::Success {
                crate::serial::_print(format_args!("GOP: Successfully set mode {}.\n", mode));
                return Ok(());
            } else {
                crate::serial::_print(format_args!(
                    "GOP: Failed to set mode {}, status: {:#x}.\n",
                    mode, set_status
                ));
            }
        }
        Err(())
    }
}

struct ConfigTableLogger<'a> {
    system_table: &'a EfiSystemTable,
}

impl<'a> ConfigTableLogger<'a> {
    fn new(system_table: &'a EfiSystemTable) -> Self {
        Self { system_table }
    }

    fn log_all(&self) {
        crate::serial::_print(format_args!(
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
            crate::serial::_print(format_args!("CONFIG[{}]: GUID {{ ", i));
            for (j, &byte) in guid_bytes.iter().enumerate() {
                crate::serial::_print(format_args!("{:02x}", byte));
                if j < guid_bytes.len() - 1 {
                    crate::serial::_print(format_args!("-"));
                }
            }
            crate::serial::_print(format_args!(" }}"));
        }
    }
}

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
            crate::serial::_print(format_args!(
                "PROTOCOL: {} - Available on {} handles\n",
                name, handle_count
            ));
            if !handles.is_null() {
                (bs.free_pool)(handles as *mut c_void);
            }
        } else {
            crate::serial::_print(format_args!(
                "PROTOCOL: {} - NOT FOUND (status: {:#x})\n",
                name, status
            ));
        }
    }
}