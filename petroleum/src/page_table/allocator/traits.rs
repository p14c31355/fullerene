use x86_64::structures::paging::{FrameAllocator, Size4KiB};

pub trait FrameAllocatorExt: FrameAllocator<Size4KiB> {
    fn total_frames(&self) -> usize;
    fn set_frame_range(&mut self, start: usize, end: usize, used: bool);
    fn set_frame_used(&mut self, frame: usize, used: bool);
}