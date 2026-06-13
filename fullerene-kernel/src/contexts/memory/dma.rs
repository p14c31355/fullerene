//! DmaContext — contiguous DMA buffer management.

/// DMA context tracking contiguous allocations used by HDA, NVMe, etc.
pub struct DmaContext {
    /// Number of active DMA regions.
    pub active_regions: usize,
    /// Total bytes allocated for DMA.
    pub total_bytes: usize,
}

impl DmaContext {
    pub const fn new() -> Self {
        Self {
            active_regions: 0,
            total_bytes: 0,
        }
    }
}