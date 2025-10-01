// fullerene-kernel/src/graphics.rs

use core::fmt;
use alloc::boxed::Box;
use petroleum::common::{FullereneFramebufferConfig, VgaFramebufferConfig, EfiGraphicsPixelFormat};
use spin::{Mutex, Once};

// A simple 8x8 PC screen font (Code Page 437).
// This is a placeholder. A more complete font would be needed for full ASCII/Unicode support.
static FONT: [[u8; 8]; 256] = include!("font.txt");

#[cfg(target_os = "uefi")]
struct FramebufferWriter {
    framebuffer: Framebuffer,
    x_pos: u32,
    y_pos: u32,
    fg_color: u32,
    bg_color: u32,
}

#[cfg(target_os = "uefi")]
struct Framebuffer {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: EfiGraphicsPixelFormat,
}

#[cfg(target_os = "uefi")]
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
            fg_color: 0xFFFFFFu32, // White
            bg_color: 0x000000u32, // Black
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
                unsafe {
                    *line_ptr.add(x as usize) = self.bg_color;
                }
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

#[cfg(target_os = "uefi")]
impl core::fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if c == '\n' {
                self.new_line();
            } else {
                self.draw_char(c, self.x_pos, self.y_pos);
                self.x_pos += 8;
                if self.x_pos + 8 > self.framebuffer.width {
                    self.new_line();
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "uefi"))]
const VGA_WIDTH: u32 = 320;
#[cfg(not(target_os = "uefi"))]
const VGA_HEIGHT: u32 = 200;
#[cfg(not(target_os = "uefi"))]
const VGA_FB_ADDR: u64 = 0xA0000;

#[cfg(not(target_os = "uefi"))]
struct VgaWriter {
    x_pos: u32,
    y_pos: u32,
    fg_color: u8,  // 256-color palette index
    bg_color: u8,
}

#[cfg(not(target_os = "uefi"))]
impl VgaWriter {
    fn new() -> Self {
        VgaWriter {
            x_pos: 0,
            y_pos: 0,
            fg_color: 0x0Fu8, // White
            bg_color: 0x00u8, // Black
        }
    }

    fn put_pixel(&self, x: u32, y: u32, color: u8) {
        if x >= VGA_WIDTH || y >= VGA_HEIGHT {
            return;
        }
        let offset = (y * VGA_WIDTH + x) as usize;
        unsafe {
            let fb_ptr = VGA_FB_ADDR as *mut u8;
            *fb_ptr.add(offset) = color;
        }
    }

    fn clear_screen(&self) {
        let fb_ptr = VGA_FB_ADDR as *mut u8;
        for i in 0..(VGA_WIDTH * VGA_HEIGHT) as usize {
            unsafe {
                *fb_ptr.add(i) = self.bg_color;
            }
        }
    }

