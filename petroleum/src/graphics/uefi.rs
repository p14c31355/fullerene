use crate::common::memory::create_framebuffer_config;
use crate::common::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID,
    EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
};
use core::ffi::c_void;
use core::ptr;
use spin::Mutex;

type EfiUniversalGraphicsAdapterProtocolPtr = isize;

macro_rules! log_uefi {
    ($($arg:tt)*) => { crate::serial::_print(format_args!($($arg)*)) };
}

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

pub fn init_uga_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let locator = ProtocolLocator::new(&EFI_UNIVERSAL_GRAPHICS_ADAPTER_PROTOCOL_GUID, system_table);
    let mut uga: *mut EfiUniversalGraphicsAdapterProtocolPtr = ptr::null_mut();
    match locator.locate(&mut uga) {
        Ok(_) => {
            log_uefi!("UGA protocol found, but UGA implementation incomplete.\n");
            None
        }
        Err(status) => {
            log_uefi!(
                "UGA protocol not available (status: {:#x})\n",
                status as u32
            );
            None
        }
    }
}

struct FramebufferInstaller;

impl FramebufferInstaller {
    fn new() -> Self {
        Self
    }

    fn install(&self, config: FullereneFramebufferConfig) -> Result<(), EfiStatus> {
        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
        log_uefi!("FramebufferInstaller::install saved config globally\n");
        Ok(())
    }

    fn clear_framebuffer(&self, config: &FullereneFramebufferConfig) {
        if config.address != 0 {
            unsafe {
                ptr::write_bytes(
                    config.address as *mut u8,
                    0x00,
                    (config.height as u64 * config.stride as u64) as usize,
                );
            }
        }
    }

    fn clear_framebuffer_gray(&self, config: &FullereneFramebufferConfig) {
        if config.address != 0 && config.bpp == 32 {
            const GRAY: u32 = 0x00808080u32;
            let pixel_count = (config.height as usize) * (config.stride as usize / 4);
            unsafe {
                let ptr = config.address as *mut u32;
                for i in 0..pixel_count {
                    ptr.add(i).write_volatile(GRAY);
                }
            }
            log_uefi!(
                "FramebufferInstaller: cleared to gray ({}x{})\n",
                config.width,
                config.height
            );
        }
    }

}

pub fn detect_standard_modes(
    device_type: &str,
    modes: &[(u32, u32, u32, u64)],
) -> Option<FullereneFramebufferConfig> {
    for (width, height, bpp, addr) in modes.iter() {
        let expected_fb_size = (*height as u64) * (*width as u64) * (*bpp as u64 / 8);
        log_uefi!(
            "[BM-GFX] Testing {}x{} mode at {:#x} (size: {}KB)\n",
            width,
            height,
            addr,
            expected_fb_size / 1024
        );
        if *addr >= 0x100000 {
            log_uefi!(
                "[BM-GFX] {} framebuffer mode {}x{} appears valid\n",
                device_type,
                width,
                height
            );
            return Some(create_framebuffer_config(
                *addr,
                *width,
                *height,
                crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                *bpp,
                (*width as u64)
                    .checked_mul(*bpp as u64 / 8)
                    .and_then(|s| u32::try_from(s).ok())
                    .unwrap_or(0),
            ));
        }
    }
    None
}

pub fn test_qemu_framebuffer_access(address: u64) -> bool {
    if address == 0 {
        return false;
    }
    log_uefi!(
        "QEMU framebuffer address {:#x} accepted (direct probe skipped)\n",
        address
    );
    true
}

pub fn find_working_qemu_config(
    configs: &[crate::QemuConfig],
) -> Option<FullereneFramebufferConfig> {
    const MAX_FRAMEBUFFER_SIZE: u64 = 0x10000000;
    for config in configs.iter() {
        let crate::QemuConfig {
            address,
            width,
            height,
            bpp,
        } = *config;
        log_uefi!(
            "Testing QEMU config at {:#x}, {}x{}, {} BPP\n",
            address,
            width,
            height,
            bpp
        );
        let framebuffer_size = (height as u64) * (width as u64) * (bpp as u64 / 8);
        if address == 0 || framebuffer_size > MAX_FRAMEBUFFER_SIZE {
            continue;
        }
        if test_qemu_framebuffer_access(address) {
            log_uefi!("QEMU framebuffer address {:#x} is accessible\n", address);
            let fb_config = create_framebuffer_config(
                address,
                width,
                height,
                crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp,
                (width as u64)
                    .checked_mul(bpp as u64 / 8)
                    .and_then(|s| u32::try_from(s).ok())
                    .unwrap_or(0),
            );
            log_uefi!(
                "QEMU framebuffer candidate: {}x{} @ {:#x}\n",
                fb_config.width,
                fb_config.height,
                fb_config.address
            );
            return Some(fb_config);
        }
    }
    log_uefi!("No working QEMU configurations found\n");
    None
}

