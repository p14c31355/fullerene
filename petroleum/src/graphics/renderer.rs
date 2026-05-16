/// Renderer trait provides a generic interface for 2D graphics operations.
/// This allows the kernel to be agnostic of the underlying hardware (e.g., Framebuffer, GPU).
pub trait Renderer {
    /// Draw a single pixel at the specified coordinates.
    fn draw_pixel(&mut self, x: i32, y: i32, color: u32);

    /// Draw a filled rectangle.
    fn draw_rect(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32);

    /// Draw text at the specified coordinates.
    fn draw_text(&mut self, x: i32, y: i32, text: &str, color: u32);

    /// Clear the entire rendering area with a single color.
    fn clear(&mut self, color: u32);

    /// Get the current resolution of the renderer.
    fn get_resolution(&self) -> (u32, u32);

    /// Present the rendered content to the display.
    /// For immediate-mode renderers, this may be a no-op.
    /// For double-buffered renderers, this performs the flip/swap.
    fn present(&mut self) {}
}
