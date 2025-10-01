use core::fmt;
use alloc::boxed::Box; // Import Box
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig, VgaFramebufferConfig}; // Import missing types
use spin::{Mutex, Once};
use core::marker::{Send, Sync};
use x86_64::instructions::port::Port;

use crate::font::FONT_8X8;

// A simple 8x8 PC screen font (Code Page 437).
// This is a placeholder. A more complete font would be needed for full ASCII/Unicode support.
static FONT: [[u8; 8]; 128] = FONT_8X8;

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

    fn bytes_per_pixel(&self) -> u32 {
        match self.framebuffer.pixel_format {
            EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
            | EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => 4,
            _ => panic!("Unsupported pixel format"),
        }
    }

    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.framebuffer.width || y >= self.framebuffer.height {
            return;
        }
        let bytes_per_pixel = self.bytes_per_pixel();
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
        let bytes_per_pixel = self.bytes_per_pixel();
        for y in 0..self.framebuffer.height {
            let offset = y * self.framebuffer.stride * bytes_per_pixel;
            let line_ptr = (self.framebuffer.address + offset as u64) as *mut u32;
            unsafe {
                let line_slice = core::slice::from_raw_parts_mut(line_ptr, self.framebuffer.width as usize);
                line_slice.fill(self.bg_color);
            }
        }
    }

    pub fn new_line(&mut self) {
        self.y_pos += 8; // Font height
        self.x_pos = 0;
        if self.y_pos >= self.framebuffer.height {
            let bytes_per_pixel = self.bytes_per_pixel();
            let bytes_per_line = self.framebuffer.stride * bytes_per_pixel;
            let shift_bytes = 8u64 * bytes_per_line as u64;
            let total_bytes = self.framebuffer.height as u64 * bytes_per_line as u64;
            let fb_ptr = self.framebuffer.address as *mut u8;
            unsafe {
                core::ptr::copy(
                    fb_ptr.add(shift_bytes as usize),
                    fb_ptr,
                    (total_bytes - shift_bytes) as usize,
                );
            }
            // Clear the last 8 lines
            let last_lines_offset = (self.framebuffer.height - 8) * self.framebuffer.stride * bytes_per_pixel;
            let clear_ptr = (self.framebuffer.address + last_lines_offset as u64) as *mut u32;
            let clear_num_u32 = 8 * self.framebuffer.stride as usize;
            unsafe {
                let clear_slice = core::slice::from_raw_parts_mut(clear_ptr, clear_num_u32);
                clear_slice.fill(self.bg_color);
            }
            self.y_pos -= 8;
        }
    }

    fn draw_char(&self, c: char, x: u32, y: u32) {
        let char_idx = (c as u8) as usize;
        // The FONT is now a 2D array [[u8; 8]; 128], so we access it directly.
        // We also need to ensure char_idx is within bounds for the 128 glyphs.
        if !c.is_ascii() || char_idx >= 128 {
            return;
        }
        let font_char = &FONT[char_idx];
        for (row, &byte) in font_char.iter().enumerate() {
            for col in 0..8 {
                let color = if (byte >> (7 - col)) & 1 == 1 {
                    self.fg_color
                } else {
                    self.bg_color
                };
                self.put_pixel(x + col as u32, y + row as u32, color);
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
struct VgaWriter {
    address: u64,
    width: u32,
    height: u32,
    x_pos: u32,
    y_pos: u32,
    fg_color: u8,  // 256-color palette index
    bg_color: u8,
}

#[cfg(not(target_os = "uefi"))]
impl VgaWriter {
    fn new(config: &VgaFramebufferConfig) -> Self {
        VgaWriter {
            address: config.address,
            width: config.width,
            height: config.height,
            x_pos: 0,
            y_pos: 0,
            fg_color: 0x0Fu8, // White
            bg_color: 0x00u8, // Black
        }
    }

    fn put_pixel(&self, x: u32, y: u32, color: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) as usize;
        unsafe {
            let fb_ptr = self.address as *mut u8;
            *fb_ptr.add(offset) = color;
        }
    }

    fn clear_screen(&self) {
        let fb_ptr = self.address as *mut u8;
        unsafe {
            core::ptr::write_bytes(fb_ptr, self.bg_color, (self.width * self.height) as usize);
        }
    }

    pub fn new_line(&mut self) {
        self.y_pos += 8; // Font height
        self.x_pos = 0;
        if self.y_pos >= self.height {
            let bytes_per_line = self.width;
            let shift_bytes = 8u64 * bytes_per_line as u64;
            let total_bytes = self.height as u64 * bytes_per_line as u64;
            let fb_ptr = self.address as *mut u8;
            unsafe {
                core::ptr::copy(
                    fb_ptr.add(shift_bytes as usize),
                    fb_ptr,
                    (total_bytes - shift_bytes) as usize,
                );
            }
            // Clear last 8 lines
            let clear_offset = (self.height - 8) * self.width;
            let clear_size = 8 * self.width as usize;
            let fb_ptr = self.address as *mut u8;
            unsafe {
                let clear_ptr = fb_ptr.add(clear_offset as usize);
                core::ptr::write_bytes(clear_ptr, self.bg_color, clear_size);
            }
            self.y_pos -= 8;
        }
    }

    fn draw_char(&self, c: char, x: u32, y: u32) {
        let char_idx = (c as u8) as usize;
        if !c.is_ascii() || char_idx >= 128 {
            return;
        }
        let font_char = &FONT[char_idx];
        for (row, &byte) in font_char.iter().enumerate() {
            for col in 0..8 {
                let color = if (byte >> (7 - col)) & 1 == 1 {
                    self.fg_color
                } else {
                    self.bg_color
                };
                self.put_pixel(x + col as u32, y + row as u32, color);
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
                if self.x_pos + 8 > self.width {
                    self.new_line();
                }
            }
        }
        Ok(())
    }
}


#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    let writer = FramebufferWriter::new(config);
    writer.clear_screen();
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
}

// Define constants for VGA port addresses
const VGA_MISC_OUTPUT_PORT_ADDRESS: u16 = 0x3C2;
const VGA_CRTC_INDEX_PORT_ADDRESS: u16 = 0x3D4;
const VGA_CRTC_DATA_PORT_ADDRESS: u16 = 0x3D5;
const VGA_STATUS_PORT_ADDRESS: u16 = 0x3DA;
const VGA_ATTRIBUTE_INDEX_PORT_ADDRESS: u16 = 0x3C0;
const VGA_DAC_INDEX_PORT_ADDRESS: u16 = 0x3C8;
const VGA_DAC_DATA_PORT_ADDRESS: u16 = 0x3C9;
const VGA_GRAPHICS_INDEX_PORT_ADDRESS: u16 = 0x3CE;
const VGA_GRAPHICS_DATA_PORT_ADDRESS: u16 = 0x3CF;
const VGA_SEQUENCER_INDEX_PORT_ADDRESS: u16 = 0x3C4;
const VGA_SEQUENCER_DATA_PORT_ADDRESS: u16 = 0x3C5;

#[cfg(not(target_os = "uefi"))]
/// Initializes VGA graphics mode 13h (320x200, 256 colors).
///
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga(config: &VgaFramebufferConfig) {
    setup_misc_output();
    setup_sequencer();
    setup_crtc(); // Must be done before other registers
    setup_graphics_controller();
    setup_attribute_controller();
    setup_palette();

    let writer = VgaWriter::new(config);
    writer.clear_screen();
    WRITER_BIOS.call_once(|| Mutex::new(Box::new(writer)));
}

/// Writes a value to an indexed VGA register.
fn write_indexed(index_port_addr: u16, data_port_addr: u16, index: u8, data: u8) {
    unsafe {
        let mut index_port = Port::new(index_port_addr);
        let mut data_port = Port::new(data_port_addr);
        index_port.write(index);
        data_port.write(data);
    }
}

/// Configures the Miscellaneous Output Register.
fn setup_misc_output() {
    unsafe {
        let mut misc_output_port = Port::new(VGA_MISC_OUTPUT_PORT_ADDRESS);
        misc_output_port.write(0x63u8); // Value for enabling VGA in 320x200x256 mode
    }
}

/// Configures the VGA Sequencer registers.
fn setup_sequencer() {
    const SEQUENCER_VALUES: &[(u8, u8)] = &[
        (0x00, 0x03), // Reset
        (0x01, 0x01), // Clocking mode
        (0x02, 0x0F), // Map mask
        (0x03, 0x00), // Character map select
        (0x04, 0x0E), // Memory mode (for 256 color, chain 4)
    ];
    for &(index, value) in SEQUENCER_VALUES {
        write_indexed(
            VGA_SEQUENCER_INDEX_PORT_ADDRESS,
            VGA_SEQUENCER_DATA_PORT_ADDRESS,
            index,
            value,
        );
    }
}

/// Configures the VGA CRTC (Cathode Ray Tube Controller) registers.
fn setup_crtc() {
    const CRTC_VALUES: &[(u8, u8)] = &[
        (0x00, 0x5F), // Horizontal total
        (0x01, 0x4F), // Horizontal displayed
        (0x02, 0x50), // Horizontal blanking start
        (0x03, 0x82), // Horizontal blanking end
        (0x04, 0x54), // Horizontal sync start
        (0x05, 0x80), // Horizontal sync end
        (0x06, 0xBF), // Vertical total
        (0x07, 0x1F), // Overflow
        (0x08, 0x00), // Preset row scan
        (0x09, 0x41), // Maximum scan line
        (0x10, 0x9C), // Vertical sync start
        (0x11, 0x8E), // Vertical sync end
        (0x12, 0x8F), // Vertical displayed
        (0x13, 0x28), // Row offset
        (0x14, 0x40), // Underline location
        (0x15, 0x96), // Vertical blanking start
        (0x16, 0xB9), // Vertical blanking end
        (0x17, 0xA3), // Line compare / Mode control
    ];
    for &(index, value) in CRTC_VALUES {
        write_indexed(
            VGA_CRTC_INDEX_PORT_ADDRESS,
            VGA_CRTC_DATA_PORT_ADDRESS,
            index,
            value,
        );
    }
}

/// Configures the VGA Graphics Controller registers.
fn setup_graphics_controller() {
    const GC_VALUES: &[(u8, u8)] = &[
        (0x00, 0x00), // Set/reset
        (0x01, 0x00), // Enable set/reset
        (0x02, 0x00), // Color compare
        (0x03, 0x00), // Data rotate
        (0x04, 0x00), // Read map select
        (0x05, 0x40), // Graphics mode (256 color)
        (0x06, 0x05), // Miscellaneous
        (0x07, 0x0F), // Color don't care
        (0x08, 0xFF), // Bit mask
    ];
    for &(index, value) in GC_VALUES {
        write_indexed(
            VGA_GRAPHICS_INDEX_PORT_ADDRESS,
            VGA_GRAPHICS_DATA_PORT_ADDRESS,
            index,
            value,
        );
    }
}

/// Configures the VGA Attribute Controller registers.
fn setup_attribute_controller() {
    const AC_VALUES: &[(u8, u8)] = &[
        (0x00, 0x00), // Mode control 1
        (0x01, 0x00), // Overscan color
        (0x02, 0x0F), // Color plane enable
        (0x03, 0x00), // Horizontal pixel panning
        (0x04, 0x00), // Color select
        (0x05, 0x00), // Mode control 2
        (0x06, 0x00), // Scroll
        (0x07, 0x00), // Graphics mode
        (0x08, 0xFF), // Line graphics
        (0x09, 0x00), // Foreground color
        (0x10, 0x41), // Mode control (for 256 colors)
        (0x11, 0x00), // Overscan color (border)
        (0x12, 0x0F), // Color plane enable
        (0x13, 0x00), // Horizontal pixel panning
        (0x14, 0x00), // Color select
    ];

    unsafe {
        let mut status_port = Port::<u8>::new(VGA_STATUS_PORT_ADDRESS);
        let mut index_port = Port::<u8>::new(VGA_ATTRIBUTE_INDEX_PORT_ADDRESS);
        let mut data_port = Port::<u8>::new(VGA_ATTRIBUTE_INDEX_PORT_ADDRESS); // Yes, same port for data

        // The AC registers are accessed in a slightly different way.
        // First, you read the status register to reset the index/data flip-flop.
        let _ = status_port.read();

        // Then, for each register, you write the index and then the data.
        for &(index, value) in AC_VALUES {
            index_port.write(index);
            data_port.write(value);
        }

        // Finally, enable video output by writing 0x20 to the index port.
        index_port.write(0x20);
    }
}

/// Sets up a simple grayscale palette for the 256-color mode.
fn setup_palette() {
    unsafe {
        let mut dac_index_port = Port::new(VGA_DAC_INDEX_PORT_ADDRESS);
        let mut dac_data_port = Port::new(VGA_DAC_DATA_PORT_ADDRESS);

        dac_index_port.write(0x00); // Start at color index 0

        for i in 0..256 {
            // Create a simple grayscale palette (6-bit values)
            let val = (i * 63 / 255) as u8;
            dac_data_port.write(val); // Red
            dac_data_port.write(val); // Green
            dac_data_port.write(val); // Blue
        }
    }
}

// Text mode structures for BIOS boot message
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum Color {
    Black = 0x0,
    LightGreen = 0xA,
    White = 0xF,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

struct VgaBuffer {
    buffer: &'static mut [[ScreenChar; BUFFER_WIDTH]; BUFFER_HEIGHT],
    column_position: usize,
    row_position: usize,
    color_code: ColorCode,
}

impl VgaBuffer {
    fn new() -> VgaBuffer {
        VgaBuffer {
            buffer: unsafe { &mut *(0xb8000 as *mut _) },
            column_position: 0,
            row_position: 0,
            color_code: ColorCode::new(Color::White, Color::Black),
        }
    }

    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = self.row_position;
                let col = self.column_position;

                self.buffer[row][col] = ScreenChar {
                    ascii_character: byte,
                    color_code: self.color_code,
                };
                self.column_position += 1;
            }
        }
    }

    fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        self.column_position = 0;
        if self.row_position < BUFFER_HEIGHT - 1 {
            self.row_position += 1;
        } else {
            for row in 1..BUFFER_HEIGHT {
                for col in 0..BUFFER_WIDTH {
                    self.buffer[row - 1][col] = self.buffer[row][col];
                }
            }
            self.clear_row(BUFFER_HEIGHT - 1);
        }
    }

    fn clear_row(&mut self, row: usize) {
        let blank_char = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer[row][col] = blank_char;
        }
    }

    fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.row_position = 0;
        self.update_cursor();
    }

    fn update_cursor(&self) {
        let pos = self.row_position * BUFFER_WIDTH + self.column_position;
        unsafe {
            const CURSOR_LOCATION_HIGH_REG: u8 = 0x0E;
            const CURSOR_LOCATION_LOW_REG: u8 = 0x0F;

            write_indexed(
                VGA_CRTC_INDEX_PORT_ADDRESS,
                VGA_CRTC_DATA_PORT_ADDRESS,
                CURSOR_LOCATION_LOW_REG,
                (pos & 0xFF) as u8,
            );
            write_indexed(
                VGA_CRTC_INDEX_PORT_ADDRESS,
                VGA_CRTC_DATA_PORT_ADDRESS,
                CURSOR_LOCATION_HIGH_REG,
                ((pos >> 8) & 0xFF) as u8,
            );
        }
    }
}

/// Initializes VGA text mode and prints boot message.
pub fn init_text() {
    let mut writer = VgaBuffer::new();
    writer.clear_screen();
    writer.color_code = ColorCode::new(Color::LightGreen, Color::Black);
    writer.write_string("Hello QEMU by fullerene!\n");
    writer.color_code = ColorCode::new(Color::White, Color::Black);
    writer.write_string("This is output directly to VGA.\n");
    writer.update_cursor();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
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
