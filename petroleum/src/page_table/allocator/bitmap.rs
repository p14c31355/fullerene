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

