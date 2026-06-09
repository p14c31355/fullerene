//! Common utilities shared across petroleum submodules.

pub mod error;
pub mod logging;
#[macro_use]
pub mod macros;
pub mod memory;
pub mod syscall;
pub mod uefi;
pub mod utils;

#[repr(C)]
pub struct VgaFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

pub fn setup_vga_mode_common() {
    crate::graphics::setup::setup_vga_mode_13h();
}

/// System diagnostics snapshot.
#[derive(Clone, Copy)]
pub struct SystemStats {
    pub total_processes: usize,
    pub active_processes: usize,
    pub memory_used: usize,
    pub uptime_ticks: u64,
}

pub fn collect_system_stats(
    get_total_processes: fn() -> usize,
    get_active_processes: fn() -> usize,
    get_uptime_ticks: fn() -> u64,
) -> SystemStats {
    SystemStats {
        total_processes: get_total_processes(),
        active_processes: get_active_processes(),
        memory_used: crate::page_table::ALLOCATOR.lock().used(),
        uptime_ticks: get_uptime_ticks(),
    }
}

use core::sync::atomic::{AtomicBool, Ordering};

static MEMORY_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn check_memory_initialized() -> bool {
    MEMORY_INITIALIZED.load(Ordering::SeqCst)
}

pub fn set_memory_initialized(initialized: bool) {
    MEMORY_INITIALIZED.store(initialized, Ordering::SeqCst);
}

pub use crate::initializer::InitSequence;
pub use error::*;
pub use macros::*;
pub use memory::*;
pub use syscall::*;
pub use uefi::*;
pub use utils::*;
