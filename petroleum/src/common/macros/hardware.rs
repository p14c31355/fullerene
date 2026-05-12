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
    ($self:expr, $dlab:expr, $divisor_low:expr, $irq:expr, $line_ctrl:expr, $fifo:expr, $modem:expr) => {{
        unsafe {
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: init_serial_port start\n");
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing dlab\n");
            $self.ops.line_ctrl_port().write($dlab); // Enable DLAB
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing divisor\n");
            $self.ops.data_port().write($divisor_low); // Baud rate divisor low byte
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing irq\n");
            $self.ops.irq_enable_port().write($irq);
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing line_ctrl\n");
            $self.ops.line_ctrl_port().write($line_ctrl); // 8 bits, no parity, one stop bit
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing fifo\n");
            $self.ops.fifo_ctrl_port().write($fifo); // Enable FIFO, clear, 14-byte threshold
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: writing modem\n");
            $self.ops.modem_ctrl_port().write($modem); // IRQs enabled, OUT2
            $crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: init_serial_port end\n");
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
