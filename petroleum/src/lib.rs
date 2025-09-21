#![no_std]
#![feature(alloc_error_handler)]
#![feature(never_type)]

extern crate alloc;

pub mod common;
pub mod serial;

use core::alloc::Layout;
use spin::Mutex;

use crate::common::EfiSystemTable;

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    panic!("Allocation error");
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    // Print the panic message using the refactored serial module.
    if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        // Initialize the writer to ensure panic messages can be printed.

        crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);

        if let Some(location) = info.location() {
            // Assuming info.message() returns Option<PanicMessage<'a>>
            // and PanicMessage<'a> has a field 'args' of type &fmt::Arguments
            println!(
                "Panic at {}:{}:{} - {}",
                location.file(),
                location.line(),
                location.column(),
                info.message() // Directly use info.message()
            );
        } else {
            // Assuming info.message() returns Option<PanicMessage<'a>>
            // and PanicMessage<'a> has a field 'args' of type &fmt::Arguments
            println!("Panic: {}", info.message()); // Directly use info.message()
        }
    }
    loop {} // Panics must diverge
}
