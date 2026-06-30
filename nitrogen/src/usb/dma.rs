//! DMA memory allocation helpers — reduce the boilerplate of:
//!   allocate_contiguous_frames → phys_to_virt → zero → use
//!
//! # Example
//!
//! ```ignore
//! let dma = alloc_dma::<Trb>(ctx, 256)?;
//! let trbs = dma.as_mut();
//! let (page_virt, page_phys) = alloc_dma_page(ctx)?;
//! ```

use crate::DriverContext;

/// Owned DMA allocation.
///
/// Holds a pointer to contiguous zeroed memory and its physical address.
/// The lifetime of the returned memory is tied to this value, not `'static`.
pub(crate) struct DmaSlice<T> {
    ptr: *mut T,
    len: usize,
    pub(crate) phys: u64,
    pub(crate) pages: usize,
}

impl<T> DmaSlice<T> {
    /// Get a read-only slice to the allocated memory.
    pub fn as_ref(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Get a mutable slice to the allocated memory.
    pub fn as_mut(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Return a raw pointer to the start of the allocation.
    pub fn as_mut_ptr(&self) -> *mut T {
        self.ptr
    }
}

/// Allocate `n` elements of type `T` in contiguous physical memory.
/// Memory is zeroed.
pub(crate) fn alloc_dma<T>(ctx: &dyn DriverContext, n: usize) -> Option<DmaSlice<T>> {
    let elem_size = core::mem::size_of::<T>();
    if n == 0 || elem_size == 0 {
        return None;
    }
    let size = n.checked_mul(elem_size)?;
    let pages = size.checked_add(4095)? / 4096;
    let zero_len = pages.checked_mul(4096)?;
    let phys = ctx.allocate_contiguous_frames(pages).ok()?;
    let virt = ctx.phys_to_virt(phys) as *mut u8;
    unsafe { core::ptr::write_bytes(virt, 0, zero_len); }
    Some(DmaSlice { ptr: virt as *mut T, len: n, phys, pages })
}

/// Allocate a single zeroed 4KB page. Returns (virtual address, physical address).
pub(crate) fn alloc_dma_page(ctx: &dyn DriverContext) -> Option<(*mut u8, u64)> {
    let phys = ctx.allocate_contiguous_frames(1).ok()?;
    let virt = ctx.phys_to_virt(phys) as *mut u8;
    unsafe { core::ptr::write_bytes(virt, 0, 4096); }
    Some((virt, phys))
}

/// Free `pages` contiguous frames at `phys`.
pub(crate) fn free_dma(ctx: &dyn DriverContext, phys: u64, pages: usize) {
    ctx.free_contiguous_frames(phys, pages);
}

/// Free a single allocated page.
pub(crate) fn free_dma_page(ctx: &dyn DriverContext, phys: u64) {
    ctx.free_contiguous_frames(phys, 1);
}
