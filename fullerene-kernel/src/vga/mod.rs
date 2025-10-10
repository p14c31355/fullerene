pub use petroleum::{Color, ColorCode, ScreenChar, TextBufferOperations};

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

// Import for VGA_BUFFER
use spin::{Mutex, Once};

pub mod buffer;

// Global singleton for the VGA buffer writer
pub static VGA_BUFFER: Once<Mutex<VgaBuffer>> = Once::new();

pub use buffer::*;

// Initialize the VGA screen and display welcome message
pub fn init_vga() {
    use spin::Mutex;

    VGA_BUFFER.call_once(|| Mutex::new(VgaBuffer::new()));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen(); // Clear screen on boot
    writer.set_color_code(ColorCode::new(Color::Green, Color::Black));
    writer.write_string("Hello QEMU by FullereneOS!\n");
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
}

#[cfg(test)]
mod tests;
