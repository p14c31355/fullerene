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

/// System diagnostics structure for monitoring
#[derive(Clone, Copy)]
pub struct SystemStats {
    pub total_processes: usize,
    pub active_processes: usize,
    pub memory_used: usize,
    pub uptime_ticks: u64,
}

/// Collect current system statistics
pub fn collect_system_stats() -> SystemStats {
    // Count total and active processes - would need to call from kernel functions
    // For now, use placeholder or move logic here when integrating
    let total_processes = 0; // Placeholder, actual from fullerene-kernel::process
    let active_processes = 0; // Placeholder
    let (memory_used, _, _) = crate::get_memory_stats!();
    let uptime_ticks = 0; // Placeholder

    SystemStats {
        total_processes,
        active_processes,
        memory_used,
        uptime_ticks,
    }
}

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
