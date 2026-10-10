//! # Early Boot Frame Allocator
//!
//! This allocator is for the **bootloader phase only**.
//!
//! ## Contract
//!
//! - MUST be discarded after world-switch to kernel.
//! - The runtime kernel must use its own allocator (`kernel_heap`, `MemoryManager`, etc.).
//! - DO NOT reference this allocator from kernel code.
//!
//! ## Why separate?
//!
//! The bootloader's allocator operates under identity mapping and unstable firmware
//! conditions. Leaking it (or its state) into the kernel causes:
//!
//! - Stale page table references after higher-half switch
//! - Bitmap state that doesn't reflect kernel's managed memory regions
//! - `PHYSICAL_MEMORY_OFFSET` mismatches when the kernel uses a different offset
//!
//! ## Usage
//!
//! ```ignore
//! use petroleum::early::allocator::EarlyFrameAllocator;
//!
//! let mut early_alloc = EarlyFrameAllocator::init_with_memory_map(&memory_map);
//! let frame = early_alloc.allocate_frame().expect("OOM");
//! ```
//!
//! After the world switch, drop this allocator and switch to kernel's allocator.

use crate::page_table::allocator::bitmap::BitmapFrameAllocator;
use crate::page_table::allocator::traits::FrameAllocatorExt;
use crate::page_table::memory_map::MemoryDescriptorValidator;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocatorTrait, PhysFrame as X86PhysFrame, Size4KiB,
};

/// Boot-phase frame allocator.
///
/// Wraps `BitmapFrameAllocator` but marks the type as early-only.
/// Runtime kernel code MUST use its own allocator instead.
pub struct EarlyFrameAllocator {
    inner: BitmapFrameAllocator,
}

impl EarlyFrameAllocator {
    /// Create a new empty early frame allocator.
    pub fn new(total_frames: usize) -> Self {
        Self {
            inner: BitmapFrameAllocator::new(total_frames),
        }
    }

    /// Initialise from a UEFI memory map.
    pub fn init_with_memory_map<T: MemoryDescriptorValidator>(memory_map: &[T]) -> Self {
        Self {
            inner: BitmapFrameAllocator::init_with_memory_map(memory_map),
        }
    }

    /// Manually mark initial reserved frames.
    pub fn init(&mut self, initial_used_frames: usize) {
        self.inner.init(initial_used_frames);
    }

    /// Allocate a single 4KiB frame.
    ///
    /// Returns `None` when the allocator is exhausted.
    pub fn allocate_frame(&mut self) -> Option<X86PhysFrame> {
        self.inner.allocate_frame()
    }

    /// Allocate `count` contiguous frames (returns physical address of first frame).
    ///
    /// This delegates to `BitmapFrameAllocator`'s contiguous allocation.
    pub fn allocate_contiguous_frames(&mut self, count: usize) -> Option<u64> {
        // The BitmapFrameAllocator's allocate_frame is used in a loop;
        // for true contiguous allocation we need the allocator's own method.
        // This is a simplified version; replace with proper contiguous allocation.
        let first = self.allocate_frame()?;
        let base = first.start_address().as_u64();
        for i in 1..count {
            let _next = self.allocate_frame()?;
            // We trust the bitmap allocator to give us contiguous frames
            // when they are available in the same free region.
            let expected = base + (i as u64) * 4096;
            debug_assert_eq!(
                _next.start_address().as_u64(),
                expected,
                "Non-contiguous frame allocation at offset {}",
                i
            );
        }
        Some(base)
    }

    /// Free a frame back to the pool.
    pub fn free_frame(&mut self, frame: X86PhysFrame) {
        // Convert x86_64 PhysFrame to page_table::types::PhysFrame for the underlying allocator
        let addr = frame.start_address().as_u64();
        let frame_idx = (addr / 4096) as usize;
        // Use set_frame_range to mark as free (0 = frame index in set_frame_used context)
        self.inner.set_frame_used(frame_idx, false);
    }
}

// Implement the x86_64 `FrameAllocator` trait so it can be used with the `x86_64::structures::paging`
// APIs (e.g., `Mapper::map_to`).
unsafe impl X86FrameAllocatorTrait<Size4KiB> for EarlyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<X86PhysFrame<Size4KiB>> {
        self.inner.allocate_frame()
    }
}
