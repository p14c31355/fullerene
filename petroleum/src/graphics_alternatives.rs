use super::*;
use crate::common::{EfiBootServices, EfiStatus};
use alloc::boxed::Box;
use alloc::vec::Vec;

/// Alternative graphics detection methods when GOP is unavailable
pub mod graphics_alternatives {
    use super::*;
    use crate::serial::_print;
    use alloc::vec::Vec;

    const EFI_PCI_IO_PROTOCOL_GUID: [u8; 16] = [
        0x4c, 0xf2, 0x39, 0x77, 0xd7, 0x93, 0xd4, 0x11, 0x9a, 0x3a, 0x00, 0x90, 0x27, 0x3f, 0xc1,
        0x4d,
    ];

    #[derive(Debug, Clone, Copy)]
    pub struct PciDevice {
        pub handle: usize, // EFI_HANDLE
        pub vendor_id: u16,
        pub device_id: u16,
        pub class_code: u8,
        pub subclass: u8,
        pub bus: u8,
        pub device: u8,
        pub function: u8,
    }

    /// Try to detect VESA-compatible graphics hardware using PCI enumeration
    pub fn detect_vesa_graphics(
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[GOP-ALT] Detecting VESA graphics hardware...\n"
        ));

        // Try PCI enumeration for graphics devices
        match enumerate_pci_graphics_devices(bs) {
            Ok(devices) if !devices.is_empty() => {
                _print(format_args!(
                    "[GOP-ALT] Found {} PCI graphics devices\n",
                    devices.len()
                ));
                for device in devices {
                    _print(format_args!(
                        "[GOP-ALT] Graphics device: {:04x}:{:04x}, class {:02x}.{:02x} at {:02x}:{:02x}:{:02x}\n",
                        device.vendor_id,
                        device.device_id,
                        device.class_code,
                        device.subclass,
                        device.bus,
                        device.device,
                        device.function
                    ));

                    // Check if this device supports linear framebuffer mode
                    if let Some(fb_info) = probe_linear_framebuffer(&device, bs) {
                        _print(format_args!(
                            "[GOP-ALT] Linear framebuffer found at {:#x}, {}x{}.\n",
                            fb_info.address, fb_info.width, fb_info.height
                        ));
                        return Some(fb_info);
                    }
                }
                _print(format_args!(
                    "[GOP-ALT] No linear framebuffers found on graphics devices\n"
                ));
                None
            }
            Ok(_) => {
                _print(format_args!(
                    "[GOP-ALT] No graphics devices found via PCI enumeration\n"
                ));
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
        _print(format_args!(
            "[GOP-ALT] Starting PCI device enumeration...\n"
        ));

        // First, enumerate all PCI_IO handles
        let mut handle_count: usize = 0;
        let mut handles: *mut usize = core::ptr::null_mut();

        let status = (bs.locate_handle_buffer)(
            2, // ByProtocol
            EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            core::ptr::null_mut(),
            &mut handle_count,
            &mut handles,
        );

        if EfiStatus::from(status) != EfiStatus::Success || handles.is_null() {
            _print(format_args!(
                "[GOP-ALT] Failed to locate PCI_IO handles: {:#x}\n",
                status
            ));
            return Err(EfiStatus::from(status));
        }

        _print(format_args!(
            "[GOP-ALT] Found {} PCI_IO protocol handles\n",
            handle_count
        ));

        let mut devices = Vec::new();

        // Process each PCI_IO handle
        for i in 0..handle_count {
            let handle = unsafe { *handles.add(i) };
            _print(format_args!(
                "[GOP-ALT] Checking PCI_IO handle {}: {:#x}\n",
                i, handle
            ));

            if let Some(dev) = probe_pci_device_on_handle(bs, handle) {
                _print(format_args!(
                    "[GOP-ALT] Found PCI device: {:04x}:{:04x} at {:02x}:{:02x}:{:02x}, class {:02x}:{:02x}\n",
                    dev.vendor_id,
                    dev.device_id,
                    dev.bus,
                    dev.device,
                    dev.function,
                    dev.class_code,
                    dev.subclass
                ));

                // Check if it's a graphics device (Display controller class, 0x03)
                if dev.class_code == 0x03 {
                    _print(format_args!("[GOP-ALT] Added graphics device to list\n"));
                    devices.push(dev);
                }
            } else {
                _print(format_args!(
                    "[GOP-ALT] Failed to probe PCI device on handle {}\n",
                    i
                ));
            }
        }

        // Free handle buffer
        if !handles.is_null() {
           (bs.free_pool)(handles as *mut core::ffi::c_void);
        }

        _print(format_args!(
            "[GOP-ALT] PCI enumeration complete, found {} graphics devices\n",
            devices.len()
        ));

        Ok(devices)
    }

    /// Probe PCI device information from a given handle
    fn probe_pci_device_on_handle(bs: &EfiBootServices, handle: usize) -> Option<PciDevice> {
        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();

        let protocol_status = (bs.open_protocol)(
            handle,
            EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            &mut pci_io,
            0,    // AgentHandle (null for now)
            0,    // ControllerHandle (null)
            0x01, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
        );

        if EfiStatus::from(protocol_status) != EfiStatus::Success || pci_io.is_null() {
            return None;
        }

        // Read PCI configuration header using PCI_IO protocol
        let pci_io_ref = unsafe { &*(pci_io as *mut crate::common::EfiPciIoProtocol) };

        // Read PCI configuration header using the proper protocol functions
        let mut vendor_id: u16 = 0;
        let mut device_id: u16 = 0;
        let mut class_code: u8 = 0;
        let mut subclass: u8 = 0;

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            1, // Word width for vendor_id
            0, // Offset 0
            1, // 1 word
            &mut vendor_id as *mut u16 as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_status) != EfiStatus::Success {
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        // Skip invalid devices
        if vendor_id == 0xFFFF || vendor_id == 0 {
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            1, // Word width for device_id
            2, // Offset 2
            1, // 1 word
            &mut device_id as *mut u16 as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_status) != EfiStatus::Success {
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            0,   // Byte width for class_code
            0xB, // Offset 0xB
            1,   // 1 byte
            &mut class_code as *mut u8 as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_status) != EfiStatus::Success {
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            0,   // Byte width for subclass
            0xA, // Offset 0xA
            1,   // 1 byte
            &mut subclass as *mut u8 as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_status) != EfiStatus::Success {
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        // Now we need to get bus/device/function info
        // Use GetLocation function from the protocol
        let mut segment_num: usize = 0;
        let mut bus_num: usize = 0;
        let mut dev_num: usize = 0;
        let mut func_num: usize = 0;

        let location_status = (pci_io_ref.get_location)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            &mut segment_num as *mut usize,
            &mut bus_num as *mut usize,
            &mut dev_num as *mut usize,
            &mut func_num as *mut usize,
        );

        // Close protocol before returning
       (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);

        if EfiStatus::from(location_status) == EfiStatus::Success {
            Some(PciDevice {
                handle,
                vendor_id,
                device_id,
                class_code,
                subclass,
                bus: bus_num as u8,
                device: dev_num as u8,
                function: func_num as u8,
            })
        } else {
            _print(format_args!(
                "[GOP-ALT] GetLocation failed: {:#x}\n",
                location_status
            ));
            None
        }
    }

    /// Probe for linear framebuffer on a graphics device
    fn probe_linear_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[GOP-ALT] Probing linear framebuffer on device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
            device.vendor_id, device.device_id, device.bus, device.device, device.function
        ));

        // Check for known virtio-gpu device IDs (vendor: 0x1af4, devices: 0x1050+)
        if device.vendor_id == 0x1af4 && device.device_id >= 0x1050 {
            _print(format_args!(
                "[GOP-ALT] Detected virtio-gpu device, attempting linear framebuffer setup\n"
            ));
            return probe_virtio_gpu_framebuffer(device, bs);
        }

        // Check for other devices that might support linear framebuffers
        // Could add support for qxl, vmware svga, etc.
        match (device.vendor_id, device.device_id) {
            (0x1b36, 0x0100) => {
                // QEMU QXL device
                _print(format_args!(
                    "[GOP-ALT] Detected QXL device - linear framebuffer not implemented yet\n"
                ));
                None
            }
            (0x15ad, 0x0405) => {
                // VMware SVGA II
                _print(format_args!(
                    "[GOP-ALT] Detected VMware SVGA device - linear framebuffer not implemented yet\n"
                ));
                None
            }
            _ => {
                _print(format_args!(
                    "[GOP-ALT] Unknown graphics device, skipping linear framebuffer probe\n"
                ));
                None
            }
        }
    }

    /// Probe virtio-gpu device for linear framebuffer capability
    fn probe_virtio_gpu_framebuffer(
        device: &PciDevice,
        bs: &EfiBootServices,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        // Build PCI configuration address for this device location (used as handle)
        let handle = crate::bare_metal_pci::build_pci_config_address(device.bus, device.device, device.function, 0) as usize;

        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();
        let status = (bs.open_protocol)(
            handle,
            EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            &mut pci_io,
            0,    // AgentHandle
            0,    // ControllerHandle
            0x01, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
        );

        if EfiStatus::from(status) != EfiStatus::Success || pci_io.is_null() {
            _print(format_args!(
                "[GOP-ALT] Failed to open PCI_IO protocol for virtio-gpu: {:#x}\n",
                status
            ));
            return None;
        }

        _print(format_args!(
            "[GOP-ALT] Successfully opened PCI_IO protocol\n"
        ));

        // Read PCI configuration to get BAR information
        let mut config_buf = [0u32; 6]; // First 24 bytes (6 dwords) contain BAR0-BAR5

        // Create a reference to the protocol for calling methods
        let pci_io_ref = unsafe { &*(pci_io as *mut crate::common::EfiPciIoProtocol) };

        let read_result = (pci_io_ref.pci_read)(
            pci_io as *mut crate::common::EfiPciIoProtocol,
            2,    // Dword width
            0x10, // Offset - BAR0 offset (0x10)
            6,    // Count - 6 BARs
            config_buf.as_mut_ptr() as *mut core::ffi::c_void,
        );

        if EfiStatus::from(read_result) != EfiStatus::Success {
            _print(format_args!(
                "[GOP-ALT] Failed to read PCI BARs: {:#x}\n",
                read_result
            ));
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        // Analyze BAR0 (typically the framebuffer for virtio-gpu)
        let bar0 = config_buf[0] & 0xFFFFFFF0; // Mask off lower 4 bits (flags)
        let bar0_type = config_buf[0] & 0xF;

        if bar0 == 0 {
            _print(format_args!(
                "[GOP-ALT] BAR0 is zero - invalid MMIO region\n"
            ));
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
            return None;
        }

        // Check if BAR0 is a memory-mapped region (bits 0-1 = 00 for 32-bit memory, 10 for 64-bit)
        if bar0_type & 0x1 != 0 {
            _print(format_args!(
                "[GOP-ALT] BAR0 is I/O space (type: {}), expected memory space\n",
                bar0_type
            ));
           (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);
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

        // Fix logging - remove protocol debug since it's already converted to status
        _print(format_args!(
            "[GOP-ALT] BAR0: {:#x}, type: {}, fb_base: {:#x}, 64-bit: {}\n",
            bar0, bar0_type, fb_base_addr, is_64bit
        ));

        // For virtio-gpu, we need to initialize the device first
        // This involves writing to the device registers in MMIO space
        // But since we don't have the capability to write to MMIO yet,
        // we'll assume a default configuration and try to read from a known offset

        // For virtio-gpu in QEMU, default resolution is typically 1024x768 or 1280x720
        // Try to detect by attempting to access the framebuffer
        let standard_modes = [(1024, 768, 32), (1280, 720, 32), (800, 600, 32)];

        for (width, height, bpp) in standard_modes.iter() {
            let stride = *width; // Assume pixels_per_scan_line = width
            let expected_fb_size = (*height * stride * bpp / 8) as u64;

            // Try to validate framebuffer access (this is a very basic check)
            if probe_framebuffer_access(fb_base_addr, expected_fb_size) {
                _print(format_args!(
                    "[GOP-ALT] Detected working virtio-gpu framebuffer: {}x{} @ {:#x}\n",
                    width, height, fb_base_addr
                ));

                // Close PCI_IO protocol
               (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);

                return Some(crate::common::FullereneFramebufferConfig {
                    address: fb_base_addr,
                    width: *width,
                    height: *height,
                    pixel_format:
                        crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                    bpp: *bpp,
                    stride: stride,
                });
            }
        }

        // If no standard mode worked, try to determine size by PCI register
        // Read BAR0 size by writing all 1s and reading back (but we can't do that without PCI_IO write access)

        _print(format_args!(
            "[GOP-ALT] Could not determine virtio-gpu framebuffer configuration\n"
        ));

        // Close PCI_IO protocol
       (bs.close_protocol)(handle, EFI_PCI_IO_PROTOCOL_GUID.as_ptr(), 0, 0);

        None
    }

    /// Try to validate framebuffer access at the given address
    fn probe_framebuffer_access(address: u64, size: u64) -> bool {
        // This is a very basic probe - in UEFI we should use proper memory mapping
        // For now, we'll just try to read from the address and see if it's accessible

        _print(format_args!(
            "[GOP-ALT] Attempting to validate framebuffer access at {:#x} (size: {}KB)\n",
            address,
            size / 1024
        ));

        // Try reading first few bytes to see if memory is accessible
        // We need to do this very carefully to avoid crashes
        let _ptr = address as *const u8;

        // Check if the address looks valid (not null, not too high)
        if address == 0 || address >= 0xFFFFFFFFFFFFF000 {
            _print(format_args!(
                "[GOP-ALT] Framebuffer address {:#x} appears invalid\n",
                address
            ));
            return false;
        }

        // In UEFI, we should use memory services to allocate/map this range first
        // For now, we'll assume the PCI_IO memory operations will handle this
        // when we actually access the framebuffer later

        _print(format_args!(
            "[GOP-ALT] Framebuffer address {:#x} appears potentially valid\n",
            address
        ));
        true // Assume valid for now - real validation would need proper mem mapping
    }



    /// Read PCI configuration register using EFI_PCI_IO_PROTOCOL
    /// This function maps bus:device:function:register addressing to protocol calls
    pub fn pci_config_read_u32(
        bus: u8,
        device: u8,
        function: u8,
        register: u8,
    ) -> Result<u32, EfiStatus> {
        // Get UEFI system table
        let system_table_ptr = UEFI_SYSTEM_TABLE.lock().as_ref().cloned();
        let system_table = match system_table_ptr {
            Some(ptr) => unsafe { &*ptr.0 },
            None => {
                serial::_print(format_args!("PCI: UEFI system table not initialized\n"));
                return Err(EfiStatus::NotInReadyState);
            }
        };

        let bs = unsafe { &*system_table.boot_services };

        // Build PCI handle for this device location
        let handle = ((bus as usize) << 8) | ((device as usize) << 3) | (function as usize);

        let mut pci_io: *mut core::ffi::c_void = core::ptr::null_mut();
        let status = (bs.open_protocol)(
            handle,
            graphics_alternatives::EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            &mut pci_io,
            0,    // AgentHandle
            0,    // ControllerHandle
            0x01, // EFI_OPEN_PROTOCOL_BY_HANDLE_PROTOCOL
        );

        if EfiStatus::from(status) != EfiStatus::Success || pci_io.is_null() {
            serial::_print(format_args!(
                "PCI: Failed to open PCI_IO protocol for {:02x}:{:02x}:{:02x}: {:#x}\n",
                bus, device, function, status
            ));
            return Err(EfiStatus::from(status));
        }

        // Read using PCI_IO protocol
        let pci_io_ref = unsafe { &*(pci_io as *mut common::EfiPciIoProtocol) };
        let mut value: u32 = 0;

        let read_status = (pci_io_ref.pci_read)(
            pci_io as *mut common::EfiPciIoProtocol,
            2, // Dword width
            register as u64,
            1, // Count
            &mut value as *mut u32 as *mut core::ffi::c_void,
        );

        // Close protocol
       (bs.close_protocol)(
            handle,
            graphics_alternatives::EFI_PCI_IO_PROTOCOL_GUID.as_ptr(),
            0,
            0,
        );

        if EfiStatus::from(read_status) == EfiStatus::Success {
            Ok(value)
        } else {
            serial::_print(format_args!(
                "PCI: Read failed for {:02x}:{:02x}:{:02x}:{:02x}: {:#x}\n",
                bus, device, function, register, read_status
            ));
            return Err(EfiStatus::from(read_status));
        }
    }

    /// Read from PCI configuration space (simplified - needs proper implementation)
    unsafe fn _port_read(_port: u16) -> u32 {
        // This function is deprecated in UEFI context
        // Use pci_config_read_u32() instead for proper UEFI PCI access
        // For bare-metal compatibility, return invalid
        0xFFFF_FFFF // Invalid read
    }
}

pub use graphics_alternatives::*;
