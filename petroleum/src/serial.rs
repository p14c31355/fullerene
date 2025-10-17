pub unsafe fn write_serial_bytes(port_addr: u16, status_port_addr: u16, bytes: &[u8]) {
    use x86_64::instructions::port::Port;
    let mut port = Port::<u8>::new(port_addr);
    let mut status_port = Port::<u8>::new(status_port_addr);
    for &byte in bytes {
        unsafe {
            while (status_port.read() & 0x20) == 0 {}
            port.write(byte);
        }
    }
}

use crate::common::{EfiSimpleTextOutput, EfiStatus};
use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::port::Port;

// Generic serial port implementation that works with different bases
pub trait SerialPortOps {
    fn data_port(&self) -> Port<u8>;
    fn irq_enable_port(&self) -> Port<u8>;
    fn fifo_ctrl_port(&self) -> Port<u8>;
    fn line_ctrl_port(&self) -> Port<u8>;
    fn modem_ctrl_port(&self) -> Port<u8>;
    fn line_status_port(&self) -> Port<u8>;
}

/// Represents a serial port for communication.
pub struct SerialPort<S: SerialPortOps> {
    ops: S,
}

impl<S: SerialPortOps> SerialPort<S> {
    /// Creates a new instance of the SerialPort.
    pub const fn new(ops: S) -> SerialPort<S> {
        SerialPort { ops }
    }

    /// Initializes the serial port.
    pub fn init(&mut self) {
        unsafe {
            self.ops.line_ctrl_port().write(0x80); // Enable DLAB
            self.ops.data_port().write(0x03); // Baud rate divisor low byte (38400 bps)
            self.ops.irq_enable_port().write(0x00);
            self.ops.line_ctrl_port().write(0x03); // 8 bits, no parity, one stop bit
            self.ops.fifo_ctrl_port().write(0xC7); // Enable FIFO, clear, 14-byte threshold
            self.ops.modem_ctrl_port().write(0x0B); // IRQs enabled, OUT2
        }
    }

    /// Writes a single byte to the serial port.
    pub fn write_byte(&mut self, byte: u8) {
        unsafe {
            while (self.ops.line_status_port().read() & 0x20) == 0 {}
            self.ops.data_port().write(byte);
        }
    }

    /// Writes a string to the serial port.
    pub fn write_string(&mut self, s: &str) {
        for b in s.bytes() {
            self.write_byte(b);
        }
    }
}

/// COM1 implementation
pub struct Com1Ports;

impl SerialPortOps for Com1Ports {
    fn data_port(&self) -> Port<u8> {
        Port::new(0x3F8)
    }
    fn irq_enable_port(&self) -> Port<u8> {
        Port::new(0x3F9)
    }
    fn fifo_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FA)
    }
    fn line_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FB)
    }
    fn modem_ctrl_port(&self) -> Port<u8> {
        Port::new(0x3FC)
    }
    fn line_status_port(&self) -> Port<u8> {
        Port::new(0x3FD)
    }
}

impl<S: SerialPortOps> fmt::Write for SerialPort<S> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// Provides a global singleton for the serial port
// Provides a global singleton for the serial port
pub static SERIAL_PORT_WRITER: Mutex<SerialPort<Com1Ports>> =
    Mutex::new(SerialPort::new(Com1Ports));

pub struct UefiWriter {
    con_out: *mut EfiSimpleTextOutput,
}

unsafe impl Sync for UefiWriter {}
unsafe impl Send for UefiWriter {}

impl Default for UefiWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl UefiWriter {
    pub const fn new() -> UefiWriter {
        UefiWriter {
            con_out: core::ptr::null_mut(),
        }
    }

    pub fn init(&mut self, con_out: *mut EfiSimpleTextOutput) {
        self.con_out = con_out;
    }

    pub fn write_string_heapless(&mut self, s: &str) -> Result<(), EfiStatus> {
        if self.con_out.is_null() {
            return Ok(());
        }

        let mut utf16_buf = [0u16; 512]; // 文字列 + null, 余裕持って
        let mut idx = 0;
        for c in s.encode_utf16() {
            if idx < utf16_buf.len() - 1 {
                utf16_buf[idx] = c;
                idx += 1;
            } else {
                break;
            }
        }
        utf16_buf[idx] = 0; // null terminate

        let status = unsafe { ((*self.con_out).output_string)(self.con_out, utf16_buf.as_ptr()) };
        let efi_status = EfiStatus::from(status);
        if efi_status != EfiStatus::Success {
            // Fallback to COM1 using the initialized global writer
            SERIAL_PORT_WRITER.lock().write_string(s);
            return Err(efi_status);
        }
        Ok(())
    }

