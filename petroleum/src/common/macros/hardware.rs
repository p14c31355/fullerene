//! Hardware operation macros for Fullerene OS

#[macro_export]
macro_rules! pci_read_bars {
    ($pci_io_ref:expr, $protocol_ptr:expr, $buf:expr, $count:expr, $offset:expr) => {{
        ($pci_io_ref.pci_read)(
            $protocol_ptr,
            2, // Dword width
            $offset,
            $count,
            $buf.as_mut_ptr() as *mut core::ffi::c_void,
        )
    }};
}

#[macro_export]
macro_rules! extract_bar_info {
    ($bars:expr, $bar_index:expr) => {{
        let bar = $bars[$bar_index] & 0xFFFFFFF0; // Mask off lower 4 bits
        let bar_type = $bars[$bar_index] & 0xF;
        let is_memory = (bar_type & 0x1) == 0;
        (bar, bar_type, is_memory)
    }};
}

#[macro_export]
macro_rules! test_framebuffer_mode {
    ($addr:expr, $width:expr, $height:expr, $bpp:expr, $stride:expr) => {{
        let fb_size = ($height * $stride * $bpp / 8) as u64;
        if crate::graphics_alternatives::probe_framebuffer_access($addr, fb_size) {
            info_log!(
                "Detected valid framebuffer: {}x{} @ {:#x}",
                $width,
                $height,
                $addr
            );
            Some($crate::common::FullereneFramebufferConfig {
                address: $addr,
                width: $width,
                height: $height,
                pixel_format:
                    $crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp: $bpp,
                stride: $stride,
            })
        } else {
            warn_log!("Framebuffer mode {}x{} invalid", $width, $height);
            None
        }
    }};
}

#[macro_export]
macro_rules! volatile_write {
    ($ptr:expr, $val:expr) => {
        unsafe { core::ptr::write_volatile($ptr, $val) }
    };
}

#[macro_export]
macro_rules! volatile_ops {
    (read, $addr:expr, $ty:ty) => {
        unsafe { core::ptr::read_volatile($addr as *const $ty) }
    };
    (write, $addr:expr, $val:expr) => {
        unsafe { core::ptr::write_volatile($addr as *mut _, $val) }
    };
}

#[macro_export]
macro_rules! init_serial_port {
    ($line_ctrl_port:expr, $data_port:expr, $irq_enable_port:expr, $fifo_ctrl_port:expr, $modem_ctrl_port:expr, $dlab:expr, $divisor_low:expr, $irq:expr, $line_ctrl:expr, $fifo:expr, $modem:expr) => {{
        unsafe {
            use nitrogen::port::HardwarePorts;

            $crate::write_serial_bytes(HardwarePorts::SERIAL_DATA_PORT, HardwarePorts::SERIAL_LINE_STATUS_PORT, b"DEBUG: init_serial_port start\n");
            
            $line_ctrl_port.write($dlab);
            $data_port.write($divisor_low);
            $irq_enable_port.write($irq);
            $line_ctrl_port.write($line_ctrl);
            $fifo_ctrl_port.write($fifo);
            $modem_ctrl_port.write($modem);

            $crate::write_serial_bytes(HardwarePorts::SERIAL_DATA_PORT, HardwarePorts::SERIAL_LINE_STATUS_PORT, b"DEBUG: init_serial_port end\n");
        }
    }};
}

#[macro_export]
macro_rules! pci_config_read {
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 32) => {
        $crate::bare_metal_pci::pci_config_read_dword($bus, $device, $function, $reg)
    };
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 16) => {
        $crate::bare_metal_pci::pci_config_read_word($bus, $device, $function, $reg)
    };
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 8) => {
        $crate::bare_metal_pci::pci_config_read_byte($bus, $device, $function, $reg)
    };
}
