// fullerene-kernel/src/graphics.rs

use core::fmt;
use spin::{Mutex, Once};
use petroleum::common::FullereneFramebufferConfig;

// A simple 8x8 PC screen font (Code Page 437).
// This is a placeholder. A more complete font would be needed for full ASCII/Unicode support.
static FONT: [[u8; 8]; 256] = include!("font.txt");

struct FramebufferWriter {
    framebuffer: Framebuffer,
    x_pos: u32,
    y_pos: u32,
    fg_color: u32,
    bg_color: u32,
}

struct Framebuffer {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: petroleum::common::EfiGraphicsPixelFormat,
}

impl FramebufferWriter {
    fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        FramebufferWriter {
            framebuffer: Framebuffer {
                address: fb_config.address,
                width: fb_config.width,
                height: fb_config.height,
                stride: fb_config.stride,
                pixel_format: fb_config.pixel_format,
            },
            x_pos: 0,
            y_pos: 0,
            fg_color: 0xFFFFFF, // White
            bg_color: 0x0000FF, // Blue
        }
    }

    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.framebuffer.width || y >= self.framebuffer.height {
            return;
        }
        let bytes_per_pixel = 4; // Assuming 32-bit color based on common formats
        let offset = (y * self.framebuffer.stride + x) * bytes_per_pixel;
        let fb_ptr = self.framebuffer.address as *mut u8;
        unsafe {
            // Assuming a BGRx or RGBx 32-bit format.
            // The color is ARGB, so we might need to swizzle.
            // For now, we write it directly.
            let pixel_ptr = fb_ptr.add(offset as usize) as *mut u32;
            *pixel_ptr = color;
        }
    }

    fn clear_screen(&self) {
        let line_bytes = self.framebuffer.width * 4;
        for y in 0..self.framebuffer.height {
            let offset = y * self.framebuffer.stride * 4;
            let line_ptr = (self.framebuffer.address + offset as u64) as *mut u32;
            for x in 0..self.framebuffer.width {
                unsafe { *line_ptr.add(x as usize) = self.bg_color; }
            }
        }
    }

    fn new_line(&mut self) {
        self.y_pos += 8; // Font height
        self.x_pos = 0;
        if self.y_pos + 8 > self.framebuffer.height {
            // Simple scrolling: clear screen and reset
            self.y_pos = 0;
            self.clear_screen();
        }
    }

    fn draw_char(&self, c: char, x: u32, y: u32) {
        let char_idx = c as usize;
        if !c.is_ascii() || char_idx >= FONT.len() {
            return;
        }
        let font_char = FONT[char_idx];
        for (row, byte) in font_char.iter().enumerate() {
            for col in 0..8 {
                let color = if (byte >> (7 - col)) & 1 == 1 {
                    self.fg_color
                } else {
                    self.bg_color
                };
                self.put_pixel(x + col, y + row as u32, color);
            }
        }
    }
}

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            match c {
                '\n' => self.new_line(),
                _ => {
                    if self.x_pos + 8 > self.framebuffer.width {
                        self.new_line();
                    }
                    self.draw_char(c, self.x_pos, self.y_pos);
                    self.x_pos += 8;
                }
            }
        }
        Ok(())
    }
}

pub static WRITER: Once<Mutex<FramebufferWriter>> = Once::new();

pub fn init(config: &FullereneFramebufferConfig) {
    let writer = FramebufferWriter::new(config);
    writer.clear_screen();
    WRITER.call_once(|| Mutex::new(writer));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    if let Some(writer) = WRITER.get() {
        let mut writer = writer.lock();
        writer.write_fmt(args).unwrap();
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::graphics::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
