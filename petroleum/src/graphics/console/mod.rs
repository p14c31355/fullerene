use core::fmt;

pub trait Console: fmt::Write {
    fn write_char(&mut self, c: char, color: u32);
    fn set_color(&mut self, color: u32);
    fn clear(&mut self);
    fn set_cursor(&mut self, x: usize, y: usize);
    fn scroll(&mut self);
}
