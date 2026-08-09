//! Lock-free driver resource counters.

use core::sync::atomic::{AtomicUsize, Ordering};

struct DmaCounters {
    current_bytes: AtomicUsize,
    high_water_bytes: AtomicUsize,
}

impl DmaCounters {
    const fn new() -> Self {
        Self {
            current_bytes: AtomicUsize::new(0),
            high_water_bytes: AtomicUsize::new(0),
        }
    }

    fn update_high_water(&self, candidate: usize) {
        let mut observed = self.high_water_bytes.load(Ordering::Relaxed);
        while candidate > observed {
            match self.high_water_bytes.compare_exchange_weak(
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

    fn allocated(&self, bytes: usize) {
        let current = self
            .current_bytes
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        self.update_high_water(current);
    }

    fn released(&self, bytes: usize) {
        let _ = self
            .current_bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(bytes))
            });
    }

    fn usage(&self) -> (usize, usize) {
        (
            self.current_bytes.load(Ordering::Relaxed),
            self.high_water_bytes.load(Ordering::Relaxed),
        )
    }
}

static DMA_COUNTERS: DmaCounters = DmaCounters::new();

/// Record a DMA allocation after it succeeds.
pub fn dma_allocated(bytes: usize) {
    DMA_COUNTERS.allocated(bytes);
}

/// Record a DMA allocation being returned to the frame allocator.
pub fn dma_released(bytes: usize) {
    DMA_COUNTERS.released(bytes);
}

/// `(current bytes, high-water bytes)`.
pub fn dma_usage() -> (usize, usize) {
    DMA_COUNTERS.usage()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_counter_tracks_current_and_high_water() {
        // Use an isolated counter so unrelated allocations and releases
        // cannot change either snapshot while this test is running.
        let counters = DmaCounters::new();
        let (before, high_before) = counters.usage();
        counters.allocated(8192);
        let (current, high) = counters.usage();
        assert_eq!(current, before + 8192);
        assert!(high >= high_before.max(current));
        counters.released(8192);
        let (after, _) = counters.usage();
        assert_eq!(after, before);
    }
}