    fn new_line(&mut self) {
        self.y_pos += 8; // Font height
        self.x_pos = 0;
        if self.y_pos + 8 > VGA_HEIGHT {
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

#[cfg(not(target_os = "uefi"))]
impl core::fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if c == '\n' {
                self.new_line();
            } else {
                self.draw_char(c, self.x_pos, self.y_pos);
                self.x_pos += 8;
                if self.x_pos + 8 > VGA_WIDTH {
                    self.new_line();
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Once<Mutex<FramebufferWriter>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<VgaWriter>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    let writer = FramebufferWriter::new(config);
    writer.clear_screen();
    WRITER_UEFI.call_once(|| Mutex::new(writer));
}

#[cfg(not(target_os = "uefi"))]
pub fn init_vga(config: &VgaFramebufferConfig) {
    // Set VGA mode 13h using port writes (no asm!)
    use x86_64::instructions::port::Port;

    unsafe {
        // Simplified VGA mode 13h setup
        let mut port = Port::new(0x3C2);
        port.write(0xE3u8); // Misc output

        port = Port::new(0x3D4);
        port.write(0x00u8); // Horizontal total
        let mut data = Port::new(0x3D5); 
        data.write(0x00u8);
        port.write(0x01u8); data.write(0xCFu8); // Horizontal displayed
        port.write(0x02u8); data.write(0x4Fu8); // Horizontal blanking start
        port.write(0x03u8); data.write(0x50u8); // Horizontal blanking end
        port.write(0x04u8); data.write(0x82u8); // Horizontal sync start
        port.write(0x05u8); data.write(0xC3u8); // Horizontal sync end
        port.write(0x06u8); data.write(0xA0u8); // Vertical total
        port.write(0x07u8); data.write(0x00u8); // Overflow
        port.write(0x08u8); data.write(0x00u8); // Preset row scan
        port.write(0x09u8); data.write(0x00u8); // Maximum scan line
        port.write(0x10u8); data.write(0x40u8); // Vertical sync start
        port.write(0x11u8); data.write(0x00u8); // Vertical sync end
        port.write(0x12u8); data.write(0x00u8); // Vertical displayed
        port.write(0x13u8); data.write(0x00u8); // Vertical blanking start
        port.write(0x14u8); data.write(0x00u8); // Vertical blanking end
        port.write(0x17u8); data.write(0x00u8); // Line compare

        // Attribute controller (simplified)
        port = Port::new(0x3C0);
        port.write(0x00u8); data = Port::new(0x3C0); data.write(0x00u8); // Mode control
        port.write(0x01u8); data.write(0x01u8); // Overscan color
        port.write(0x02u8); data.write(0x0Fu8); // Color plane enable
        port.write(0x03u8); data.write(0x00u8); // Horizontal pixel panning
        port.write(0x04u8); data.write(0x00u8); // Color select
        port.write(0x05u8); data.write(0x00u8); // Mode control
        port.write(0x06u8); data.write(0x00u8); // Scroll
        port.write(0x07u8); data.write(0x00u8); // Graphics mode
        port.write(0x08u8); data.write(0xFFu8); // Line graphics
        port.write(0x09u8); data.write(0x00u8); // Foreground color
        port.write(0x10u8); data.write(0x00u8); // Background color
        port.write(0x11u8); data.write(0x00u8); // Border color
        port.write(0x12u8); data.write(0x00u8); // Internal palette
        port.write(0x13u8); data.write(0x00u8); // Internal palette
        port.write(0x14u8); data.write(0x00u8); // Internal palette
        port.write(0x15u8); data.write(0x00u8); // Internal palette
        port.write(0x16u8); data.write(0x00u8); // Internal palette
        port.write(0x17u8); data.write(0x00u8); // Internal palette

        // DAC (simplified, default palette)
        let mut port = Port::new(0x3C8);
        for i in 0..256 {
            port.write(i as u8); // Set index
            let mut data_port = Port::new(0x3C9);
            let val = (i * 63 / 255) as u8; // 6-bit grayscale
            data_port.write(val); // Red
            data_port.write(val); // Green
            data_port.write(val); // Blue
        }

        // Graphics controller
        port = Port::new(0x3CE);
        port.write(0x00u8); data = Port::new(0x3CF); data.write(0x00u8); // Set/reset
        port.write(0x01u8); data.write(0x00u8); // Enable set/reset
        port.write(0x02u8); data.write(0x00u8); // Color compare
        port.write(0x03u8); data.write(0x00u8); // Data rotate
        port.write(0x04u8); data.write(0x00u8); // Read map select
        port.write(0x05u8); data.write(0x10u8); // Graphics mode (256 color)
        port.write(0x06u8); data.write(0x40u8); // Miscellaneous
        port.write(0x07u8); data.write(0x0Fu8); // Color don't care
        port.write(0x08u8); data.write(0xFFu8); // Bit mask

        // Sequencer
        port = Port::new(0x3C4);
        port.write(0x00u8); data = Port::new(0x3C5); data.write(0x03u8); // Reset
        port.write(0x01u8); data.write(0x01u8); // Clocking mode
        port.write(0x02u8); data.write(0x0Fu8); // Map mask
        port.write(0x03u8); data.write(0x00u8); // Character map select
        port.write(0x04u8); data.write(0x03u8); // Memory mode (0x03 for 256 color)
    }

    let writer = VgaWriter::new();
    writer.clear_screen();
    WRITER_BIOS.call_once(|| Mutex::new(writer));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    #[cfg(target_os = "uefi")]
    {
        if let Some(writer) = WRITER_UEFI.get() {
            let mut writer = writer.lock();
            writer.write_fmt(args).ok();
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        if let Some(writer) = WRITER_BIOS.get() {
            let mut writer = writer.lock();
            writer.write_fmt(args).ok();
        }
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
