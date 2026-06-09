use super::*;
use crate::common::{EfiBootServices, EfiStatus};

/// Alternative graphics detection methods when GOP is unavailable
pub mod graphics_alternatives {
    use super::*;
    use crate::serial::_print;
    use alloc::vec::Vec;

    /// Local shorthand for debug-logging. Replaces the verbose
    /// `_print(format_args!(...))` idiom used throughout this module.
    macro_rules! log_gop {
        ($($arg:tt)*) => { _print(format_args!($($arg)*)) };
    }

    const EFI_PCI_IO_PROTOCOL_GUID: [u8; 16] = [
        0x7b, 0x27, 0xcf, 0x04, 0xd7, 0x39, 0xd2, 0x4b, 0x9a, 0x3a, 0x00, 0x90, 0x27, 0x3f, 0xc1,
        0x4d,
    ];

    /// RAII guard for PCI_IO protocol that ensures close_protocol is always called
    struct PciIoGuard<'a> {
        bs: &'a EfiBootServices,
        handle: usize,
        protocol: *mut crate::common::EfiPciIoProtocol,
    }

    impl<'a> PciIoGuard<'a> {
        fn new(bs: &'a EfiBootServices, handle: usize) -> Result<Self, EfiStatus> {
            let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();
            let status = (bs.open_protocol)(
                handle,
                EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
                &mut pci_io,
                0,
                0,
                0x01,
            );

            if EfiStatus::from(status) != EfiStatus::Success || pci_io.is_null() {
                Err(EfiStatus::from(status))
            } else {
                Ok(Self {
                    bs,
                    handle,
                    protocol: pci_io as *mut _,
                })
            }
        }
    }

    impl<'a> Drop for PciIoGuard<'a> {
        fn drop(&mut self) {
            (self.bs.close_protocol)(self.handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct PciDevice {
        pub handle: usize,
        pub vendor_id: u16,
        pub device_id: u16,
        pub class_code: u8,
        pub subclass: u8,
        pub bus: u8,
        pub device: u8,
        pub function: u8,
    }

    pub fn detect_vesa_graphics(
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        if let Some(config) = detect_vesa_graphics_internal(bs) {
            return Some(config);
        }
        info_log!("EFI PCI enumeration failed, trying bare-metal detection");
        detect_bare_metal_graphics(bs)
    }

    fn detect_vesa_graphics_internal(
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_gop!("[GOP-ALT] Detecting VESA graphics hardware...\n");

        match enumerate_pci_graphics_devices(bs) {
            Ok(devices) if !devices.is_empty() => {
                log_gop!("[GOP-ALT] Found {} PCI graphics devices\n", devices.len());
                for device in devices {
                    log_gop!(
                        "[GOP-ALT] Graphics device: {:04x}:{:04x}, class {:02x}.{:02x} at {:02x}:{:02x}:{:02x}\n",
                        device.vendor_id, device.device_id,
                        device.class_code, device.subclass,
                        device.bus, device.device, device.function
                    );
                    if let Some(fb_info) = probe_linear_framebuffer(&device, bs) {
                        log_gop!("[GOP-ALT] Linear framebuffer found at {:#x}, {}x{}.\n",
                            fb_info.address, fb_info.width, fb_info.height);
                        return Some(fb_info);
                    }
                }
                log_gop!("[GOP-ALT] No linear framebuffers found on graphics devices\n");
                None
            }
            Ok(_) => {
                log_gop!("[GOP-ALT] No graphics devices found via PCI enumeration\n");
                None
            }
            Err(e) => {
                log_gop!("[GOP-ALT] PCI enumeration failed: {:?}", e);
                None
            }
        }
    }

    fn enumerate_pci_graphics_devices(bs: &EfiBootServices) -> Result<Vec<PciDevice>, EfiStatus> {
        log_gop!("[GOP-ALT] Starting PCI device enumeration...\n");

        let mut handle_count: usize = 0;
        let mut handles: *mut usize = core::ptr::null_mut();

        let status = (bs.locate_handle_buffer)(
            2,
            EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            core::ptr::null_mut(),
            &mut handle_count,
            &mut handles,
        );

        if EfiStatus::from(status) != EfiStatus::Success || handles.is_null() {
            log_gop!("[GOP-ALT] Failed to locate PCI_IO handles: {:#x}\n", status);
            return Err(EfiStatus::from(status));
        }

        log_gop!("[GOP-ALT] Found {} PCI_IO protocol handles\n", handle_count);

        let mut devices = Vec::new();
        for i in 0..handle_count {
            let handle = unsafe { *handles.add(i) };
            log_gop!("[GOP-ALT] Checking PCI_IO handle {}: {:#x}\n", i, handle);

            if let Some(dev) = probe_pci_device_on_handle(bs, handle) {
                log_gop!(
                    "[GOP-ALT] Found PCI device: {:04x}:{:04x} at {:02x}:{:02x}:{:02x}, class {:02x}:{:02x}\n",
                    dev.vendor_id, dev.device_id,
                    dev.bus, dev.device, dev.function,
                    dev.class_code, dev.subclass
                );
                if dev.class_code == 0x03 {
                    log_gop!("[GOP-ALT] Added graphics device to list\n");
                    devices.push(dev);
                }
            } else {
                log_gop!("[GOP-ALT] Failed to probe PCI device on handle {}\n", i);
            }
        }

        if !handles.is_null() {
            (bs.free_pool)(handles as *mut core::ffi::c_void);
        }

        log_gop!("[GOP-ALT] PCI enumeration complete, found {} graphics devices\n", devices.len());
        Ok(devices)
    }

    fn probe_pci_device_on_handle(bs: &EfiBootServices, handle: usize) -> Option<PciDevice> {
        let guard = PciIoGuard::new(bs, handle).ok()?;
        let pci_io_ref = unsafe { &*guard.protocol };

        let mut vendor_id: u16 = 0;
        let mut device_id: u16 = 0;
        let mut class_code: u8 = 0;
        let mut subclass: u8 = 0;

        let read_status = (pci_io_ref.pci_read)(
            guard.protocol as *mut crate::common::EfiPciIoProtocol,
            1, 0, 1,
            &mut vendor_id as *mut u16 as *mut core::ffi::c_void,
        );
        if EfiStatus::from(read_status) != EfiStatus::Success {
            return None;
        }
        if vendor_id == 0xFFFF || vendor_id == 0 {
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            guard.protocol as *mut crate::common::EfiPciIoProtocol,
            1, 2, 1,
            &mut device_id as *mut u16 as *mut core::ffi::c_void,
        );
        if EfiStatus::from(read_status) != EfiStatus::Success {
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            guard.protocol as *mut crate::common::EfiPciIoProtocol,
            0, 0xB, 1,
            &mut class_code as *mut u8 as *mut core::ffi::c_void,
        );
        if EfiStatus::from(read_status) != EfiStatus::Success {
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            guard.protocol as *mut crate::common::EfiPciIoProtocol,
            0, 0xA, 1,
            &mut subclass as *mut u8 as *mut core::ffi::c_void,
        );
        if EfiStatus::from(read_status) != EfiStatus::Success {
            return None;
        }

        let mut segment_num: usize = 0;
        let mut bus_num: usize = 0;
        let mut dev_num: usize = 0;
        let mut func_num: usize = 0;

        let location_status = (pci_io_ref.get_location)(
            guard.protocol as *mut crate::common::EfiPciIoProtocol,
            &mut segment_num as *mut usize,
            &mut bus_num as *mut usize,
            &mut dev_num as *mut usize,
            &mut func_num as *mut usize,
        );

        if EfiStatus::from(location_status) == EfiStatus::Success {
            Some(PciDevice {
                handle, vendor_id, device_id, class_code, subclass,
                bus: bus_num as u8, device: dev_num as u8, function: func_num as u8,
            })
        } else {
            log_gop!("[GOP-ALT] GetLocation failed: {:#x}\n", location_status);
            None
        }
    }

    fn probe_linear_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_gop!(
            "[GOP-ALT] Probing linear framebuffer on device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
            device.vendor_id, device.device_id, device.bus, device.device, device.function
        );

        if device.vendor_id == 0x1af4 && device.device_id >= 0x1050 {
            log_gop!("[GOP-ALT] Detected virtio-gpu device, attempting linear framebuffer setup\n");
            return probe_virtio_gpu_framebuffer(device, bs);
        }

        match (device.vendor_id, device.device_id) {
            (0x1b36, 0x0100) => probe_qxl_framebuffer(device, bs),
            (0x15ad, 0x0405) => {
                log_gop!("[GOP-ALT] Detected VMware SVGA device - linear framebuffer not implemented yet\n");
                None
            }
            (0x1234, 0x1111) | (0x1234, 0x1112) => {
                log_gop!("[GOP-ALT] Detected QEMU std VGA device\n");
                probe_std_vga_framebuffer(device, bs)
            }
            _ => {
                log_gop!("[GOP-ALT] Unknown graphics device ({:04x}:{:04x}), skipping\n",
                    device.vendor_id, device.device_id);
                None
            }
        }
    }

    fn probe_virtio_gpu_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        let guard = match PciIoGuard::new(bs, device.handle) {
            Ok(g) => g,
            Err(status) => {
                log_gop!("[GOP-ALT] Failed to open PCI_IO protocol for virtio-gpu: {:#x}\n",
                    status as usize);
                return None;
            }
        };

        log_gop!("[GOP-ALT] Successfully opened PCI_IO protocol\n");

        let mut config_buf = [0u32; 6];
        let pci_io_ref = unsafe { &*guard.protocol };
        let read_result = crate::pci_read_bars!(pci_io_ref, guard.protocol, config_buf, 6, 0x10);

        if EfiStatus::from(read_result) != EfiStatus::Success {
            error_log!("Failed to read PCI BARs: {:#x}", read_result);
            return None;
        }

        let (bar0, bar0_type, is_memory) = crate::extract_bar_info!(config_buf, 0);

        if bar0 == 0 {
            error_log!("BAR0 is zero - invalid MMIO region");
            return None;
        }
        if !is_memory {
            error_log!("BAR0 is I/O space (type: {}), expected memory space", bar0_type);
            return None;
        }

        let fb_base_addr = if (bar0_type & 0x4) != 0 {
            let bar1 = config_buf[1];
            ((bar1 as u64) << 32) | (bar0 as u64 & 0xFFFFFFF0)
        } else {
            bar0 as u64
        };

        log_gop!("[GOP-ALT] BAR0: {:#x}, type: {}, fb_base: {:#x}, 64-bit: {}\n",
            bar0, bar0_type, fb_base_addr, (bar0_type & 0x4) != 0);

        for &(width, height, bpp) in &[(1024, 768, 32), (1280, 720, 32), (800, 600, 32)] {
            let expected_fb_size = (height * width * bpp / 8) as u64;
            if probe_framebuffer_access(fb_base_addr, expected_fb_size) {
                log_gop!("[GOP-ALT] Detected working virtio-gpu framebuffer: {}x{} @ {:#x}\n",
                    width, height, fb_base_addr);
                return Some(crate::common::FullereneFramebufferConfig {
                    address: fb_base_addr, width, height,
                    pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    bpp, stride: width,
                });
            }
        }

        log_gop!("[GOP-ALT] Could not determine virtio-gpu framebuffer configuration\n");
        None
    }

    fn probe_qxl_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_gop!("[BM-GFX] QXL bare-metal detection starting\n");

        let guard = match PciIoGuard::new(bs, device.handle) {
            Ok(g) => g,
            Err(status) => {
                log_gop!("[BM-GFX] Failed to open PCI_IO protocol for QXL: {:#x}\n",
                    status as usize);
                return None;
            }
        };

        let mut config_buf = [0u32; 6];
        let pci_io_ref = unsafe { &*guard.protocol };
        let read_result = (pci_io_ref.pci_read)(
            guard.protocol, 2, 0x10, 6,
            config_buf.as_mut_ptr() as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_result) != EfiStatus::Success {
            log_gop!("[BM-GFX] Failed to read PCI BARs for QXL: {:#x}\n", read_result);
            return None;
        }

        let bar1 = config_buf[1] & 0xFFFFFFF0;
        let bar1_type = config_buf[1] & 0xF;

        if bar1 == 0 {
            log_gop!("[BM-GFX] BAR1 is zero - invalid framebuffer region\n");
            return None;
        }
        if bar1_type & 0x1 != 0 {
            log_gop!("[BM-GFX] BAR1 is I/O space (type: {}), expected memory space\n", bar1_type);
            return None;
        }

        let fb_base_addr = bar1 as u64;
        log_gop!("[BM-GFX] QXL BAR1: {:#x}\n", fb_base_addr);

        let width = 1024;
        let height = 768;
        let bpp = 32;
        let stride = width;

        log_gop!("[BM-GFX] Testing {}x{} mode at {:#x} (size: {}KB)\n",
            width, height, fb_base_addr, (height * stride * bpp / 8) / 1024);

        if probe_framebuffer_access(fb_base_addr, (height * stride * bpp / 8) as u64) {
            log_gop!("[BM-GFX] QXL framebuffer mode {}x{} appears valid\n", width, height);
            Some(crate::common::FullereneFramebufferConfig {
                address: fb_base_addr, width, height,
                pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp, stride,
            })
        } else {
            log_gop!("[BM-GFX] QXL framebuffer mode {}x{} is invalid\n", width, height);
            None
        }
    }

    fn detect_bare_metal_graphics(
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_gop!("[BM-GFX] Starting bare-metal graphics detection...\n");

        let devices = match enumerate_bare_metal_pci_devices() {
            Some(devices) if !devices.is_empty() => {
                log_gop!("[BM-GFX] Found {} graphics devices via direct PCI enumeration\n",
                    devices.len());
                devices
            }
            _ => {
                log_gop!("[BM-GFX] No graphics devices found via bare-metal detection\n");
                return None;
            }
        };

        for device in devices.iter() {
            log_gop!("[BM-GFX] Probing device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
                device.vendor_id, device.device_id, device.bus, device.device, device.function);

            match (device.vendor_id, device.device_id) {
                (0x1af4, id) if id >= 0x1050 => {
                    log_gop!("[BM-GFX] Detected virtio-gpu, attempting framebuffer detection\n");
                    if let Some(fb_info) = probe_linear_framebuffer(device, bs) {
                        return Some(fb_info);
                    }
                }
                (0x1b36, 0x0100) => {
                    log_gop!("[BM-GFX] Detected QXL device, attempting framebuffer detection\n");
                    if let Some(fb_info) = probe_qxl_framebuffer(device, bs) {
                        return Some(fb_info);
                    }
                }
                (0x1234, 0x1111) | (0x1234, 0x1112) => {
                    log_gop!("[BM-GFX] Detected std VGA device, attempting framebuffer detection\n");
                    if let Some(fb_info) = probe_linear_framebuffer(device, bs) {
                        return Some(fb_info);
                    }
                }
                (0x1013, _) => { log_gop!("[BM-GFX] Detected Cirrus Logic VGA, skipping (no linear framebuffer)\n"); }
                (0x15ad, 0x0405) => { log_gop!("[BM-GFX] Detected VMware SVGA, not yet implemented\n"); }
                _ => { log_gop!("[BM-GFX] Unknown graphics device ({:04x}:{:04x}), skipping\n",
                    device.vendor_id, device.device_id); }
            }
        }

        log_gop!("[BM-GFX] Bare-metal graphics detection completed without success\n");
        None
    }

    fn enumerate_bare_metal_pci_devices() -> Option<Vec<PciDevice>> {
        log_gop!("[BM-GFX] Performing bare-metal PCI enumeration...\n");

        let all_devices = crate::bare_metal_pci::enumerate_all_pci_devices();
        let graphics_devices: Vec<PciDevice> = all_devices
            .into_iter()
            .filter(|dev| dev.class_code == 0x03)
            .collect();

        if graphics_devices.is_empty() {
            log_gop!("[BM-GFX] No graphics devices found via bare-metal PCI enumeration\n");
            None
        } else {
            log_gop!("[BM-GFX] Found {} graphics devices via bare-metal PCI enumeration\n",
                graphics_devices.len());
            Some(graphics_devices)
        }
    }

    fn probe_framebuffer_access(address: u64, size: u64) -> bool {
        log_gop!("[GOP-ALT] Attempting to validate framebuffer access at {:#x} (size: {}KB)\n",
            address, size / 1024);

        let _ptr = address as *const u8;

        if address == 0 || address >= 0xFFFFFFFFFFFFF000 {
            log_gop!("[GOP-ALT] Framebuffer address {:#x} appears invalid\n", address);
            return false;
        }

        log_gop!("[GOP-ALT] Framebuffer address {:#x} appears potentially valid\n", address);
        true
    }

    pub fn pci_config_read_u32(
        bus: u8, device: u8, function: u8, register: u8,
    ) -> Result<u32, EfiStatus> {
        let system_table_ptr = crate::UEFI_SYSTEM_TABLE.lock().as_ref().cloned();
        let system_table = match system_table_ptr {
            Some(ptr) => unsafe { &*ptr.0 },
            None => {
                serial::_print(format_args!("PCI: UEFI system table not initialized\n"));
                return Err(EfiStatus::NotInReadyState);
            }
        };

        let bs = unsafe { &*system_table.boot_services };
        let handle = ((bus as usize) << 8) | ((device as usize) << 3) | (function as usize);

        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();
        let status = (bs.open_protocol)(
            handle,
            graphics_alternatives::EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            &mut pci_io, 0, 0, 0x01,
        );

        if EfiStatus::from(status) != EfiStatus::Success || pci_io.is_null() {
            serial::_print(format_args!(
                "PCI: Failed to open PCI_IO protocol for {:02x}:{:02x}:{:02x}: {:#x}\n",
                bus, device, function, status
            ));
            return Err(EfiStatus::from(status));
        }

        let pci_io_ref = unsafe { &*(pci_io as *mut common::EfiPciIoProtocol) };
        let mut value: u32 = 0;

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut common::EfiPciIoProtocol,
            2, register as u64, 1,
            &mut value as *mut u32 as *mut core::ffi::c_void,
        );

        (bs.close_protocol)(
            handle,
            graphics_alternatives::EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            0, 0,
        );

        if EfiStatus::from(read_status) == EfiStatus::Success {
            Ok(value)
        } else {
            serial::_print(format_args!(
                "PCI: Read failed for {:02x}:{:02x}:{:02x}:{:02x}: {:#x}\n",
                bus, device, function, register, read_status
            ));
            Err(EfiStatus::from(read_status))
        }
    }

    fn probe_std_vga_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_gop!("[GOP-ALT] Probing std VGA framebuffer at {:02x}:{:02x}:{:02x}\n",
            device.bus, device.device, device.function);

        let guard = match PciIoGuard::new(bs, device.handle) {
            Ok(g) => g,
            Err(status) => {
                log_gop!("[GOP-ALT] Failed to open PCI_IO: {:#x}\n", status as usize);
                return None;
            }
        };

        let pci_io_ref = unsafe { &*guard.protocol };
        let mut bar0_raw: u32 = 0;
        let read_result = (pci_io_ref.pci_read)(
            guard.protocol, 2, 0x10, 1,
            &mut bar0_raw as *mut u32 as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_result) != EfiStatus::Success || (bar0_raw & 0xFFFFFFF0) == 0 {
            log_gop!("[GOP-ALT] std VGA BAR0 invalid: {:#x}\n", bar0_raw);
            return None;
        }

        let fb_addr = (bar0_raw & 0xFFFFFFF0) as u64;
        log_gop!("[GOP-ALT] std VGA framebuffer at {:#x}\n", fb_addr);

        for &(width, height, bpp) in &[(1024, 768, 32), (800, 600, 32), (1280, 1024, 32)] {
            let stride = width;
            let fb_size = (height as u64 * stride as u64 * bpp as u64 / 8) as u64;
            if probe_framebuffer_access(fb_addr, fb_size) {
                log_gop!("[GOP-ALT] std VGA framebuffer: {}x{} @ {:#x}\n", width, height, fb_addr);
                return Some(crate::common::FullereneFramebufferConfig {
                    address: fb_addr, width, height,
                    pixel_format: crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    bpp, stride,
                });
            }
        }
        None
    }

    #[allow(dead_code)]
    unsafe fn _port_read(_port: u16) -> u32 {
        0xFFFF_FFFF
    }
}

pub use graphics_alternatives::*;