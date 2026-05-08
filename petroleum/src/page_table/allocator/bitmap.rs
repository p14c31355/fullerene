use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use crate::page_table::allocator::traits::FrameAllocatorExt;
use crate::common::logging::SystemResult;

pub struct BitmapFrameAllocator {
    bitmap: alloc::vec::Vec<u64>,
    total_frames: usize,
}

impl BitmapFrameAllocator {
    pub fn new(total_frames: usize) -> Self {
        let bitmap_size = (total_frames + 63) / 64;
        Self {
            bitmap: alloc::vec::Vec::with_capacity(bitmap_size),
            total_frames,
        }
    }

    pub fn init(&mut self, initial_used_frames: usize) {
        self.bitmap.resize(self.bitmap.capacity(), 0);
        for i in 0..initial_used_frames {
            self.set_frame_used(i, true);
        }
    }

    pub fn init_with_memory_map<T: crate::page_table::types::MemoryDescriptorValidator>(memory_map: &[T]) -> Self {
        let mut max_phys = 0u64;
        for desc in memory_map {
            let end = desc.get_physical_start() + desc.get_page_count() * 4096;
            if end > max_phys {
                max_phys = end;
            }
        }
        let total_frames = ((max_phys + 4095) / 4096) as usize;
        let mut allocator = Self::new(total_frames);
        allocator.bitmap.resize(allocator.bitmap.capacity(), u64::MAX);
        
        for desc in memory_map {
            if desc.get_type() == crate::common::EfiMemoryType::EfiConventionalMemory as u32 {
                let start_frame = (desc.get_physical_start() / 4096) as usize;
                let end_frame = ((desc.get_physical_start() + desc.get_page_count() * 4096) / 4096) as usize;
                allocator.set_frame_range(start_frame, end_frame, false);
            }
        }
        allocator
    }

    pub fn allocate_contiguous_frames(&mut self, pages: usize) -> crate::common::logging::SystemResult<u64> {
        let mut count = 0;
        let mut start = 0;
        for i in 0..self.total_frames {
            if !self.is_frame_available(i) {
                count = 0;
                start = i + 1;
            } else if count + 1 == pages {
                for j in start..=i {
                    self.set_frame_used(j, true);
                }
                return Ok(start as u64 * 4096);
            } else {
                count += 1;
            }
        }
        Err(crate::common::logging::SystemError::FrameAllocationFailed)
    }

    pub fn available_frames(&self) -> usize {
        let mut count = 0;
        for i in 0..self.total_frames {
            if self.is_frame_available(i) {
                count += 1;
            }
        }
        count
    }

    pub fn frame_size(&self) -> usize {
        4096
    }

    pub fn is_frame_available(&self, frame: usize) -> bool {
        if frame >= self.total_frames {
            return false;
        }
        let idx = frame / 64;
        let bit = frame % 64;
        (self.bitmap[idx] & (1 << bit)) == 0
    }

    pub fn free_frame(&mut self, frame: x86_64::structures::paging::PhysFrame) {
        self.deallocate_frame(frame);
    }

    pub fn free_contiguous_frames(&mut self, start_phys: u64, pages: usize) {
        let start_frame = (start_phys / 4096) as usize;
        for i in 0..pages {
            self.set_frame_used(start_frame + i, false);
        }
    }

    pub fn reserve_frames(&mut self, start_phys: u64, pages: usize) -> crate::common::logging::SystemResult<()> {
        let start_frame = (start_phys / 4096) as usize;
        for i in 0..pages {
            self.set_frame_used(start_frame + i, true);
        }
        Ok(())
    }

    pub fn release_frames(&mut self, start_phys: u64, pages: usize) {
        let start_frame = (start_phys / 4096) as usize;
        for i in 0..pages {
            self.set_frame_used(start_frame + i, false);
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        for i in 0..self.bitmap.len() {
            if self.bitmap[i] != u64::MAX {
                for j in 0..64 {
                    if (self.bitmap[i] & (1 << j)) == 0 {
                        let frame_idx = i * 64 + j;
                        if frame_idx >= self.total_frames {
                            return None;
                        }
                        self.set_frame_used(frame_idx, true);
                        return Some(PhysFrame::containing_address(x86_64::PhysAddr::new(frame_idx as u64 * 4096)));
                    }
                }
            }
        }
        None
    }
}

impl FrameAllocatorExt for BitmapFrameAllocator {
    fn total_frames(&self) -> usize {
        self.total_frames
    }

    fn set_frame_range(&mut self, start: usize, end: usize, used: bool) {
        for i in start..end {
            self.set_frame_used(i, used);
        }
    }

    fn set_frame_used(&mut self, frame: usize, used: bool) {
        if frame >= self.total_frames {
            return;
        }
        let idx = frame / 64;
        let bit = frame % 64;
        if used {
            self.bitmap[idx] |= 1 << bit;
        } else {
            self.bitmap[idx] &= !(1 << bit);
        }
    }

    fn deallocate_frame(&mut self, frame: x86_64::structures::paging::PhysFrame) {
        let frame_idx = (frame.start_address().as_u64() / 4096) as usize;
        self.set_frame_used(frame_idx, false);
    }
}

