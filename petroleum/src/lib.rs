#![no_std]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

pub mod common;
pub mod serial;

use core::{alloc::Layout, arch::asm, fmt::Write};
use spin::Mutex;

use crate::common::EfiSystemTable;

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

// Helper function to convert u32 to string without heap allocation
fn u32_to_str_heapless(n: u32, buffer: &mut [u8]) -> &str {
    let mut i = buffer.len();
    let mut n = n;
    if n == 0 {
        buffer[i - 1] = b'0';
        return core::str::from_utf8(&buffer[i - 1..i]).unwrap_or("ERR");
    }
    loop {
        i -= 1;
        buffer[i] = (n % 10) as u8 + b'0';
        n /= 10;
        if n == 0 || i == 0 {
            break;
        }
    }
    core::str::from_utf8(&buffer[i..]).unwrap_or("ERR")
}

/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    // Avoid recursive panics by directly looping
    loop {
        // Optionally, try to print a message using the heap-less writer if possible
        if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
            let st_ref = unsafe { &*st_ptr.0 };
            crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);
            crate::serial::UEFI_WRITER
                .lock()
                .write_string_heapless("Allocation error!\n")
                .ok();
        }
        unsafe {
            asm!("hlt"); // For QEMU debugging
        }
    }
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    // Print the panic message using the refactored serial module.
    if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);

        // Use write_string_heapless for panic messages to avoid heap allocation
        let mut writer = crate::serial::UEFI_WRITER.lock();
        let mut line_buf = [0u8; 10]; // Buffer for line number
        let mut col_buf = [0u8; 10]; // Buffer for column number

        if let Some(loc) = info.location() {
            let _ = writer.write_string_heapless("Panic at ");
            let _ = writer.write_string_heapless(loc.file());
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.line(), &mut line_buf));
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.column(), &mut col_buf));
            let _ = writer.write_string_heapless("\n");
        }

        let _ = writer.write_string_heapless("Panic occurred!\n");
        if let Some(msg) = info.message() {
            let _ = writer.write_string_heapless("Message: ");
            let _ = writer.write_fmt(*msg);
            let _ = writer.write_string_heapless("\n");
        } else {
            let _ = writer.write_string_heapless("(no message)\n");
        }
    }
    // For QEMU debugging, halt the CPU
    unsafe {
        core::arch::asm!("hlt");
    }
    loop {} // Panics must diverge
}
