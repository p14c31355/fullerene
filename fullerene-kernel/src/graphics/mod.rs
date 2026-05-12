use petroleum::graphics::{Console, Renderer};
use spin::Mutex;
use alloc::boxed::Box;

/// Global primary console for system-wide text output.
pub static PRIMARY_CONSOLE: Mutex<Option<Box<dyn Console + Send>>> = Mutex::new(None);

/// Global primary renderer for system-wide graphics operations.
pub static PRIMARY_RENDERER: Mutex<Option<Box<dyn Renderer + Send>>> = Mutex::new(None);

/// Sets the primary console for the system.
pub fn set_primary_console(console: Box<dyn Console + Send>) {
    *PRIMARY_CONSOLE.lock() = Some(console);
}

/// Sets the primary renderer for the system.
pub fn set_primary_renderer(renderer: Box<dyn Renderer + Send>) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to write to the primary console.
pub fn print_to_console(s: &str) {
    let mut lock = PRIMARY_CONSOLE.lock();
    if let Some(ref mut console) = *lock {
        let _ = console.write_str(s);
    } else {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: PRIMARY_CONSOLE is None!\n");
    }
}

/// Helper to write formatted text to the primary console.
pub fn print_fmt(args: core::fmt::Arguments) {
    if let Some(ref mut console) = *PRIMARY_CONSOLE.lock() {
        let _ = core::fmt::write(console.as_mut(), args);
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
