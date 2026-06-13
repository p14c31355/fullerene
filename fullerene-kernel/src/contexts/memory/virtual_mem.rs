//! VirtualMemoryContext — page-table and address-space tracking.

/// Virtual memory management context.
pub struct VirtualMemoryContext {
    /// Number of active page tables (process + kernel).
    pub active_tables: usize,
    /// Total virtual pages mapped.
    pub mapped_pages: usize,
}

impl VirtualMemoryContext {
    pub const fn new() -> Self {
        Self {
            active_tables: 0,
            mapped_pages: 0,
        }
    }
}