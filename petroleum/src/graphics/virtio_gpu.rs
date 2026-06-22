use crate::graphics::FramebufferBackend;
use alloc::boxed::Box;
use nitrogen::virtio::gpu::VirtioGpu;

pub struct VirtioGpuFramebuffer {
    gpu: Box<VirtioGpu>,
    width: usize,
    height: usize,
    stride: usize,
    address: u64,
}

impl VirtioGpuFramebuffer {
    pub fn new(
        gpu: Box<VirtioGpu>,
        width: usize,
        height: usize,
        stride: usize,
        address: u64,
    ) -> Self {
        Self {
            gpu,
            width,
            height,
            stride,
            address,
        }
    }
}

impl FramebufferBackend for VirtioGpuFramebuffer {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn pitch(&self) -> usize {
        self.stride
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        let total_size = self.stride * self.height * 4;
        unsafe { core::slice::from_raw_parts_mut(self.address as *mut u8, total_size) }
    }

    fn flush(&mut self) {
        self.gpu.flush(self.width as u32, self.height as u32);
    }
}