pub fn detect_qemu_framebuffer(
    standard_configs: &[crate::QemuConfig],
) -> Option<FullereneFramebufferConfig> {
    log_uefi!("Testing QEMU framebuffer configurations...\n");
    find_working_qemu_config(standard_configs)
}

pub fn init_gop_framebuffer_alternative(
    _system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    log_uefi!("GOP: Trying alternative detection methods for QEMU...\n");
    if let Some(fb_config) = find_working_qemu_config(&crate::QEMU_CONFIGS) {
        log_uefi!("GOP: Attempting to install framebuffer config table...\n");
        let installer = FramebufferInstaller::new();
        match installer.install(fb_config) {
            Ok(_) => {
                log_uefi!("GOP: Config table installed successfully, clearing framebuffer...\n");
                let _ = installer.clear_framebuffer(&fb_config);
                log_uefi!(
                    "GOP: Successfully initialized QEMU framebuffer: {}x{} @ {:#x}\n",
                    fb_config.width,
                    fb_config.height,
                    fb_config.address
                );
                Some(fb_config)
            }
            Err(status) => {
                log_uefi!(
                    "GOP: Failed to install config table (status: {:#x}).\n",
                    status as u32
                );
                None
            }
        }
    } else {
        log_uefi!("GOP: No QEMU framebuffer configurations succeeded\n");
        None
    }
}

pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    let locator = ProtocolLocator::new(&EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, system_table);
    let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();

    log_uefi!("GOP: Attempting to locate Graphics Output Protocol...\n");
    match locator.locate(&mut gop) {
        Err(status) => {
            log_uefi!(
                "GOP: Failed to locate GOP protocol (status: {:#x}). Skipping QEMU fallback, will try PCI enumeration.\n",
                status as u32
            );
            return None;
        }
        Ok(_) => log_uefi!("GOP: Protocol located successfully at {:#p}.\n", gop),
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        log_uefi!("GOP: Mode pointer is null.\n");
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };
    let current_mode = mode_ref.mode;
    let max_mode_u32 = mode_ref.max_mode;
    if max_mode_u32 == 0 {
        log_uefi!("GOP: Max mode is 0, skipping.\n");
        return None;
    }
    let max_mode = max_mode_u32 as usize;

    log_uefi!(
        "GOP: Current mode: {}, Max mode: {}.\n",
        current_mode,
        max_mode
    );
    log_uefi!(
        "GOP: Skipping SetMode() — using current mode {} as-is (InsydeH2O workaround).\n",
        current_mode
    );

    let mode_ref = unsafe { &*gop_ref.mode };
    if mode_ref.info.is_null() {
        log_uefi!("GOP: Mode info pointer is null after setting mode.\n");
        return None;
    }

    let info = unsafe { &*mode_ref.info };
    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    log_uefi!(
        "GOP: Framebuffer addr: {:#x}, size: {}KB\n",
        fb_addr,
        fb_size / 1024
    );
    log_uefi!(
        "GOP: Resolution: {}x{}, stride: {}\n",
        info.horizontal_resolution,
        info.vertical_resolution,
        info.pixels_per_scan_line
    );
    log_uefi!(
        "GOP: pixel_format = {:?} (0=RGB, 1=BGR, 2=BitMask, 3=BltOnly)\n",
        info.pixel_format
    );
    if info.pixel_format == crate::common::EfiGraphicsPixelFormat::PixelBitMask {
        let pi = info.pixel_information;
        log_uefi!(
            "GOP: PixelBitMask: R={:#010x} G={:#010x} B={:#010x} Res={:#010x}\n",
            pi[0],
            pi[1],
            pi[2],
            pi[3]
        );
    }

    if fb_addr == 0 || fb_size == 0 {
        log_uefi!("GOP: Invalid framebuffer.\n");
        return None;
    }

    let bpp = crate::common::get_bpp_from_pixel_format(info.pixel_format);
    let stride_bytes = (info.pixels_per_scan_line as u64)
        .checked_mul((bpp / 8) as u64)
        .and_then(|s| u32::try_from(s).ok())
        .unwrap_or(0);
    let config = create_framebuffer_config(
        fb_addr as u64,
        info.horizontal_resolution,
        info.vertical_resolution,
        info.pixel_format,
        bpp,
        stride_bytes,
    );

    let calculated_fb_size = (stride_bytes as u64) * (info.vertical_resolution as u64);
    log_uefi!(
        "GOP: fb_size from GOP={} bytes, calculated=stride*height={} bytes, match={}\n",
        fb_size,
        calculated_fb_size,
        fb_size as u64 == calculated_fb_size
    );
    log_uefi!(
        "GOP: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
        config.width,
        config.height,
        config.address,
        config.bpp,
        config.stride
    );

    let installer = FramebufferInstaller::new();
    match installer.install(config) {
        Ok(_) => {
            installer.clear_framebuffer_gray(&config);
            log_uefi!("GOP: Configuration table installed successfully.\n");
            Some(config)
        }
        Err(status) => {
            log_uefi!(
                "GOP: Failed to install config table (status: {:#x}).\n",
                status as u32
            );
            None
        }
    }
}

