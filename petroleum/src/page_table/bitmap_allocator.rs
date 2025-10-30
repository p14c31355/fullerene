use spin::Once;
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{debug_log_no_alloc, mem_debug};

/// Static buffer for bitmap - sized for up to 32GiB of RAM (8M frames)
/// Each bit represents one 4KB frame, so size is (8M / 64) = 128K u64s = 1MB
pub(crate) static mut BITMAP_STATIC: [u64; 131072] = [u64::MAX; 131072];

/// Bitmap-based frame allocator implementation
pub struct BitmapFrameAllocator {
    bitmap: Option<&'static mut [u64]>,
    frame_count: usize,
    next_free_frame: usize,
    initialized: bool,
}

impl BitmapFrameAllocator {
    /// Create a new bitmap frame allocator
    pub fn new() -> Self {
        Self {
            bitmap: None,
            frame_count: 0,
            next_free_frame: 0,
            initialized: false,
        }
    }

    /// Create a FrameAllocator from the passed memory map.
    ///
    /// # Safety
    ///
    /// This function is unsafe because calling it multiple times will cause
    /// mutable aliasing of the global static `BITMAP_STATIC` buffer, leading
    /// to undefined behavior. It must only be called once during system initialization.
    /// (for compatibility)
    pub unsafe fn init(memory_map: &[impl super::efi_memory::MemoryDescriptorValidator]) -> Self {
        let mut allocator = BitmapFrameAllocator::new();
        unsafe {
            allocator
                .init_with_memory_map(memory_map)
                .expect("Failed to init bitmap allocator");
        }
        allocator
    }

