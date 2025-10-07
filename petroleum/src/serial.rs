// petroleum/src/serial.rs

use crate::common::{EfiSimpleTextOutput, EfiStatus};
use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

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
            // Fallback to COM1
            debug_print_str_to_com1(s);
            return Err(efi_status);
        }
        Ok(())
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

// Macro to write to port with wait
macro_rules! write_port {
    ($port:expr, $value:expr) => {
        unsafe {
            while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
            $port.write($value);
        }
    };
}

/// Writes a string to the COM1 serial port.
fn debug_print_str_to_com1(s: &str) {
    for byte in s.bytes() {
        debug_print_byte_to_com1(byte);
    }
}

/// Writes a single byte to the COM1 serial port (0x3F8).
fn debug_print_byte_to_com1(byte: u8) {
    let mut port = Port::new(0x3F8);
    write_port!(port, byte);
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
