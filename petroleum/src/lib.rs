#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod apic;
pub mod common;
pub mod graphics;
pub mod page_table;
pub mod serial;
pub use apic::{IoApic, IoApicRedirectionEntry, init_io_apic};
pub use graphics::{
    Color, ColorCode, ScreenChar, TextBufferOperations, VgaPortOps, VgaPorts, init_vga_graphics,
};
pub use graphics::ports::{MsrHelper, PortOperations, PortWriter, RegisterConfig};
pub use serial::{Com1Ports, SerialPort, SerialPortOps};

use core::arch::asm;
use spin::Mutex;

use crate::common::{EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID};
use crate::common::{
    EfiGraphicsOutputProtocol, EfiStatus, EfiSystemTable, FullereneFramebufferConfig,
};

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

/// Helper to initialize UEFI system table
pub fn init_uefi_system_table(system_table: *mut EfiSystemTable) {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table));
}

/// Helper to initialize serial for bootloader
pub unsafe fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    unsafe {
        serial::write_serial_bytes(port, status_port, bytes);
    }
}

/// macro for bootloader serial logging
#[macro_export]
macro_rules! write_serial_bytes {
    ($port:expr, $status:expr, $bytes:expr) => {
        unsafe {
            $crate::write_serial_bytes($port, $status, $bytes);
        }
    };
}

type EfiGraphicsOutputProtocolPtr = EfiGraphicsOutputProtocol;

/// Helper to find GOP and init framebuffer
pub fn init_gop_framebuffer(system_table: &EfiSystemTable) -> Option<FullereneFramebufferConfig> {
    use core::ffi::c_void;
    use core::ptr;

    let bs = unsafe { &*system_table.boot_services };
    let mut gop: *mut EfiGraphicsOutputProtocolPtr = ptr::null_mut();

    let status = (bs.locate_protocol)(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID as *const _ as *const u8,
        ptr::null_mut(),
        &mut gop as *mut _ as *mut *mut c_void,
    );

    if EfiStatus::from(status) != EfiStatus::Success || gop.is_null() {
        serial::_print(format_args!("Failed to locate GOP protocol.\n"));
        return None;
    }

    let gop_ref = unsafe { &*gop };
    if gop_ref.mode.is_null() {
        serial::_print(format_args!("GOP mode pointer is null.\n"));
        return None;
    }

    let mode_ref = unsafe { &*gop_ref.mode };

    // Set GOP to graphics mode (mode 0)
    if mode_ref.mode != 0 {
        let status = (gop_ref.set_mode)(gop, 0);
        if EfiStatus::from(status) != EfiStatus::Success {
            serial::_print(format_args!("Failed to set GOP mode.\n"));
            return None;
        }
    }

    if mode_ref.info.is_null() {
        serial::_print(format_args!("GOP mode info pointer is null.\n"));
        return None;
    }

    let info = unsafe { &*mode_ref.info };

    let fb_addr = mode_ref.frame_buffer_base;
    let fb_size = mode_ref.frame_buffer_size;

    if fb_addr == 0 || fb_size == 0 {
        serial::_print(format_args!("GOP framebuffer info is invalid.\n"));
        return None;
    }

    let config = FullereneFramebufferConfig {
        address: fb_addr as u64,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        pixel_format: info.pixel_format,
        bpp: 32,
        stride: info.pixels_per_scan_line,
    };

    let config_ptr = Box::leak(Box::new(config));

    let status = (bs.install_configuration_table)(
        &FULLERENE_FRAMEBUFFER_CONFIG_TABLE_GUID as *const _ as *const u8,
        config_ptr as *const _ as *mut c_void,
    );

    if EfiStatus::from(status) != EfiStatus::Success {
        let _ = unsafe { Box::from_raw(config_ptr) };
        serial::_print(format_args!(
            "Failed to install framebuffer config table.\n"
        ));
        return None;
    }

    // Clear screen
    unsafe {
        ptr::write_bytes(fb_addr as *mut u8, 0x00, fb_size as usize);
    }

    Some(*config_ptr)
}

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

    // Also output to VGA buffer if available - heapless formatting
    #[cfg(feature = "vga_panic")]
    {
        // Import VGA module here to avoid dependency issues
        extern crate vga_buffer;
        use vga_buffer::{BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, Writer};

        let mut writer = Writer {
            column_position: 0,
            color_code: ColorCode::new(Color::Red, Color::Black),
            buffer: unsafe { &mut *(0xb8000 as *mut vga_buffer::Buffer) },
        };

        // Write "PANIC: " header
        let header = b"PANIC: ";
        for &byte in header {
            writer.write_byte(byte);
        }

        // Write location if available
        if let Some(loc) = info.location() {
            let loc_str = loc.file();
            for byte in loc_str.bytes() {
                if byte == b'\n' {
                    writer.new_line();
                } else if byte.is_ascii_graphic()
                    || byte == b' '
                    || byte == b'.'
                    || byte == b'/'
                    || byte == b'\\'
                {
                    writer.write_byte(byte);
                }
            }
            let colons = b":";
            for &byte in colons {
                writer.write_byte(byte);
            }
            let mut line_buf = [0u8; 10];
            let line_str = u32_to_str_heapless(loc.line(), &mut line_buf);
            for byte in line_str.bytes() {
                writer.write_byte(byte);
            }
            for &byte in colons {
                writer.write_byte(byte);
            }
            let mut col_buf = [0u8; 10];
            let col_str = u32_to_str_heapless(loc.column(), &mut col_buf);
            for byte in col_str.bytes() {
                writer.write_byte(byte);
            }
            writer.new_line();
        }

        // Write message
        if let Some(msg) = info.message().as_str() {
            for byte in msg.bytes() {
                if byte == b'\n' {
                    writer.new_line();
                } else if byte.is_ascii_graphic() || byte == b' ' {
                    writer.write_byte(byte);
                }
            }
        } else {
            let msg_failed = b"(message formatting failed)";
            for &byte in msg_failed {
                writer.write_byte(byte);
            }
        }
        writer.new_line();
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
    unsafe {
        core::ptr::copy(
            fb_ptr.add(shift_bytes as usize),
            fb_ptr,
            (total_bytes - shift_bytes) as usize,
        );
    }
    // Clear last 8 lines
    let clear_offset = (height - 8) as usize * bytes_per_line as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let clear_count = 8 * stride as usize;
    unsafe {
        core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color);
    }
}

/// Generic function to clear a raw pixel buffer
/// Reduces code duplication in buffer initialization
pub unsafe fn clear_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let fb_ptr = address as *mut T;
    let count = (stride * height) as usize;
    unsafe {
        core::slice::from_raw_parts_mut(fb_ptr, count).fill(bg_color);
    }
}

use alloc::boxed::Box;
