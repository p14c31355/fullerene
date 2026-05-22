use crate::graphics::backend::FramebufferBackend;
use crate::graphics::color::FramebufferInfo;
use crate::graphics::framebuffer::FramebufferWriter;

pub struct GopFramebuffer {
    info: FramebufferInfo,
    writer: FramebufferWriter<u32>,
}

impl GopFramebuffer {
    pub fn new(info: FramebufferInfo) -> Self {
        let writer = FramebufferWriter::<u32>::new(info.clone());
        Self { info, writer }
    }
}

impl FramebufferBackend for GopFramebuffer {
    fn width(&self) -> usize {
        self.info.width as usize
    }
    fn height(&self) -> usize {
        self.info.height as usize
    }
    fn pitch(&self) -> usize {
        self.info.stride as usize
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        let total_size = (self.info.stride as usize) * (self.info.height as usize) * 4;
        unsafe { core::slice::from_raw_parts_mut(self.info.address as *mut u8, total_size) }
    }

    fn flush(&mut self) {
        // GOP framebuffer is usually directly mapped, so no explicit flush needed
    }
}