    /// Initialize with EFI memory map
    pub unsafe fn init_with_memory_map(
        &mut self,
        memory_map: &[impl super::efi_memory::MemoryDescriptorValidator],
    ) -> crate::common::logging::SystemResult<()> {
        // Debug: Log memory map information
        debug_log_no_alloc!("Memory map contains ", memory_map.len(), " descriptors");

        // Validate memory map is not empty
        if memory_map.is_empty() {
            debug_log_no_alloc!("ERROR: Empty memory map received");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Debug: Log each descriptor
        for (i, desc) in memory_map.iter().enumerate() {
            mem_debug!(
                "Memory descriptor ",
                i,
                ", type=",
                desc.get_type() as usize,
                ", phys_start=",
                desc.get_physical_start() as usize,
                ", pages=",
                desc.get_page_count() as usize,
                "\n"
            );
        }

        let (max_addr, total_frames, bitmap_size) =
            super::efi_memory::calculate_frame_allocation_params(memory_map);

        debug_log_no_alloc!("Max address: ", max_addr as usize);
        debug_log_no_alloc!("Calculated total frames: ", total_frames);

        if total_frames == 0 {
            debug_log_no_alloc!("ERROR: No valid frames found in memory map");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        debug_log_no_alloc!("Required bitmap size: ", bitmap_size);

        // Ensure bitmap size doesn't exceed our static buffer
        if bitmap_size > 131072 {
            debug_log_no_alloc!("ERROR: Bitmap size ", bitmap_size, " exceeds limit 131072");
            return Err(crate::common::logging::SystemError::InternalError);
        }

        // Get a mutable slice from the static buffer
        unsafe {
            self.bitmap = Some(&mut BITMAP_STATIC[..bitmap_size]);

            // Initialize bitmap - mark all as used initially
            for chunk in self.bitmap.as_mut().unwrap().iter_mut() {
                *chunk = u64::MAX;
            }
        }

        self.frame_count = total_frames;
        self.next_free_frame = 0;
        self.initialized = true;

        // Mark available frames as free based on memory map
        super::efi_memory::mark_available_frames(self, memory_map);

        debug_log_no_alloc!(
            "BitmapFrameAllocator initialized successfully with ",
            total_frames,
            " frames"
        );

        Ok(())
    }

    /// Set a frame as free in the bitmap
    fn set_frame_free(&mut self, frame_index: usize) {
        bit_ops!(bitmap_set_free, self.bitmap, frame_index);
    }

    /// Set a frame as used in the bitmap
    pub fn set_frame_used(&mut self, frame_index: usize) {
        bit_ops!(bitmap_set_used, self.bitmap, frame_index);
    }

    /// Check if a frame is free
    fn is_frame_free(&self, frame_index: usize) -> bool {
        bit_ops!(bitmap_is_free, self.bitmap, frame_index)
    }

    /// Find the next free frame starting from a given index
    fn find_next_free_frame(&self, start_index: usize) -> Option<usize> {
        if !self.initialized {
            return None;
        }

        self.bitmap
            .as_ref()
            .and_then(|bitmap| Self::find_frame_in_bitmap(bitmap, start_index, self.frame_count))
    }

    /// Allocate a specific frame range (for reserving used regions)
    pub fn allocate_frames_at(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        crate::ensure_initialized!(self);

        let start_frame = start_addr / 4096;
        let end_frame = start_frame + count;
        if end_frame > self.frame_count {
            return Err(crate::common::logging::SystemError::InvalidArgument);
        }

        // Check if frames are free before allocating to prevent double-allocation
        for frame_index in start_frame..end_frame {
            if !self.is_frame_free(frame_index) {
                debug_log_no_alloc!(
                    "Frame allocation failed: frame already in use at index ",
                    frame_index
                );
                return Err(crate::common::logging::SystemError::FrameAllocationFailed);
            }
        }

        // Mark frames as used
        self.set_frame_range(start_frame, end_frame, true);

        Ok(())
    }

    /// Deallocate a specific frame back to the free pool
    pub fn deallocate_frame(&mut self, frame: PhysFrame) {
        if !self.initialized {
            return;
        }
        let frame_index = (frame.start_address().as_u64() / 4096) as usize;
        if frame_index < self.frame_count {
            self.set_frame_free(frame_index);
        }
    }

    /// Set a range of frames as used or free
    pub fn set_frame_range(&mut self, start_frame: usize, end_frame: usize, used: bool) {
        for i in start_frame..end_frame {
            if used {
                self.set_frame_used(i);
            } else {
                self.set_frame_free(i);
            }
        }
    }

    /// Helper method for bitmap operations
    fn find_frame_in_bitmap(
        bitmap: &[u64],
        start_index: usize,
        frame_count: usize,
    ) -> Option<usize> {
        let mut chunk_index = start_index / 64;
        let bit_in_chunk = start_index % 64;

        if chunk_index < bitmap.len() {
            let mut chunk = bitmap[chunk_index];
            chunk |= (1u64.wrapping_shl(bit_in_chunk as u32)).wrapping_sub(1);
            if chunk != u64::MAX {
                let first_free_bit = (!chunk).trailing_zeros() as usize;
                if chunk_index * 64 + first_free_bit < frame_count {
                    return Some(chunk_index * 64 + first_free_bit);
                }
            }
            chunk_index += 1;
        }

        for i in chunk_index..bitmap.len() {
            if bitmap[i] != u64::MAX {
                let first_free_bit = (!bitmap[i]).trailing_zeros() as usize;
                if i * 64 + first_free_bit < frame_count {
                    return Some(i * 64 + first_free_bit);
                }
            }
        }
        None
    }

    /// Allocate contiguous frames for large allocations
    fn allocate_contiguous_frames(
        &mut self,
        count: usize,
    ) -> crate::common::logging::SystemResult<usize> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let mut start_index = 0;
        let mut found_count = 0;

        for i in 0..self.frame_count {
            if self.is_frame_free(i) {
                if found_count == 0 {
                    start_index = i;
                }
                found_count += 1;
                if found_count == count {
                    for j in 0..count {
                        self.set_frame_used(start_index + j);
                    }
                    return Ok(start_index * 4096);
                }
            } else {
                found_count = 0;
            }
        }
        Err(crate::common::logging::SystemError::FrameAllocationFailed)
    }

    /// Deallocate contiguous frames
    fn free_contiguous_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        if !self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }

