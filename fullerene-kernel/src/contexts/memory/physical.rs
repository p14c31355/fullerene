//! PhysicalMemoryContext — frame allocation, MMIO, contiguous allocations.

/// Physical memory management context.
///
/// Tracks physical frame allocation and MMIO mapping state.
pub struct PhysicalMemoryContext {
    /// Total physical frames available (populated during init).
    pub total_frames: usize,
    /// Frames currently allocated.
    pub allocated_frames: usize,
    /// MMIO regions mapped (count).
    pub mmio_regions: usize,
}

impl PhysicalMemoryContext {
    pub const fn new() -> Self {
        Self {
            total_frames: 0,
            allocated_frames: 0,
            mmio_regions: 0,
        }
    }
}