pub fn init_graphics_protocols(
    system_table: &EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    if system_table.boot_services.is_null() {
        log_uefi!("GOP: System table boot services pointer is null.\n");
        return None;
    }

    log_uefi!("GOP: Initializing graphics protocols...\n");
    log_uefi!(
        "GOP: Configuration table count: {}\n",
        system_table.number_of_table_entries
    );

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
        &crate::common::EFI_LOADED_IMAGE_PROTOCOL_GUID,
        "EFI_LOADED_IMAGE_PROTOCOL",
    );
    tester.test_availability(
        &crate::common::EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
        "EFI_SIMPLE_FILE_SYSTEM_PROTOCOL",
    );

    if let Some(config) = init_gop_framebuffer(system_table) {
        return Some(config);
    }

    log_uefi!("GOP not available, trying UGA protocol...\n");
    if let Some(config) = init_uga_framebuffer(system_table) {
        return Some(config);
    }

    log_uefi!("All graphics protocols failed, trying alternative VESA detection...\n");

    if let Some(config) =
        crate::graphics_alternatives::detect_vesa_graphics(unsafe { &*system_table.boot_services })
    {
        log_uefi!("EFI PCI enumeration succeeded, saving config globally\n");
        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
        log_uefi!("EFI: Config saved globally successfully.\n");
        log_uefi!(
            "EFI: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config.width,
            config.height,
            config.address,
            config.bpp,
            config.stride
        );
        if config.address != 0 {
            unsafe {
                ptr::write_bytes(
                    config.address as *mut u8,
                    0x00,
                    (config.height as u64 * config.stride as u64) as usize,
                );
            }
        }
        log_uefi!("EFI: Framebuffer cleared.\n");
        return Some(config);
    }

    log_uefi!("EFI PCI enumeration failed, trying bare-metal detection\n");
    if let Some(config) = crate::bare_metal_graphics_detection::detect_bare_metal_graphics() {
        log_uefi!("Bare-metal: Config detected, saving globally\n");
        crate::FULLERENE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
        log_uefi!("Bare-metal: Config saved globally successfully.\n");
        log_uefi!(
            "Bare-metal: Framebuffer ready: {}x{} @ {:#x}, {} BPP, stride {}\n",
            config.width,
            config.height,
            config.address,
            config.bpp,
            config.stride
        );
        if config.address != 0 {
            unsafe {
                ptr::write_bytes(
                    config.address as *mut u8,
                    0x00,
                    (config.height as u64 * config.stride as u64) as usize,
                );
            }
        }
        log_uefi!("Bare-metal: Framebuffer cleared.\n");
        return Some(config);
    } else {
        log_uefi!("Bare-metal graphics detection also failed\n");
    }

    log_uefi!("All graphics protocols failed, falling back to VGA text mode.\n");
    log_uefi!(
        "NOTE: GOP protocol typically requires UEFI-compatible video hardware (e.g., QEMU with -vga qxl or virtio-gpu).\n"
    );
    None
}

struct ConfigTableLogger<'a> {
    system_table: &'a EfiSystemTable,
}

impl<'a> ConfigTableLogger<'a> {
    fn new(system_table: &'a EfiSystemTable) -> Self {
        Self { system_table }
    }
    fn log_all(&self) {
        log_uefi!(
            "CONFIG: Enumerating configuration tables ({} total):\n",
            self.system_table.number_of_table_entries
        );
        let config_tables = unsafe {
            core::slice::from_raw_parts(
                self.system_table.configuration_table,
                self.system_table.number_of_table_entries,
            )
        };
        for (i, table) in config_tables.iter().enumerate() {
            log_uefi!("CONFIG[{}]: GUID {{ ", i);
            for (j, &byte) in table.vendor_guid.iter().enumerate() {
                log_uefi!("{:02x}", byte);
                if j < table.vendor_guid.len() - 1 {
                    log_uefi!("-");
                }
            }
            log_uefi!(" }}");
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
            2,
            guid.as_ptr(),
            ptr::null_mut(),
            &mut handle_count,
            &mut handles,
        );
        if EfiStatus::from(status) == EfiStatus::Success && handle_count > 0 {
            log_uefi!(
                "PROTOCOL: {} - Available on {} handles\n",
                name,
                handle_count
            );
            if !handles.is_null() {
                (bs.free_pool)(handles as *mut c_void);
            }
        } else {
            log_uefi!("PROTOCOL: {} - NOT FOUND (status: {:#x})\n", name, status);
        }
    }
}
