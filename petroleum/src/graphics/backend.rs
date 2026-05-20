pub trait FramebufferBackend {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn pitch(&self) -> usize;
    fn buffer_mut(&mut self) -> &mut [u8];
    fn flush(&mut self);
}
