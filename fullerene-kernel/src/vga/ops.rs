// Import necessary dependencies
use super::{Color, ColorCode, TextBufferOperations, VgaBuffer};
use spin::{Mutex, Once};

// Global singleton for the VGA buffer writer
pub(crate) static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

/// Logs a message to the VGA screen.
pub fn log(msg: &str) {
    if let Some(vga) = VGA_BUFFER.get() {
        let mut writer = vga.lock();
        writer.write_string(msg);
        writer.update_cursor();
    }
}

/// Initializes the VGA screen.
pub fn vga_init() {
    VGA_BUFFER.call_once(|| Mutex::new(VgaBuffer::new()));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen(); // Clear screen on boot
    writer.set_color_code(ColorCode::new(Color::Green, Color::Black));
    writer.write_string("Hello QEMU by FullereneOS!\n");
    writer.set_color_code(ColorCode::new(Color::Green, Color::Black));
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
}