        let start_frame = start_addr / 4096;
        for frame_index in start_frame..(start_frame + count) {
            if frame_index < self.frame_count {
                self.set_frame_free(frame_index);
            }
        }
        Ok(())
    }

    /// Reserve frames at a specific address
    fn reserve_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        self.allocate_frames_at(start_addr, count)
    }

    /// Release frames at a specific address
    fn release_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        self.free_contiguous_frames(start_addr, count)
    }

    /// Check if a frame is available
    fn is_frame_available(&self, frame_addr: usize) -> bool {
        let frame_index = frame_addr / 4096;
        frame_index < self.frame_count && self.is_frame_free(frame_index)
    }

    /// Get the frame size (constant)
    fn frame_size(&self) -> usize {
        4096
    }

    /// Get available frames count
    fn available_frames(&self) -> usize {
        if !self.initialized || self.bitmap.is_none() {
            return 0;
        }

        let bitmap = self.bitmap.as_ref().unwrap();
        let mut free_frames = 0;
        let full_chunks = self.frame_count / 64;

        for i in 0..full_chunks {
            free_frames += bitmap[i].count_zeros() as usize;
        }

        let remainder_bits = self.frame_count % 64;
        if remainder_bits > 0 {
            let last_chunk = bitmap[full_chunks];
            let mask = (1u64 << remainder_bits) - 1;
            free_frames += (!last_chunk & mask).count_ones() as usize;
        }

        free_frames
    }

    /// Get total frames count
    pub fn total_frames(&self) -> usize {
        self.frame_count
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        if !self.initialized {
            return None;
        }

        if let Some(frame_index) = self.find_next_free_frame(self.next_free_frame) {
            self.set_frame_used(frame_index);
            self.next_free_frame = frame_index + 1;

            let frame_addr = frame_index * 4096;
            Some(PhysFrame::containing_address(PhysAddr::new(
                frame_addr as u64,
            )))
        } else {
            None
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for &mut BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        (**self).allocate_frame()
    }
}

// Implement petroleum's FrameAllocator trait for the BitmapFrameAllocator
impl crate::initializer::FrameAllocator for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> crate::common::logging::SystemResult<usize> {
        <Self as FrameAllocator<Size4KiB>>::allocate_frame(self)
            .map(|f| f.start_address().as_u64() as usize)
            .ok_or(crate::common::logging::SystemError::FrameAllocationFailed)
    }

    fn free_frame(&mut self, frame_addr: usize) -> crate::common::logging::SystemResult<()> {
        let frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
            x86_64::PhysAddr::new(frame_addr as u64),
        );
        self.deallocate_frame(frame);
        Ok(())
    }

    fn allocate_contiguous_frames(
        &mut self,
        count: usize,
    ) -> crate::common::logging::SystemResult<usize> {
        self.allocate_contiguous_frames(count)
    }

    fn free_contiguous_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        self.free_contiguous_frames(start_addr, count)
    }

    fn total_frames(&self) -> usize {
        self.frame_count
    }

    fn available_frames(&self) -> usize {
        self.available_frames()
    }

    fn reserve_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        self.reserve_frames(start_addr, count)
    }

    fn release_frames(
        &mut self,
        start_addr: usize,
        count: usize,
    ) -> crate::common::logging::SystemResult<()> {
        self.release_frames(start_addr, count)
    }

    fn is_frame_available(&self, frame_addr: usize) -> bool {
        self.is_frame_available(frame_addr)
    }

    fn frame_size(&self) -> usize {
        self.frame_size()
    }
}

// Global heap allocator
pub static HEAP_INITIALIZED: Once<bool> = Once::new();