    pub fn write_string(&mut self, s: &str) {
        self.write_string_heapless(s).ok();
    }
}

impl fmt::Write for UefiWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string_heapless(s).map_err(|_| fmt::Error)
    }
}

// Global writer instance
pub static UEFI_WRITER: Mutex<UefiWriter> = Mutex::new(UefiWriter::new());

/// Writes a string to the COM1 serial port.
/// This is a very early debug function for use beforeUEFI writers are available.
pub fn debug_print_str_to_com1(s: &str) {
    unsafe {
        write_serial_bytes(0x3F8, 0x3FD, s.as_bytes());
    }
}

pub fn serial_log(args: core::fmt::Arguments) {
    _print(args);
}

/// Writes a single byte to the COM1 serial port (0x3F8).
pub fn debug_print_byte_to_com1(byte: u8) {
    SERIAL_PORT_WRITER.lock().write_byte(byte);
}

/// Prints a usize as hex to COM1 (early debug, no alloc).
pub fn debug_print_hex(value: usize) {
    let mut writer = SERIAL_PORT_WRITER.lock();
    let _ = format_hex(&mut *writer, value);
}

/// Formats a usize as hex to the given writer without allocation.
pub fn format_hex(writer: &mut impl core::fmt::Write, value: usize) -> core::fmt::Result {
    write!(writer, "0x")?;
    if value == 0 {
        return write!(writer, "0");
    }
    let mut temp = value;
    let mut digits = [0u8; 16];
    let mut i = 0;
    while temp > 0 && i < 16 {
        let digit = (temp % 16) as u8;
        digits[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        temp /= 16;
        i += 1;
    }
    // Reverse the digits slice in place
    digits[0..i].reverse();
    // Write the full hex string in one call
    writer.write_str(core::str::from_utf8(&digits[0..i]).unwrap())
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    (&mut *SERIAL_PORT_WRITER.lock()).write_fmt(args).ok();
    (&mut *UEFI_WRITER.lock()).write_fmt(args).ok();
}

/// Macro to reduce repetitive debug serial output
#[macro_export]
macro_rules! debug_log {
    ($msg:expr) => {{
        $crate::serial::debug_print_str_to_com1($msg);
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        $crate::serial::serial_log(format_args!($fmt, $($arg)*));
    }};
}

/// Initializes the global serial port writer.
pub fn serial_init() {
    SERIAL_PORT_WRITER.lock().init();
}

/// Formats a u64 value as hex to a byte buffer with limited digits.
/// Returns the number of bytes written.
pub fn format_hex_to_buffer(value: u64, buf: &mut [u8], max_digits: usize) -> usize {
    let mut temp = value;
    let mut i = 0;
    let mut digit_buf = [0u8; 16];
    if temp == 0 {
        buf[0] = b'0';
        return 1;
    }
    while temp > 0 && i < max_digits && i < 16 {
        let digit = (temp % 16) as u8;
        digit_buf[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        temp /= 16;
        i += 1;
    }
    // Reverse
    for j in 0..i {
        buf[j] = digit_buf[i - 1 - j];
    }
    i
}

/// Formats a usize value as decimal to a byte buffer.
/// Returns the number of bytes written.
pub fn format_dec_to_buffer(value: usize, buf: &mut [u8]) -> usize {
    let mut temp = value;
    let mut i = 0;
    let mut digit_buf = [0u8; 16];
    if temp == 0 {
        buf[0] = b'0';
        return 1;
    }
    while temp > 0 && i < 16 {
        let digit = (temp % 10) as u8;
        digit_buf[i] = b'0' + digit;
        temp /= 10;
        i += 1;
    }
    // Reverse
    for j in 0..i {
        buf[j] = digit_buf[i - 1 - j];
    }
    i
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::fmt;

    #[test]
    fn test_format_hex_output() {
        struct TestWriter {
            buf: Vec<u8>,
        }

        impl TestWriter {
            fn new() -> Self {
                TestWriter { buf: Vec::new() }
            }
        }

        impl fmt::Write for TestWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.buf.extend_from_slice(s.as_bytes());
                Ok(())
            }
        }

        let mut writer = TestWriter::new();
        super::format_hex(&mut writer, 0).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0x0");

        let mut writer = TestWriter::new();
        super::format_hex(&mut writer, 255).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0xff");

        let mut writer = TestWriter::new();
        super::format_hex(&mut writer, 4096).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0x1000");
    }

    #[test]
    fn test_uefi_writer_new() {
        let writer = super::UefiWriter::new();
        assert!(writer.con_out.is_null());
    }
}
