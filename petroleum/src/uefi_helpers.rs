use super::*;

/// Helper function to convert u32 to string without heap allocation
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
    crate::halt!();
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
        crate::halt!(); // For QEMU debugging
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
        log::info!("{}...\t", core::any::type_name::<T>());
        self();
        log::info!("[ok]");
    }
}

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    log::info!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
}

/// Kernel-side fallback framebuffer detection when config table is not available
/// Uses shared logic from petroleum crate
pub fn kernel_fallback_framebuffer_detection() -> Option<crate::common::FullereneFramebufferConfig>
{
    // Call petroleum's consolidated QEMU framebuffer detection
    crate::detect_qemu_framebuffer(&crate::QEMU_CONFIGS)
}

/// Helper function to initialize graphics with framebuffer configuration
/// Returns true if graphics were successfully initialized and drawn
pub fn initialize_graphics_with_config() -> bool {
    // Check if framebuffer config is available in global storage
    if let Some(config_mutex) = crate::FULLERENE_FRAMEBUFFER_CONFIG.get() {
        if config_mutex.lock().is_some() {
            serial_log!("Graphics configuration found in global storage");
            return true;
        }
    }

    serial_log!("No graphics configuration available");
    false
}

/// Serial logging macro for UEFI helpers
#[macro_export]
macro_rules! serial_log {
    ($($arg:tt)*) => {{
        crate::serial::serial_log(format_args!($($arg)*));
    }};
}
