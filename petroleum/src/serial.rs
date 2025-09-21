// petroleum/src/serial.rs

use crate::common::{EfiStatus, EfiSimpleTextOutput};
use core::fmt;
use alloc::vec::Vec;
use spin::Mutex;

pub struct UefiWriter {
    con_out: *mut EfiSimpleTextOutput,
}

unsafe impl Sync for UefiWriter {}
unsafe impl Send for UefiWriter {}

impl UefiWriter {
    pub const fn new() -> UefiWriter {
        UefiWriter {
            con_out: core::ptr::null_mut(),
        }
    }

    pub fn init(&mut self, con_out: *mut EfiSimpleTextOutput) {
        self.con_out = con_out;
    }

    pub fn write_string(&mut self, s: &str) -> Result<(), EfiStatus> {
        if self.con_out.is_null() {
            return Ok(());
        }

        let mut s_utf16: Vec<u16> = s.encode_utf16().collect();
        s_utf16.push(0); // Add null terminator

        let status = unsafe { ((*self.con_out).output_string)(self.con_out, s_utf16.as_ptr()) };

        let efi_status = EfiStatus::from(status);
        if efi_status == EfiStatus::Success {
            Ok(())
        } else {
            Err(efi_status)
        }
    }
}

impl fmt::Write for UefiWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s).map_err(|_| fmt::Error)
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

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // By using a Mutex, we ensure safe access to the global writer,
    // even though we expect single-threaded execution in the bootloader.
    // This is safer and more idiomatic than using a `static mut`.
    UEFI_WRITER.lock().write_fmt(args).unwrap();
}
