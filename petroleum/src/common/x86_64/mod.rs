//! x86-only pieces of the common layer (VGA-era setup and heap summaries).
//!
//! Selected by `common/mod.rs` with one cfg pair, so the rest of `common`
//! stays architecture-neutral.

pub use crate::initializer::InitSequence;
pub use crate::vdso::*;

/// System diagnostics snapshot.
#[derive(Clone, Copy)]
pub struct SystemStats {
    pub total_processes: usize,
    pub active_processes: usize,
    pub memory_used: usize,
    pub uptime_ticks: u64,
}

pub fn setup_vga_mode_common() {
    crate::graphics::setup::setup_vga_mode_13h();
}

pub fn collect_system_stats(
    get_total_processes: fn() -> usize,
    get_active_processes: fn() -> usize,
    get_uptime_ticks: fn() -> u64,
) -> SystemStats {
    SystemStats {
        total_processes: get_total_processes(),
        active_processes: get_active_processes(),
        memory_used: crate::page_table::heap::heap_stats().used,
        uptime_ticks: get_uptime_ticks(),
    }
}
