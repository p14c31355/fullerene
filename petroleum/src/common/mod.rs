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
#[macro_use]
pub mod macros;
pub mod memory;
pub mod syscall;

// Common VGA mode setup helper to avoid code duplication
pub fn setup_vga_mode_common() {
    crate::graphics::setup::setup_vga_mode_13h();
}

pub mod uefi;

// Memory initialization state tracking
static MEMORY_INITIALIZED: spin::Mutex<bool> = spin::Mutex::new(false);

// Function to check if memory has been initialized
pub fn check_memory_initialized() -> bool {
    *MEMORY_INITIALIZED.lock()
}

// Function to mark memory as initialized
pub fn set_memory_initialized(initialized: bool) {
    *MEMORY_INITIALIZED.lock() = initialized;
}

// Re-exports to maintain compatibility
pub use error::*;
pub use macros::*;
pub use memory::*;
pub use syscall::*;
pub use uefi::*;
