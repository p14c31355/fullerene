// Graphics submodules
#[macro_use]
pub mod desktop;
pub mod framebuffer;
pub mod text;

// Re-export public API
pub use desktop::*;
pub use petroleum::graphics::*;
pub use text::*;

// Re-export color conversion utility for use within the module
pub use petroleum::graphics::color::u32_to_rgb888;
