use petroleum::graphics::*;

// Re-export specific functions from petroleum to maintain consistency
pub use petroleum::graphics::draw_os_desktop;

// Keep local text module for configuration-specific init
pub mod text;
pub use text::*;

// Re-export color conversion utility for use within the module
pub use petroleum::graphics::color::u32_to_rgb888;
