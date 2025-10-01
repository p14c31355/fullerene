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
        unsafe {
            core::ptr::write_bytes(fb_ptr, self.bg_color, (VGA_WIDTH * VGA_HEIGHT) as usize);
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
const VGA_MISC_OUTPUT_WRITE: u16 = 0x3C2;
#[cfg(not(target_os = "uefi"))]
const VGA_CRTC_INDEX: u16 = 0x3D4;
#[cfg(not(target_os = "uefi"))]
const VGA_CRTC_DATA: u16 = 0x3D5;
#[cfg(not(target_os = "uefi"))]
const VGA_ATTRIBUTE_INDEX: u16 = 0x3C0;
#[cfg(not(target_os = "uefi"))]
const VGA_DAC_INDEX: u16 = 0x3C8;
#[cfg(not(target_os = "uefi"))]
const VGA_DAC_DATA: u16 = 0x3C9;
#[cfg(not(target_os = "uefi"))]
const VGA_GRAPHICS_INDEX: u16 = 0x3CE;
#[cfg(not(target_os = "uefi"))]
const VGA_GRAPHICS_DATA: u16 = 0x3CF;
#[cfg(not(target_os = "uefi"))]
const VGA_SEQUENCER_INDEX: u16 = 0x3C4;
#[cfg(not(target_os = "uefi"))]
const VGA_SEQUENCER_DATA: u16 = 0x3C5;

#[cfg(not(target_os = "uefi"))]
pub fn init_vga(config: &VgaFramebufferConfig) {
    // Set VGA mode 13h using port writes (no asm!)
    use x86_64::instructions::port::Port;

    unsafe {
        // Miscellaneous output register
        let mut misc_output_port = Port::new(VGA_MISC_OUTPUT_WRITE);
        misc_output_port.write(0xE3u8);

        // CRTC registers
        let mut crtc_index_port = Port::new(VGA_CRTC_INDEX);
        let mut crtc_data_port = Port::new(VGA_CRTC_DATA);
        crtc_index_port.write(0x00u8); crtc_data_port.write(0x00u8); // Horizontal total
        crtc_index_port.write(0x01u8); crtc_data_port.write(0xCFu8); // Horizontal displayed
        crtc_index_port.write(0x02u8); crtc_data_port.write(0x4Fu8); // Horizontal blanking start
        crtc_index_port.write(0x03u8); crtc_data_port.write(0x50u8); // Horizontal blanking end
        crtc_index_port.write(0x04u8); crtc_data_port.write(0x82u8); // Horizontal sync start
        crtc_index_port.write(0x05u8); crtc_data_port.write(0xC3u8); // Horizontal sync end
        crtc_index_port.write(0x06u8); crtc_data_port.write(0xA0u8); // Vertical total
        crtc_index_port.write(0x07u8); crtc_data_port.write(0x00u8); // Overflow
        crtc_index_port.write(0x08u8); crtc_data_port.write(0x00u8); // Preset row scan
        crtc_index_port.write(0x09u8); crtc_data_port.write(0x00u8); // Maximum scan line
        crtc_index_port.write(0x10u8); crtc_data_port.write(0x40u8); // Vertical sync start
        crtc_index_port.write(0x11u8); crtc_data_port.write(0x00u8); // Vertical sync end
        crtc_index_port.write(0x12u8); crtc_data_port.write(0x00u8); // Vertical displayed
        crtc_index_port.write(0x13u8); crtc_data_port.write(0x00u8); // Vertical blanking start
        crtc_index_port.write(0x14u8); crtc_data_port.write(0x00u8); // Vertical blanking end
        crtc_index_port.write(0x17u8); crtc_data_port.write(0x00u8); // Line compare

        // Attribute controller registers
        let mut attribute_index_port = Port::new(VGA_ATTRIBUTE_INDEX);
        let mut attribute_data_port = Port::new(VGA_ATTRIBUTE_INDEX);
        attribute_index_port.write(0x00u8); attribute_data_port.write(0x00u8); // Mode control
        attribute_index_port.write(0x01u8); attribute_data_port.write(0x01u8); // Overscan color
        attribute_index_port.write(0x02u8); attribute_data_port.write(0x0Fu8); // Color plane enable
        attribute_index_port.write(0x03u8); attribute_data_port.write(0x00u8); // Horizontal pixel panning
        attribute_index_port.write(0x04u8); attribute_data_port.write(0x00u8); // Color select
        attribute_index_port.write(0x05u8); attribute_data_port.write(0x00u8); // Mode control
        attribute_index_port.write(0x06u8); attribute_data_port.write(0x00u8); // Scroll
        attribute_index_port.write(0x07u8); attribute_data_port.write(0x00u8); // Graphics mode
        attribute_index_port.write(0x08u8); attribute_data_port.write(0xFFu8); // Line graphics
        attribute_index_port.write(0x09u8); attribute_data_port.write(0x00u8); // Foreground color
        attribute_index_port.write(0x10u8); attribute_data_port.write(0x00u8); // Background color
        attribute_index_port.write(0x11u8); attribute_data_port.write(0x00u8); // Border color
        attribute_index_port.write(0x12u8); attribute_data_port.write(0x00u8); // Internal palette
        attribute_index_port.write(0x13u8); attribute_data_port.write(0x00u8); // Internal palette
        attribute_index_port.write(0x14u8); attribute_data_port.write(0x00u8); // Internal palette
        attribute_index_port.write(0x15u8); attribute_data_port.write(0x00u8); // Internal palette
        attribute_index_port.write(0x16u8); attribute_data_port.write(0x00u8); // Internal palette
        attribute_index_port.write(0x17u8); attribute_data_port.write(0x00u8); // Internal palette

        // DAC (simplified, default palette)
        let mut dac_index_port = Port::new(VGA_DAC_INDEX);
        let mut dac_data_port = Port::new(VGA_DAC_DATA);
        for i in 0..256 {
            dac_index_port.write(i as u8); // Set index
            let val = (i * 63 / 255) as u8; // 6-bit grayscale
            dac_data_port.write(val); // Red
            dac_data_port.write(val); // Green
            dac_data_port.write(val); // Blue
        }

        // Graphics controller registers
        let mut graphics_index_port = Port::new(VGA_GRAPHICS_INDEX);
        let mut graphics_data_port = Port::new(VGA_GRAPHICS_DATA);
        graphics_index_port.write(0x00u8); graphics_data_port.write(0x00u8); // Set/reset
        graphics_index_port.write(0x01u8); graphics_data_port.write(0x00u8); // Enable set/reset
        graphics_index_port.write(0x02u8); graphics_data_port.write(0x00u8); // Color compare
        graphics_index_port.write(0x03u8); graphics_data_port.write(0x00u8); // Data rotate
        graphics_index_port.write(0x04u8); graphics_data_port.write(0x00u8); // Read map select
        graphics_index_port.write(0x05u8); graphics_data_port.write(0x10u8); // Graphics mode (256 color)
        graphics_index_port.write(0x06u8); graphics_data_port.write(0x40u8); // Miscellaneous
        graphics_index_port.write(0x07u8); graphics_data_port.write(0x0Fu8); // Color don't care
        graphics_index_port.write(0x08u8); graphics_data_port.write(0xFFu8); // Bit mask

        // Sequencer registers
        let mut sequencer_index_port = Port::new(VGA_SEQUENCER_INDEX);
        let mut sequencer_data_port = Port::new(VGA_SEQUENCER_DATA);
        sequencer_index_port.write(0x00u8); sequencer_data_port.write(0x03u8); // Reset
        sequencer_index_port.write(0x01u8); sequencer_data_port.write(0x01u8); // Clocking mode
        sequencer_index_port.write(0x02u8); sequencer_data_port.write(0x0Fu8); // Map mask
        sequencer_index_port.write(0x03u8); sequencer_data_port.write(0x00u8); // Character map select
        sequencer_index_port.write(0x04u8); sequencer_data_port.write(0x03u8); // Memory mode (0x03 for 256 color)
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
