//! Hardware operation macros for Fullerene OS

#[macro_export]
macro_rules! volatile_write {
    ($ptr:expr, $val:expr) => {{
        let ptr = $ptr;
        let value = $val;
        unsafe { core::ptr::write_volatile(ptr, value) }
    }};
}
#[macro_export]
macro_rules! volatile_ops {
    (read, $addr:expr, $ty:ty) => {{
        let address = $addr;
        unsafe { core::ptr::read_volatile(address as *const $ty) }
    }};
    (write, $addr:expr, $val:expr) => {{
        let address = $addr;
        let value = $val;
        unsafe { core::ptr::write_volatile(address as *mut _, value) }
    }};
}

/// Safe volatile read with PCI master-abort detection.
///
/// Returns `None` if the read returns `0xFFFF_FFFF` (PCI master abort).
/// Use this for ALL PCIe MMIO register reads to detect unresponsive devices.
///
/// The `$health` parameter is an `Option<&PciHealth>`. When `Some`, a
/// `is_device_present()` check is performed before the read.
#[macro_export]
macro_rules! volatile_read_safe {
    ($addr:expr, $ty:ty) => {{
        let address = $addr as *const $ty;
        let val = unsafe { core::ptr::read_volatile(address) };
        if val == <$ty>::MAX {
            None
        } else {
            Some(val)
        }
    }};
    ($addr:expr, $ty:ty, $health:expr) => {{
        let address = $addr as *const $ty;
        let result = (|| -> Option<$ty> {
            if let Some(h) = $health.as_ref() {
                if !h.is_device_present() {
                    return None;
                }
            }
            let val = unsafe { core::ptr::read_volatile(address) };
            if val == <$ty>::MAX {
                None
            } else {
                Some(val)
            }
        })();
        result
    }};
}

#[macro_export]
macro_rules! init_serial_port {
    ($line_ctrl_port:expr, $data_port:expr, $irq_enable_port:expr, $fifo_ctrl_port:expr, $modem_ctrl_port:expr, $dlab:expr, $divisor_low:expr, $irq:expr, $line_ctrl:expr, $fifo:expr, $modem:expr) => {{
        let mut line_ctrl_port = $line_ctrl_port;
        let mut data_port = $data_port;
        let mut irq_enable_port = $irq_enable_port;
        let mut fifo_ctrl_port = $fifo_ctrl_port;
        let mut modem_ctrl_port = $modem_ctrl_port;
        let (dlab, divisor_low, irq, line_ctrl, fifo, modem) =
            ($dlab, $divisor_low, $irq, $line_ctrl, $fifo, $modem);
        unsafe {
            $crate::write_serial_bytes(
                $crate::io::HardwarePorts::SERIAL_DATA_PORT,
                $crate::io::HardwarePorts::SERIAL_LINE_STATUS_PORT,
                b"DEBUG: init_serial_port start\n",
            );

            line_ctrl_port.write(dlab);
            data_port.write(divisor_low);
            irq_enable_port.write(irq);
            line_ctrl_port.write(line_ctrl);
            fifo_ctrl_port.write(fifo);
            modem_ctrl_port.write(modem);

            $crate::write_serial_bytes(
                $crate::io::HardwarePorts::SERIAL_DATA_PORT,
                $crate::io::HardwarePorts::SERIAL_LINE_STATUS_PORT,
                b"DEBUG: init_serial_port end\n",
            );
        }
    }};
}
