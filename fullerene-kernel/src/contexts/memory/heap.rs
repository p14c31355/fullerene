//! HeapContext — kernel heap sizing and statistics.

/// Kernel heap tracking context.
pub struct HeapContext {
    /// Heap start virtual address.
    pub heap_start: usize,
    /// Heap size in bytes.
    pub heap_size: usize,
    /// Bytes currently allocated.
    pub allocated_bytes: usize,
}

impl HeapContext {
    pub const fn new() -> Self {
        Self {
            heap_start: 0,
            heap_size: 0,
            allocated_bytes: 0,
        }
    }
}