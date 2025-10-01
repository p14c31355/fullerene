use core::fmt;

// Trait to unify display writers for text output across VGA text mode, framebuffer, and VGA graphics mode.
pub trait DisplayWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result;
    fn clear_screen(&mut self);
    fn new_line(&mut self);
    fn update_cursor(&mut self);
}
