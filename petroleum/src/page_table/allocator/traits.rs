//! Frame allocator traits.
//!
//! Provides a unified interface for physical frame allocation with
//! proper error handling and optional fallback strategies.

use crate::page_table::types::{PhysFrame, SIZE_4K};

/// Errors from frame allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Out of physical memory.
    OutOfMemory,
    /// Requested alignment cannot be satisfied.
    InvalidAlignment,
    /// The allocator has not been initialized.
    NotInitialized,
}

impl core::fmt::Display for AllocError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AllocError::OutOfMemory => write!(f, "out of physical memory"),
            AllocError::InvalidAlignment => write!(f, "invalid alignment"),
            AllocError::NotInitialized => write!(f, "allocator not initialized"),
        }
    }
}

/// Trait for allocating physical frames.
///
/// Implementors must ensure that returned frames are zeroed and aligned
/// to at least 4 KiB boundaries.
pub trait FrameAllocator {
    /// Allocate a single 4 KiB frame.
    ///
    /// The frame must be zeroed before return.
    fn allocate(&mut self) -> Result<PhysFrame, AllocError>;

    /// Allocate a contiguous range of frames.
    ///
    /// Default implementation calls `allocate()` in a loop. Override for
    /// better performance with contiguous allocators.
    fn allocate_contiguous(&mut self, count: usize) -> Result<PhysFrame, AllocError> {
        if count == 0 {
            return Err(AllocError::InvalidAlignment);
        }
        let first = self.allocate()?;
        for _ in 1..count {
            let _ = self.allocate()?;
        }
        Ok(first)
    }

    /// Deallocate a single 4 KiB frame.
    fn deallocate(&mut self, frame: PhysFrame);

    /// Check if the allocator has been initialized.
    fn is_initialized(&self) -> bool;
}

/// A frame allocator that always fails. Useful as a placeholder.
pub struct NullAllocator;

impl FrameAllocator for NullAllocator {
    fn allocate(&mut self) -> Result<PhysFrame, AllocError> {
        Err(AllocError::NotInitialized)
    }

    fn deallocate(&mut self, _frame: PhysFrame) {}

    fn is_initialized(&self) -> bool {
        false
    }
}

/// Adapter: Convert any `FrameAllocator` into the simpler `FrameAlloc` trait
/// used by the page table walker.
pub struct WalkerAdapter<'a, A: FrameAllocator>(pub &'a mut A);

impl<'a, A: FrameAllocator> crate::page_table::raw::walker::FrameAlloc for WalkerAdapter<'a, A> {
    fn alloc_zeroed(&mut self) -> Option<u64> {
        self.0.allocate().ok().map(|f| f.start_address())
    }
}

/// Helper: Allocate a frame and zero it.
///
/// # Safety
/// The physical address must refer to a valid, accessible frame.
pub unsafe fn alloc_and_zero<A: FrameAllocator>(
    allocator: &mut A,
) -> Result<PhysFrame, AllocError> {
    let frame = allocator.allocate()?;
    core::ptr::write_bytes(frame.as_mut_ptr::<u8>(), 0, SIZE_4K as usize);
    Ok(frame)
}

/// Extended frame allocator trait with additional operations.
///
/// This trait extends `FrameAllocator` with methods needed by the
/// existing codebase (total_frames, set_frame_range, deallocate_frame, etc.)
pub trait FrameAllocatorExt: FrameAllocator {
    /// Total number of frames managed by this allocator.
    fn total_frames(&self) -> usize;
    /// Set a range of frames as used or free.
    fn set_frame_range(&mut self, start: usize, end: usize, used: bool);
    /// Set a single frame as used or free.
    fn set_frame_used(&mut self, frame: usize, used: bool);
    /// Deallocate a frame (backward-compatible with our custom PhysFrame).
    fn deallocate_frame(&mut self, frame: crate::page_table::types::PhysFrame);
    /// Total memory managed by this allocator in bytes.
    fn total_memory(&self) -> u64 {
        self.total_frames() as u64 * 4096
    }
}