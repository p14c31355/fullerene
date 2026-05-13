use core::fmt::Write;
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use petroleum::write_serial_bytes;
use alloc::boxed::Box;

/// Global primary console — stored as a concrete type (no Box, no allocator).
pub static mut PRIMARY_CONSOLE: Option<UefiFramebufferWriter> = None;
/// Global primary renderer — stored as a concrete type (no Box, no allocator).
pub static mut PRIMARY_RENDERER: Option<UefiFramebufferWriter> = None;

/// Sets the primary console for the system.
pub fn set_primary_console(console: Box<dyn Console + Send>) {
    let raw = Box::into_raw(console);
    unsafe {
        PRIMARY_CONSOLE = Some(*Box::from_raw(raw as *mut UefiFramebufferWriter));
    }
}

/// Sets the primary renderer for the system.
pub fn set_primary_renderer(renderer: Box<dyn Renderer + Send>) {
    let raw = Box::into_raw(renderer);
    unsafe {
        PRIMARY_RENDERER = Some(*Box::from_raw(raw as *mut UefiFramebufferWriter));
    }
}

/// Helper to write to the primary console.
pub fn print_to_console(s: &str) {
    unsafe {
        if let Some(ref mut console) = PRIMARY_CONSOLE {
            let _ = console.write_str(s);
        } else {
            write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: PRIMARY_CONSOLE is None!\n");
        }
    }
}

/// Helper to write formatted text to the primary console.
pub fn print_fmt(args: core::fmt::Arguments) {
    unsafe {
        if let Some(ref mut console) = PRIMARY_CONSOLE {
            let _ = core::fmt::write(console, args);
        }
    }
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}

// Re-export desktop drawing
pub use petroleum::graphics::draw_os_desktop;

// Re-export color conversion utility
pub use petroleum::graphics::color::u32_to_rgb888;
