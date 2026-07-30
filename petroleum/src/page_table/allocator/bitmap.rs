use crate::page_table::allocator::traits::{FrameAllocator, FrameAllocatorExt};
use crate::page_table::memory_map::MemoryDescriptorValidator;
use crate::page_table::types::PhysFrame;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, PhysFrame as X86PhysFrame, Size4KiB,
};

/// Normal allocations stay above legacy low memory.  Call
/// `allocate_frame_low` explicitly for hardware that genuinely requires a
/// sub-16MiB DMA frame.
const LOW_MEM_SKIP_FRAMES: usize = 16 * 1024 * 1024 / 4096;

pub struct BitmapFrameAllocator {
    bitmap: alloc::vec::Vec<u64>,
    total_frames: usize,
}

impl BitmapFrameAllocator {
    fn take_available_frame(&mut self, start: usize, end: usize) -> Option<usize> {
        for frame_idx in start.max(1)..end.min(self.total_frames) {
            if self.is_frame_available(frame_idx) {
                self.set_frame_used(frame_idx, true);
                return Some(frame_idx);
            }
        }
        None
    }

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
            let end = desc
                .get_physical_start()
                .saturating_add(desc.get_page_count().saturating_mul(4096));
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
                let end_frame = ((desc
                    .get_physical_start()
                    .saturating_add(desc.get_page_count().saturating_mul(4096)))
                    / 4096) as usize;
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
        // Skip low memory (<16MB) to avoid:
        //   - IVT/BDA (0x00000-0x00FFF)
        //   - BIOS/bootloader data (0x05000-0x9FFFF)
        //   - VGA/ROM regions (0xA0000-0xFFFFF)
        //   - DMA-safe buffer must be in conventional RAM, not reserved/firmware areas
        //   - Some QEMU/UEFI configurations leave low memory for legacy compatibility
        // Using 16MB boundary to ensure we're well above all low-memory regions.
        let mut start = LOW_MEM_SKIP_FRAMES;
        for i in LOW_MEM_SKIP_FRAMES..self.total_frames {
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
        // Read CR3 through the safe `x86_64` API (replaces a `mov rax, cr3`
        // asm block). The PML4 frame must never be handed back to a caller.
        let cr3_addr = x86_64::registers::control::Cr3::read()
            .0
            .start_address()
            .as_u64();
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
        let frame_idx = self
            .take_available_frame(LOW_MEM_SKIP_FRAMES, self.total_frames)
            .or_else(|| self.take_available_frame(1, LOW_MEM_SKIP_FRAMES))
            .ok_or(crate::page_table::allocator::traits::AllocError::OutOfMemory)?;
        Ok(PhysFrame {
            start_address: frame_idx as u64 * 4096,
        })
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
        let frame_idx = self
            .take_available_frame(LOW_MEM_SKIP_FRAMES, self.total_frames)
            .or_else(|| self.take_available_frame(1, LOW_MEM_SKIP_FRAMES))?;
        Some(X86PhysFrame::containing_address(x86_64::PhysAddr::new(
            frame_idx as u64 * 4096,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_allocation_falls_back_below_16_mib_on_small_systems() {
        let mut allocator = BitmapFrameAllocator::new(128);
        allocator.init(1);

        let frame = FrameAllocator::allocate(&mut allocator).expect("fallback frame");

        assert_eq!(frame.start_address(), 4096);
    }

    #[test]
    fn ordinary_allocation_prefers_memory_at_or_above_16_mib() {
        let mut allocator = BitmapFrameAllocator::new(LOW_MEM_SKIP_FRAMES + 2);
        allocator.init(1);

        let frame = FrameAllocator::allocate(&mut allocator).expect("high frame");

        assert_eq!(frame.start_address(), LOW_MEM_SKIP_FRAMES as u64 * 4096);
    }
}
