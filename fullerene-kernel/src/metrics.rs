//! Runtime performance and resource telemetry.

use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static BOOT_START_TSC: AtomicU64 = AtomicU64::new(0);
static BOOT_TIME_US: AtomicU64 = AtomicU64::new(0);
static FRAME_LAST_US: AtomicU64 = AtomicU64::new(0);
static FRAME_MAX_US: AtomicU64 = AtomicU64::new(0);
static HEAP_HIGH_WATER_BYTES: AtomicUsize = AtomicUsize::new(0);

fn update_max_u64(target: &AtomicU64, candidate: u64) {
    let mut observed = target.load(Ordering::Relaxed);
    while candidate > observed {
        match target.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => observed = actual,
        }
    }
}

fn update_max_usize(target: &AtomicUsize, candidate: usize) {
    let mut observed = target.load(Ordering::Relaxed);
    while candidate > observed {
        match target.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => observed = actual,
        }
    }
}

fn ticks_to_us(ticks: u64) -> u64 {
    let ticks_per_ms = solvent::get_tsc_per_ms().max(1);
    ((ticks as u128 * 1000) / ticks_per_ms as u128) as u64
}

/// Start boot timing at the first common kernel entry.
pub fn mark_boot_start() {
    let now = unsafe { core::arch::x86_64::_rdtsc() };
    let _ = BOOT_START_TSC.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
}

/// Freeze the boot duration once desktop services are ready.
pub fn mark_boot_ready() {
    let start = BOOT_START_TSC.load(Ordering::Relaxed);
    if start != 0 {
        let elapsed = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        BOOT_TIME_US.store(ticks_to_us(elapsed), Ordering::Relaxed);
    }
    sample_heap();
}

/// Record one complete compositor/present pass.
pub fn record_frame_ticks(ticks: u64) {
    let micros = ticks_to_us(ticks);
    FRAME_LAST_US.store(micros, Ordering::Relaxed);
    update_max_u64(&FRAME_MAX_US, micros);
    sample_heap();
}

/// Sample allocator usage and update its high-water mark.
pub fn sample_heap() {
    let used = petroleum::page_table::heap::heap_stats().used;
    update_max_usize(&HEAP_HIGH_WATER_BYTES, used);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub boot_time_us: u64,
    pub frame_last_us: u64,
    pub frame_max_us: u64,
    pub heap_current_bytes: usize,
    pub heap_high_water_bytes: usize,
    pub dma_current_bytes: usize,
    pub dma_high_water_bytes: usize,
}

pub fn snapshot() -> Snapshot {
    let heap = petroleum::page_table::heap::heap_stats();
    update_max_usize(&HEAP_HIGH_WATER_BYTES, heap.used);
    let (dma_current, dma_high_water) = nitrogen::metrics::dma_usage();
    Snapshot {
        boot_time_us: BOOT_TIME_US.load(Ordering::Relaxed),
        frame_last_us: FRAME_LAST_US.load(Ordering::Relaxed),
        frame_max_us: FRAME_MAX_US.load(Ordering::Relaxed),
        heap_current_bytes: heap.used,
        heap_high_water_bytes: HEAP_HIGH_WATER_BYTES.load(Ordering::Relaxed),
        dma_current_bytes: dma_current,
        dma_high_water_bytes: dma_high_water,
    }
}

pub fn format_snapshot() -> String {
    let metrics = snapshot();
    let mut out = String::with_capacity(256);
    let _ = writeln!(out, "Boot time:       {} us", metrics.boot_time_us);
    let _ = writeln!(
        out,
        "Frame time:      {} us (max {} us)",
        metrics.frame_last_us, metrics.frame_max_us
    );
    let _ = writeln!(
        out,
        "Heap usage:      {} KiB (high-water {} KiB)",
        metrics.heap_current_bytes / 1024,
        metrics.heap_high_water_bytes / 1024
    );
    let _ = writeln!(
        out,
        "DMA usage:       {} KiB (high-water {} KiB)",
        metrics.dma_current_bytes / 1024,
        metrics.dma_high_water_bytes / 1024
    );
    out
}
