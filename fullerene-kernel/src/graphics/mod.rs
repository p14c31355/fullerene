// Graphics submodules
pub mod text;
pub mod framebuffer;
pub mod desktop;

// Re-export public API
pub use petroleum::graphics::*;
pub use text::*;
pub use desktop::*;

// Explicitly re-export init function (only available on UEFI)
#[cfg(target_os = "uefi")]
pub use text::init;
