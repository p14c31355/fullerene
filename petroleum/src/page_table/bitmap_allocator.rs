use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::{
    structures::paging::{FrameAllocator, Page, PageTableFlags, PhysFrame, Size4KiB, Translate},
    PhysAddr, VirtAddr,
};
use spin::Once;

use super::constants::{PAGE_SIZE, UEFI_COMPAT_PAGES};
use crate::{
    calc_offset_addr, create_page_and_frame, debug_log_no_alloc, ensure_initialized,
    flush_tlb_and_verify, log_memory_descriptor, map_and_flush, map_with_offset,
};

/// Static buffer for bitmap - sized for up to 32GiB of RAM (8M frames)
/// Each bit represents one 4KB frame, so size is (8M / 64) = 128K u64s = 1MB
static mut BITMAP_STATIC: [u64; 131072] = [u64::MAX; 131072];

/// Bitmap-based frame allocator implementation
pub struct BitmapFrameAllocator {
    pub bitmap: Option<&'static mut [u64]>,
    pub frame_count: usize,
    pub next_free_frame: usize,
    pub initialized: bool,
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
    pub unsafe fn init(memory_map: &[super::efi_memory::EfiMemoryDescriptor]) -> Self {
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
        memory_map: &[super::efi_memory::EfiMemoryDescriptor],
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
            log_memory_descriptor!(desc, i);
        }

        let (max_addr, total_frames, bitmap_size) =
            super::efi_memory::calculate_frame_allocation_params(memory_map);

        debug_log_no_alloc!("Max address: 0x", max_addr as usize);
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
        if let Some(ref mut bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] &= !(1 << bit_index);
            }
        }
    }

    /// Set a frame as used in the bitmap
    pub fn set_frame_used(&mut self, frame_index: usize) {
        if let Some(ref mut bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] |= 1 << bit_index;
            }
        }
    }

    /// Check if a frame is free
    fn is_frame_free(&self, frame_index: usize) -> bool {
        if let Some(ref bitmap) = self.bitmap {
            let chunk_index = frame_index / 64;
            let bit_index = frame_index % 64;
            if chunk_index < bitmap.len() {
                (bitmap[chunk_index] & (1 << bit_index)) == 0
            } else {
                false
            }
        } else {
            false
        }
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
    fn allocate_contiguous_frames(&mut self, count: usize) -> crate::common::logging::SystemResult<usize> {
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
    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> crate::common::logging::SystemResult<()> {
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
    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> crate::common::logging::SystemResult<()> {
        self.allocate_frames_at(start_addr, count)
    }

    /// Release frames at a specific address
    fn release_frames(&mut self, start_addr: usize, count: usize) -> crate::common::logging::SystemResult<()> {
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
        let mut used = 0;
        let chunks_to_check = self.frame_count.div_ceil(64);

        for i in 0..chunks_to_check {
            used += bitmap[i].count_ones() as usize;
        }

        self.frame_count - used
    }

}

// Global heap allocator
pub static HEAP_INITIALIZED: Once<bool> = Once::new();

/// Initialize a new OffsetPageTable.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> x86_64::structures::paging::OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { x86_64::structures::paging::OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Returns a mutable reference to the active level 4 table.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`. Also, this function must be only called once
/// to avoid aliasing `&mut` references (which is undefined behavior).
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut x86_64::structures::paging::PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut x86_64::structures::paging::PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// Private function that is called by `translate_addr`.
///
/// This function is safe to limit the scope of `unsafe` because Rust is
/// conservative around generic types.
fn translate_addr_inner(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::page_table::FrameError;

    // read the active level 4 frame from the CR3 register
    let (level_4_table_frame, _) = Cr3::read();

    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let mut frame = level_4_table_frame;

    // traverse the multi-level page table
    for &index in &table_indexes {
        // convert the frame into a page table reference
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *const x86_64::structures::paging::PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr };

        // read the page table entry and update `frame`
        let entry = &table[index];
        frame = match entry.frame() {
            Ok(frame) => frame,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("huge pages not supported"),
        };
    }

    // calculate the physical address by adding the page offset
    Some(frame.start_address() + u64::from(addr.page_offset()))
}

/// Translates the given virtual address to the mapped physical address, or
/// `None` if the address is not mapped.
///
/// This function is unsafe because the caller must guarantee that the
/// complete physical memory is mapped to virtual memory at the passed
/// `physical_memory_offset`.
pub unsafe fn translate_addr(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    translate_addr_inner(addr, physical_memory_offset)
}

/// Returns the higher-half kernel mapping offset.
pub const HIGHER_HALF_OFFSET: VirtAddr = VirtAddr::new(0xFFFF_8000_0000_0000);
