#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod common;
pub mod graphics;
pub mod page_table;
pub mod serial;
pub use graphics::{Color, ColorCode, ScreenChar, TextBufferOperations};
pub use serial::{Com1Ops, SerialPort, SerialPortOps};

use core::arch::asm;
use spin::Mutex;

use crate::common::EfiSystemTable;

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

// Helper function to convert u32 to string without heap allocation
pub fn u32_to_str_heapless(n: u32, buffer: &mut [u8]) -> &str {
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

/// Panic handler implementation that can be used by binaries
pub fn handle_panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(st_ptr) = UEFI_SYSTEM_TABLE.lock().as_ref() {
        let st_ref = unsafe { &*st_ptr.0 };
        crate::serial::UEFI_WRITER.lock().init(st_ref.con_out);

        // Use write_string_heapless for panic messages to avoid heap allocation initially
        let mut writer = crate::serial::UEFI_WRITER.lock();
        let _ = writer.write_string_heapless("PANIC!\n");

        if let Some(loc) = info.location() {
            let mut line_buf = [0u8; 10];
            let mut col_buf = [0u8; 10];
            let _ = writer.write_string_heapless("Location: ");
            let _ = writer.write_string_heapless(loc.file());
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.line(), &mut line_buf));
            let _ = writer.write_string_heapless(":");
            let _ = writer.write_string_heapless(u32_to_str_heapless(loc.column(), &mut col_buf));
            let _ = writer.write_string_heapless("\n");
        }

        let _ = writer.write_string_heapless("Message: ");
        // Try to write the message as a string slice if possible
        if let Some(msg) = info.message().as_str() {
            let _ = writer.write_string_heapless(msg);
        } else {
            let _ = writer.write_string_heapless("(message formatting failed)");
        }
        let _ = writer.write_string_heapless("\n");
    }

    // Also output to VGA buffer if available
    #[cfg(feature = "vga_panic")]
    {
        // Import VGA module here to avoid dependency issues
        extern crate vga_buffer;
        use vga_buffer::{Writer, ColorCode, Color, BUFFER_HEIGHT, BUFFER_WIDTH};

        let mut writer = Writer {
            column_position: 0,
            color_code: ColorCode::new(Color::Red, Color::Black),
            buffer: unsafe { &mut *(0xb8000 as *mut vga_buffer::Buffer) },
        };

        for (i, line) in format_args!("PANIC: {}", info).to_string().lines().enumerate() {
            if i >= BUFFER_HEIGHT {
                break;
            }
            for (j, byte) in line.bytes().enumerate() {
                if j >= BUFFER_WIDTH {
                    break;
                }
                writer.write_byte(byte);
            }
            writer.new_line();
        }
    }

    // For QEMU debugging, halt the CPU
    unsafe {
        asm!("hlt");
    }
    loop {} // Panics must diverge
}

/// Alloc error handler required when using `alloc` in no_std.
#[cfg(all(panic = "unwind", not(feature = "std"), not(test)))]
#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
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

/// Test harness for no_std environment
#[cfg(test)]
pub trait Testable {
    fn run(&self);
}

#[cfg(test)]
impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        println!("{}...\t", core::any::type_name::<T>());
        self();
        println!("[ok]");
    }
}

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
}

/// Generic function to safely and efficiently scroll a raw pixel buffer up
/// Reduces code duplication in buffer management
pub unsafe fn scroll_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let bytes_per_line = stride * bytes_per_pixel;
    let shift_bytes = 8u64 * bytes_per_line as u64;
    let fb_ptr = address as *mut u8;
    let total_bytes = height as u64 * bytes_per_line as u64;
    core::ptr::copy(
        fb_ptr.add(shift_bytes as usize),
        fb_ptr,
        (total_bytes - shift_bytes) as usize,
    );
    // Clear last 8 lines
    let clear_offset = (height - 8) as usize * bytes_per_line as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let clear_count = 8 * stride as usize;
    core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color);
}

/// Generic function to clear a raw pixel buffer
/// Reduces code duplication in buffer initialization
pub unsafe fn clear_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let fb_ptr = address as *mut T;
    let count = (stride * height) as usize;
    core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color);
}
