//! Bitmap-based Frame Allocator Implementation
//!
//! This module provides physical frame allocation and deallocation using a bitmap-based approach.

use super::*;
use petroleum::common::logging::{SystemError, SystemResult};
use x86_64::structures::paging::Size4KiB;

/// Frame operation types for type-safe frame range operations
#[derive(Debug, Clone, Copy)]
pub enum FrameOperation {
    /// Mark frames as free
    Free,
    /// Mark frames as used
    Used,
}

// Note: super::* used to inherit Debug/Clone traits and common types from parent module

/// Bitmap-based frame allocator implementation
pub struct BitmapFrameAllocator {
    bitmap: alloc::vec::Vec<u64>,
    frame_count: usize,
    next_free_frame: usize,
    initialized: bool,
}

impl BitmapFrameAllocator {
    /// Create a new bitmap frame allocator
    pub fn new() -> Self {
        Self {
            bitmap: alloc::vec::Vec::new(),
            frame_count: 0,
            next_free_frame: 0,
            initialized: false,
        }
    }

    /// Initialize with EFI memory map
    pub fn init_with_memory_map(
        &mut self,
        memory_map: &'static [petroleum::page_table::EfiMemoryDescriptor],
    ) -> SystemResult<()> {
        // Calculate total memory and initialize bitmap
        let mut total_frames = 0usize;

        for descriptor in memory_map {
            // EFI memory type 7 is EfiConventionalMemory (available RAM)
            if descriptor.type_ == petroleum::common::EfiMemoryType::EfiConventionalMemory {
                total_frames += descriptor.number_of_pages as usize;
            }
        }

        // Initialize bitmap (each bit represents a frame)
        let bitmap_size = (total_frames + 63) / 64; // Round up for 64-bit chunks
        self.bitmap = alloc::vec::Vec::new();
        self.bitmap.resize(bitmap_size, 0xFFFF_FFFF_FFFF_FFFF); // Mark all as used initially

        self.frame_count = total_frames;
        self.next_free_frame = 0;
        self.initialized = true;

        // Mark available frames as free
        for descriptor in memory_map {
            // EFI memory type 7 is EfiConventionalMemory (available RAM)
            if descriptor.type_ == petroleum::common::EfiMemoryType::EfiConventionalMemory {
                let start_frame = descriptor.physical_start as usize / 4096;
                let frame_count = descriptor.number_of_pages as usize;

                for i in 0..frame_count {
                    let frame_index = start_frame + i;
                    if frame_index < total_frames {
                        self.set_frame_free(frame_index);
                    }
                }
            }
        }

        Ok(())
    }

    /// Set a frame as free in the bitmap
    fn set_frame_free(&mut self, frame_index: usize) {
        let chunk_index = frame_index / 64;
        let bit_index = frame_index % 64;
        if chunk_index < self.bitmap.len() {
            self.bitmap[chunk_index] &= !(1 << bit_index);
        }
    }

    /// Set a frame as used in the bitmap using consolidated macro
    fn set_frame_used(&mut self, frame_index: usize) {
        let chunk_index = frame_index / 64;
        let bit_index = frame_index % 64;
        if chunk_index < self.bitmap.len() {
            self.bitmap[chunk_index] |= 1 << bit_index;
        }
    }

    /// Check if a frame is free using consolidated macro
    fn is_frame_free(&self, frame_index: usize) -> bool {
        let chunk_index = frame_index / 64;
        let bit_index = frame_index % 64;
        if chunk_index < self.bitmap.len() {
            (self.bitmap[chunk_index] & (1 << bit_index)) == 0
        } else {
            false
        }
    }

    /// Find the next free frame starting from a given index
    fn find_next_free_frame(&self, start_index: usize) -> Option<usize> {
        let mut index = start_index;

        while index < self.frame_count {
            if self.is_frame_free(index) {
                return Some(index);
            }
            index += 1;
        }

        None
    }

    /// Helper function to set a range of frames using macro
    fn set_frame_range(
        &mut self,
        start_frame: usize,
        count: usize,
        operation: FrameOperation,
    ) -> SystemResult<()> {
        if start_frame + count > self.frame_count {
            return Err(SystemError::InvalidArgument);
        }

        match operation {
            FrameOperation::Free => {
                for i in 0..count {
                    self.set_frame_free(start_frame + i);
                }
            }
            FrameOperation::Used => {
                for i in 0..count {
                    self.set_frame_used(start_frame + i);
                }
            }
        }

        Ok(())
    }
}

// Implementation of FrameAllocator trait for BitmapFrameAllocator
impl FrameAllocator for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(frame_index) = self.find_next_free_frame(self.next_free_frame) {
            self.set_frame_used(frame_index);
            self.next_free_frame = frame_index + 1;

            Ok(frame_index * 4096)
        } else {
            Err(SystemError::MemOutOfMemory)
        }
    }

    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let frame_index = frame_addr / 4096;
        if frame_index < self.frame_count {
            self.set_frame_free(frame_index);
            Ok(())
        } else {
            Err(SystemError::InvalidArgument)
        }
    }

    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // Find contiguous free frames
        let mut start_index = 0;
        let mut found_count = 0;

        for i in 0..self.frame_count {
            if self.is_frame_free(i) {
                if found_count == 0 {
                    start_index = i;
                }
                found_count += 1;

                if found_count == count {
                    // Mark all frames as used
                    for j in 0..count {
                        self.set_frame_used(start_index + j);
                    }

                    return Ok(start_index * 4096);
                }
            } else {
                found_count = 0;
            }
        }

        Err(SystemError::MemOutOfMemory)
    }

    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let start_frame = start_addr / 4096;
        self.set_frame_range(start_frame, count, FrameOperation::Free)
    }

    fn total_frames(&self) -> usize {
        self.frame_count
    }

    fn available_frames(&self) -> usize {
        let mut available = 0;

        for i in 0..self.frame_count {
            if self.is_frame_free(i) {
                available += 1;
            }
        }

        available
    }

    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let start_frame = start_addr / 4096;
        self.set_frame_range(start_frame, count, FrameOperation::Used)
    }

    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let start_frame = start_addr / 4096;
        self.set_frame_range(start_frame, count, FrameOperation::Free)
    }

    fn is_frame_available(&self, frame_addr: usize) -> bool {
        let frame_index = frame_addr / 4096;
        frame_index < self.frame_count && self.is_frame_free(frame_index)
    }

    fn frame_size(&self) -> usize {
        4096
    }
}

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<x86_64::structures::paging::PhysFrame<Size4KiB>> {
        <BitmapFrameAllocator as FrameAllocator>::allocate_frame(self)
            .ok()
            .and_then(|frame_addr| {
                Some(x86_64::structures::paging::PhysFrame::containing_address(
                    x86_64::PhysAddr::new(frame_addr as u64),
                ))
            })
    }
}

// Implementation of Initializable trait for BitmapFrameAllocator
impl Initializable for BitmapFrameAllocator {
    fn init(&mut self) -> SystemResult<()> {
        // Initialize with empty memory map
        let empty_map = &[];
        self.init_with_memory_map(empty_map)
    }

    fn name(&self) -> &'static str {
        "BitmapFrameAllocator"
    }

    fn priority(&self) -> i32 {
        900 // Very high priority for frame allocation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_frame_allocator_creation() {
        let allocator = BitmapFrameAllocator::new();
        assert_eq!(allocator.total_frames(), 0);
        assert!(!allocator.initialized);
    }
}
