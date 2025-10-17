// Submodules for petroleum common utilities

// BIOS VGA config (fixed for mode 13h). moved from bios.rs for integration
#[repr(C)]
pub struct VgaFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32, // Bits per pixel
}

pub mod error;
pub mod logging;
pub mod macros;
pub mod syscall;

// Common VGA mode setup helper to avoid code duplication
pub fn setup_vga_mode_common() {
    crate::graphics::setup::setup_vga_mode_13h();
}

pub mod uefi;

// Re-exports to maintain compatibility and new macros
pub use error::*;
pub use syscall::*;
pub use uefi::*;
