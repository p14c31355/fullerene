use crate::common::{EfiSimpleTextOutput, EfiStatus};
use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Low-level serial port writer for COM1 (0x3F8).
pub struct SerialPortWriter {
    data: Port<u8>,
    irq_enable: Port<u8>,
    fifo_ctrl: Port<u8>,
    line_ctrl: Port<u8>,
    modem_ctrl: Port<u8>,
    line_status: Port<u8>,
}

impl SerialPortWriter {
    /// Creates a new instance of the SerialPortWriter.
    pub const fn new() -> SerialPortWriter {
        SerialPortWriter {
            data: Port::new(0x3F8),
            irq_enable: Port::new(0x3F9),
            fifo_ctrl: Port::new(0x3FA),
            line_ctrl: Port::new(0x3FB),
            modem_ctrl: Port::new(0x3FC),
            line_status: Port::new(0x3FD),
        }
    }

    /// Initializes the serial port.
    pub fn init(&mut self) {
        unsafe {
            self.line_ctrl.write(0x80); // Enable DLAB
            self.data.write(0x03); // Baud rate divisor low byte (38400 bps)
            self.irq_enable.write(0x00);
            self.line_ctrl.write(0x03); // 8 bits, no parity, one stop bit
            self.fifo_ctrl.write(0xC7); // Enable FIFO, clear, 14-byte threshold
            self.modem_ctrl.write(0x0B); // IRQs enabled, OUT2
        }
    }

    /// Writes a single byte to the serial port.
    pub fn write_byte(&mut self, byte: u8) {
        unsafe {
            while (self.line_status.read() & 0x20) == 0 {}
            self.data.write(byte);
        }
    }

    /// Writes a string to the serial port.
    pub fn write_string(&mut self, s: &str) {
        for b in s.bytes() {
            self.write_byte(b);
        }
    }
}

// Provides a fmt::Write implementation for SerialPortWriter
impl fmt::Write for SerialPortWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// Provides a global singleton for the serial port
pub static SERIAL_PORT_WRITER: Mutex<SerialPortWriter> = Mutex::new(SerialPortWriter::new());

/// Initializes the global serial port writer.
pub fn serial_init() {
    SERIAL_PORT_WRITER.lock().init();
}

/// Logs a string to the serial port.
pub fn serial_log(s: &str) {
    let mut writer = SERIAL_PORT_WRITER.lock();
    writer.write_string(s);
    writer.write_string("\n");
}

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

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Writes a string to the COM1 serial port.
/// This is a very early debug function for use beforeUEFI writers are available.
pub fn debug_print_str_to_com1(s: &str) {
    SERIAL_PORT_WRITER.lock().write_string(s);
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
fn format_hex(writer: &mut impl core::fmt::Write, value: usize) -> core::fmt::Result {
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
    for j in (0..i).rev() {
        writer.write_str(core::str::from_utf8(&[digits[j]]).unwrap())?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // By using a Mutex, we ensure safe access to the global writer,
    // even though we expect single-threaded execution in the bootloader.
    // This is safer and more idiomatic than using a `static mut`.
    UEFI_WRITER
        .lock()
        .write_fmt(args)
        .expect("Serial write failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_print_hex() {
        // Test that debug_print_hex doesn't panic (output goes to serial)
        debug_print_hex(0);
        debug_print_hex(255);
        debug_print_hex(4096);
    }

    #[test]
    fn test_format_hex_output() {
        struct TestWriter {
            buf: alloc::vec::Vec<u8>,
        }

        impl TestWriter {
            fn new() -> Self {
                TestWriter {
                    buf: alloc::vec::Vec::new(),
                }
            }
        }

        impl fmt::Write for TestWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.buf.extend_from_slice(s.as_bytes());
                Ok(())
            }
        }

        let mut writer = TestWriter::new();
        format_hex(&mut writer, 0).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0x0");

        let mut writer = TestWriter::new();
        format_hex(&mut writer, 255).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0xff");

        let mut writer = TestWriter::new();
        format_hex(&mut writer, 4096).unwrap();
        assert_eq!(core::str::from_utf8(&writer.buf).unwrap(), "0x1000");
    }

    #[test]
    fn test_uefi_writer_new() {
        let writer = UefiWriter::new();
        assert!(writer.con_out.is_null());
    }
}
