// bellows/src/loader/serial.rs

use alloc::vec::Vec;
use core::fmt;
use spin::Mutex;

/// Minimal UEFI Simple Text Output Protocol
#[repr(C)]
pub struct EfiSimpleTextOutput {
    _pad: [usize; 2],
    /// output_string(This, *mut u16) -> EFI_STATUS
    pub output_string: extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> usize,
}

pub struct UefiWriter {
    con_out: *mut EfiSimpleTextOutput,
}

unsafe impl Sync for UefiWriter {}

impl UefiWriter {
    pub const fn new() -> UefiWriter {
        UefiWriter {
            con_out: core::ptr::null_mut(),
        }
    }

    pub fn init(&mut self, con_out: *mut EfiSimpleTextOutput) {
        self.con_out = con_out;
    }

        pub fn write_string(&mut self, s: &str) -> Result<(), crate::uefi::EfiStatus> {
        if self.con_out.is_null() {
            return Ok(());
        }

        let mut s_utf16: Vec<u16> = s.encode_utf16().collect();
        s_utf16.push(0); // Add null terminator

        let status = unsafe {
            ((*self.con_out).output_string)(self.con_out, s_utf16.as_ptr())
        };

        let efi_status = crate::uefi::EfiStatus::from(status);
        if efi_status == crate::uefi::EfiStatus::Success {
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
pub static UEFI_WRITER: spin::Mutex<UefiWriter> = spin::Mutex::new(UefiWriter::new());

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::loader::serial::_print(format_args!($($arg)*)));
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
