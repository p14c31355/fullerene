use core::fmt;

// Trait to unify display writers for text output across VGA text mode, framebuffer, and VGA graphics mode.
pub trait DisplayWriter: Send + Sync + core::fmt::Write {
    // Re-declare write_str to satisfy the compiler, even though it's provided by core::fmt::Write
    // This makes it explicit that DisplayWriter requires this method.
    fn write_str(&mut self, s: &str) -> fmt::Result;
    fn clear_screen(&mut self);
    fn new_line(&mut self);
    fn update_cursor(&mut self);
}
