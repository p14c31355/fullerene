#[macro_use]
pub mod ports;
pub mod registers;
pub mod setup;
pub mod text;

// Re-exports for public API
pub use ports::{PortWriter, VgaPortOps, VgaPorts};
pub use setup::{init_vga_graphics, init_vga_text_mode};
pub use text::{Color, ColorCode, ScreenChar, TextBufferOperations};
