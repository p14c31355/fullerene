use crate::common::logging::SystemResult;
use crate::page_table::allocator::traits::{FrameAllocator, FrameAllocatorExt};
use crate::page_table::memory_map::MemoryDescriptorValidator;
use crate::page_table::types::PhysFrame;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, PhysFrame as X86PhysFrame, Size4KiB,
};

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

    pub fn init_with_memory_map<T: MemoryDescriptorValidator>(memory_map: &[T]) -> Self {
        let mut max_phys = 0u64;
        for desc in memory_map {
            let end = desc.get_physical_start() + desc.get_page_count() * 4096;
            if end > max_phys {
                max_phys = end;
            }
        }
        let total_frames = ((max_phys + 4095) / 4096) as usize;
        let mut allocator = Self::new(total_frames);
        allocator
            .bitmap
            .resize(allocator.bitmap.capacity(), u64::MAX);

        for desc in memory_map {
            if desc.get_type() == crate::common::EfiMemoryType::EfiConventionalMemory as u32 {
                let start_frame = (desc.get_physical_start() / 4096) as usize;
                let end_frame =
                    ((desc.get_physical_start() + desc.get_page_count() * 4096) / 4096) as usize;
                allocator.set_frame_range(start_frame, end_frame, false);
            }
        }
        allocator
    }

    pub fn allocate_contiguous_frames(
        &mut self,
        pages: usize,
    ) -> crate::common::logging::SystemResult<u64> {
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

    pub fn free_frame(&mut self, frame: X86PhysFrame) {
        let phys_addr = frame.start_address().as_u64();
        let frame_idx = (phys_addr / 4096) as usize;
        if frame_idx < self.total_frames {
            self.set_frame_used(frame_idx, false);
        }
    }

    pub fn free_contiguous_frames(&mut self, start_phys: u64, pages: usize) {
        let start_frame = (start_phys / 4096) as usize;
        for i in 0..pages {
            self.set_frame_used(start_frame + i, false);
        }
    }

    pub fn reserve_frames(
        &mut self,
        start_phys: u64,
        pages: usize,
    ) -> crate::common::logging::SystemResult<()> {
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

    /// Allocate a frame from the low memory region (below 1MB).
    pub fn allocate_frame_low(&mut self) -> Option<X86PhysFrame> {
        const LOW_MEMORY_LIMIT: usize = 1024 * 1024 / 4096;
        let cr3_addr: u64;
        unsafe { core::arch::asm!("mov rax, cr3", out("rax") cr3_addr, options(nomem, nostack)) };
        let l4_frame_idx = (cr3_addr / 4096) as usize;
        for frame_idx in 1..LOW_MEMORY_LIMIT.min(self.total_frames) {
            if frame_idx == l4_frame_idx {
                continue;
            }
            let idx = frame_idx / 64;
            let bit = frame_idx % 64;
            if (self.bitmap[idx] & (1 << bit)) == 0 {
                self.set_frame_used(frame_idx, true);
                return Some(X86PhysFrame::containing_address(x86_64::PhysAddr::new(
                    frame_idx as u64 * 4096,
                )));
            }
        }
        None
    }
}

// ── Implement our custom FrameAllocator trait ──────────────────────────

impl FrameAllocator for BitmapFrameAllocator {
    fn allocate(&mut self) -> Result<PhysFrame, crate::page_table::allocator::traits::AllocError> {
        for i in 0..self.bitmap.len() {
            if self.bitmap[i] != u64::MAX {
                for j in 0..64 {
                    let frame_idx = i * 64 + j;
                    if frame_idx == 0 {
                        continue;
                    }
                    if frame_idx >= self.total_frames {
                        return Err(crate::page_table::allocator::traits::AllocError::OutOfMemory);
                    }
                    if (self.bitmap[i] & (1 << j)) == 0 {
                        self.set_frame_used(frame_idx, true);
                        let phys_addr = frame_idx as u64 * 4096;
                        return Ok(PhysFrame {
                            start_address: phys_addr,
                        });
                    }
                }
            }
        }
        Err(crate::page_table::allocator::traits::AllocError::OutOfMemory)
    }

    fn deallocate(&mut self, frame: PhysFrame) {
        let frame_idx = (frame.start_address() / 4096) as usize;
        if frame_idx < self.total_frames {
            self.set_frame_used(frame_idx, false);
        }
    }

    fn is_initialized(&self) -> bool {
        !self.bitmap.is_empty()
    }
}

// ── Implement FrameAllocatorExt ────────────────────────────────────────

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

    fn deallocate_frame(&mut self, frame: PhysFrame) {
        self.deallocate(frame);
    }
}

// ── Implement x86_64 FrameAllocator for backward compatibility ────────

unsafe impl X86FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<X86PhysFrame> {
        for i in 0..self.bitmap.len() {
            if self.bitmap[i] != u64::MAX {
                for j in 0..64 {
                    let frame_idx = i * 64 + j;
                    if frame_idx == 0 {
                        continue;
                    }
                    if frame_idx >= self.total_frames {
                        return None;
                    }
                    if (self.bitmap[i] & (1 << j)) == 0 {
                        self.set_frame_used(frame_idx, true);
                        let phys_addr = frame_idx as u64 * 4096;
                        return Some(X86PhysFrame::containing_address(x86_64::PhysAddr::new(
                            phys_addr,
                        )));
                    }
                }
            }
        }
        None
    }
}
