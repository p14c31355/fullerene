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
pub mod utils;

/// System diagnostics structure for monitoring
#[derive(Clone, Copy)]
pub struct SystemStats {
    pub total_processes: usize,
    pub active_processes: usize,
    pub memory_used: usize,
    pub uptime_ticks: u64,
}

/// Collect system statistics using provided getters
/// This allows petroleum to define the collection logic while kernel provides the data
pub fn collect_system_stats(
    get_total_processes: fn() -> usize,
    get_active_processes: fn() -> usize,
    get_uptime_ticks: fn() -> u64,
) -> SystemStats {
    let total_processes = get_total_processes();
    let active_processes = get_active_processes();
    let (memory_used, _, _) = (0, 0, 0); // TODO: implement get_memory_stats
    let uptime_ticks = get_uptime_ticks();
    SystemStats {
        total_processes,
        active_processes,
        memory_used,
        uptime_ticks,
    }
}

use core::sync::atomic::{AtomicBool, Ordering};

// Memory initialization state tracking
static MEMORY_INITIALIZED: AtomicBool = AtomicBool::new(false);

// Function to check if memory has been initialized
pub fn check_memory_initialized() -> bool {
    MEMORY_INITIALIZED.load(Ordering::SeqCst)
}

// Function to mark memory as initialized
pub fn set_memory_initialized(initialized: bool) {
    MEMORY_INITIALIZED.store(initialized, Ordering::SeqCst);
}

// Re-exports to maintain compatibility
pub use crate::initializer::InitSequence;
pub use error::*;
pub use macros::*;
pub use memory::*;
pub use syscall::*;
pub use uefi::*;
pub use utils::*;
