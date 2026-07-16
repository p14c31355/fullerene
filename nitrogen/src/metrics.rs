//! Lock-free driver resource counters.

use core::sync::atomic::{AtomicUsize, Ordering};

static DMA_CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static DMA_HIGH_WATER_BYTES: AtomicUsize = AtomicUsize::new(0);

fn update_high_water(candidate: usize) {
    let mut observed = DMA_HIGH_WATER_BYTES.load(Ordering::Relaxed);
    while candidate > observed {
        match DMA_HIGH_WATER_BYTES.compare_exchange_weak(
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

/// Record a DMA allocation after it succeeds.
pub fn dma_allocated(bytes: usize) {
    let current = DMA_CURRENT_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    update_high_water(current);
}

/// Record a DMA allocation being returned to the frame allocator.
pub fn dma_released(bytes: usize) {
    let _ = DMA_CURRENT_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(bytes))
    });
}

/// `(current bytes, high-water bytes)`.
pub fn dma_usage() -> (usize, usize) {
    (
        DMA_CURRENT_BYTES.load(Ordering::Relaxed),
        DMA_HIGH_WATER_BYTES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_counter_tracks_current_and_high_water() {
        let (before, high_before) = dma_usage();
        dma_allocated(8192);
        let (current, high) = dma_usage();
        assert_eq!(current, before + 8192);
        assert!(high >= high_before.max(current));
        dma_released(8192);
        assert_eq!(dma_usage().0, before);
    }
}
