pub mod color;
pub mod constants;
#[macro_use]
pub mod ports;
pub mod registers;
pub mod setup;
pub mod text;

// VGA constants
pub use constants::*;

// Re-exports for public API
pub use color::*;
pub use ports::{HardwarePorts, PortWriter, VgaPortOps};
// VGA graphics modes
pub use setup::{
    detect_and_init_vga_graphics, detect_cirrus_vga, init_vga_graphics, init_vga_text_mode,
    setup_cirrus_vga_mode,
};

// VGA text operations
pub use text::{Color, ColorCode, ScreenChar, TextBufferOperations};
