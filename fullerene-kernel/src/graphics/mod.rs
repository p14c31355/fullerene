// Graphics submodules
pub mod desktop;
pub mod framebuffer;
pub mod text;
pub mod vga_device;

// Re-export public API
pub use desktop::*;
pub use petroleum::graphics::*;
pub use text::*;
pub use vga_device::*;

// Re-export color conversion utility for use within the module
pub use petroleum::graphics::color::u32_to_rgb888;
