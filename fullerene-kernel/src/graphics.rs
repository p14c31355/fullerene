use alloc::boxed::Box; // Import Box
use core::fmt;
use core::marker::{Send, Sync};
#[cfg(not(target_os = "uefi"))]
use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig}; // Import missing types
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

use crate::font::FONT_8X8;

// A simple 8x8 PC screen font (Code Page 437).
static FONT: [[u8; 8]; 128] = FONT_8X8;

fn draw_char(fb: &impl FramebufferLike, c: char, x: u32, y: u32) {
    let char_idx = (c as u8) as usize;
    if !c.is_ascii() || char_idx >= 128 {
        return;
    }
    let font_char = &FONT[char_idx];
    for (row, &byte) in font_char.iter().enumerate() {
        for col in 0..8 {
            let color = if (byte >> (7 - col)) & 1 == 1 {
                fb.get_fg_color()
            } else {
                fb.get_bg_color()
            };
            fb.put_pixel(x + col as u32, y + row as u32, color);
        }
    }
}

fn new_line(fb: &mut impl FramebufferLike) {
    let (_, mut y_pos) = fb.get_position();
    y_pos += 8; // Font height
    let x_pos = 0;
    if y_pos >= fb.get_height() {
        fb.scroll_up();
        y_pos -= 8;
    }
    fb.set_position(x_pos, y_pos);
}

fn write_text<W: FramebufferLike>(writer: &mut W, s: &str) -> core::fmt::Result {
    let (mut x_pos, mut y_pos) = writer.get_position();
    for c in s.chars() {
        if c == '\n' {
            new_line(writer);
            let (new_x, new_y) = writer.get_position();
            x_pos = new_x;
            y_pos = new_y;
        } else {
            draw_char(writer, c, x_pos, y_pos);
            x_pos += 8;
            if x_pos + 8 > writer.get_width() {
                new_line(writer);
                let (new_x, new_y) = writer.get_position();
                x_pos = new_x;
                y_pos = new_y;
            } else {
                writer.set_position(x_pos, y_pos);
            }
        }
    }
    Ok(())
}

/// Generic scroll function for framebuffer buffers.
/// Scrolls the buffer up by 8 lines, clearing the last 8 lines with bg_color.
unsafe fn scroll_buffer<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let bytes_per_line = stride * bytes_per_pixel;
    let shift_bytes = 8u64 * bytes_per_line as u64;
    let total_bytes = height as u64 * bytes_per_line as u64;
    let fb_ptr = address as *mut u8;
    core::ptr::copy(
        fb_ptr.add(shift_bytes as usize),
        fb_ptr,
        (total_bytes - shift_bytes) as usize,
    );
    // Clear the last 8 lines
    let clear_offset = (height - 8) as usize * bytes_per_line as usize;
    let clear_ptr = (address + clear_offset as u64) as *mut T;
    let clear_count = 8 * stride as usize;
    core::slice::from_raw_parts_mut(clear_ptr, clear_count).fill(bg_color);
}

trait FramebufferLike {
    fn put_pixel(&self, x: u32, y: u32, color: u32);
    fn clear_screen(&self);
    fn get_width(&self) -> u32;
    fn get_height(&self) -> u32;
    fn get_fg_color(&self) -> u32;
    fn get_bg_color(&self) -> u32;
    fn set_position(&mut self, x: u32, y: u32);
    fn get_position(&self) -> (u32, u32);
    fn scroll_up(&self);
}

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

    fn scroll_up(&self) {
        unsafe {
            scroll_buffer::<u32>(
                self.framebuffer.address,
                self.framebuffer.stride,
                self.framebuffer.height,
                self.bg_color,
            );
        }
    }
}

#[cfg(target_os = "uefi")]
impl FramebufferLike for FramebufferWriter {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.framebuffer.width || y >= self.framebuffer.height {
            return;
        }
        let bytes_per_pixel = self.bytes_per_pixel();
        let offset = (y * self.framebuffer.stride + x) * bytes_per_pixel;
        let fb_ptr = self.framebuffer.address as *mut u8;
        unsafe {
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
                let line_slice =
                    core::slice::from_raw_parts_mut(line_ptr, self.framebuffer.width as usize);
                line_slice.fill(self.bg_color);
            }
        }
    }

    fn get_width(&self) -> u32 {
        self.framebuffer.width
    }

    fn get_height(&self) -> u32 {
        self.framebuffer.height
    }

    fn get_fg_color(&self) -> u32 {
        self.fg_color
    }

    fn get_bg_color(&self) -> u32 {
        self.bg_color
    }

    fn set_position(&mut self, x: u32, y: u32) {
        self.x_pos = x;
        self.y_pos = y;
    }

    fn get_position(&self) -> (u32, u32) {
        (self.x_pos, self.y_pos)
    }

    fn scroll_up(&self) {
        FramebufferWriter::scroll_up(self);
    }
}

#[cfg(target_os = "uefi")]
impl core::fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
    }
}

#[cfg(not(target_os = "uefi"))]
struct VgaWriter {
    address: u64,
    width: u32,
    height: u32,
    x_pos: u32,
    y_pos: u32,
    fg_color: u8, // 256-color palette index
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

    fn scroll_up(&self) {
        unsafe { scroll_buffer::<u8>(self.address, self.width, self.height, self.bg_color) };
    }
}

#[cfg(not(target_os = "uefi"))]
impl FramebufferLike for VgaWriter {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) as usize;
        unsafe {
            let fb_ptr = self.address as *mut u8;
            *fb_ptr.add(offset) = color as u8;
        }
    }

    fn clear_screen(&self) {
        let fb_ptr = self.address as *mut u8;
        unsafe {
            core::ptr::write_bytes(fb_ptr, self.bg_color, (self.width * self.height) as usize);
        }
    }

    fn get_width(&self) -> u32 {
        self.width
    }

    fn get_height(&self) -> u32 {
        self.height
    }

    fn get_fg_color(&self) -> u32 {
        self.fg_color as u32
    }

    fn get_bg_color(&self) -> u32 {
        self.bg_color as u32
    }

    fn set_position(&mut self, x: u32, y: u32) {
        self.x_pos = x;
        self.y_pos = y;
    }

    fn get_position(&self) -> (u32, u32) {
        (self.x_pos, self.y_pos)
    }

    fn scroll_up(&self) {
        VgaWriter::scroll_up(self);
    }
}

#[cfg(not(target_os = "uefi"))]
impl core::fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
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

/// Macro to setup multiple registers at once.
macro_rules! setup_registers {
    ($index_port:expr, $data_port:expr, $($index:expr => $value:expr),*) => {
        $(
            write_indexed($index_port, $data_port, $index, $value);
        )*
    };
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
    setup_registers!(
        VGA_SEQUENCER_INDEX_PORT_ADDRESS,
        VGA_SEQUENCER_DATA_PORT_ADDRESS,
        0x00 => 0x03, // Reset
        0x01 => 0x01, // Clocking mode
        0x02 => 0x0F, // Map mask
        0x03 => 0x00, // Character map select
        0x04 => 0x0E  // Memory mode (for 256 color, chain 4)
    );
}

/// Configures the VGA CRTC (Cathode Ray Tube Controller) registers.
fn setup_crtc() {
    setup_registers!(
        VGA_CRTC_INDEX_PORT_ADDRESS,
        VGA_CRTC_DATA_PORT_ADDRESS,
        0x00 => 0x5F, // Horizontal total
        0x01 => 0x4F, // Horizontal displayed
        0x02 => 0x50, // Horizontal blanking start
        0x03 => 0x82, // Horizontal blanking end
        0x04 => 0x54, // Horizontal sync start
        0x05 => 0x80, // Horizontal sync end
        0x06 => 0xBF, // Vertical total
        0x07 => 0x1F, // Overflow
        0x08 => 0x00, // Preset row scan
        0x09 => 0x41, // Maximum scan line
        0x10 => 0x9C, // Vertical sync start
        0x11 => 0x8E, // Vertical sync end
        0x12 => 0x8F, // Vertical displayed
        0x13 => 0x28, // Row offset
        0x14 => 0x40, // Underline location
        0x15 => 0x96, // Vertical blanking start
        0x16 => 0xB9, // Vertical blanking end
        0x17 => 0xA3  // Line compare / Mode control
    );
}

/// Configures the VGA Graphics Controller registers.
fn setup_graphics_controller() {
    setup_registers!(
        VGA_GRAPHICS_INDEX_PORT_ADDRESS,
        VGA_GRAPHICS_DATA_PORT_ADDRESS,
        0x00 => 0x00, // Set/reset
        0x01 => 0x00, // Enable set/reset
        0x02 => 0x00, // Color compare
        0x03 => 0x00, // Data rotate
        0x04 => 0x00, // Read map select
        0x05 => 0x40, // Graphics mode (256 color)
        0x06 => 0x05, // Miscellaneous
        0x07 => 0x0F, // Color don't care
        0x08 => 0xFF  // Bit mask
    );
}

/// Configures the VGA Attribute Controller registers.
fn setup_attribute_controller() {
    // Attribute Controller Register Indices
    const AC_MODE_CONTROL_1: u8 = 0x00;
    const AC_OVERSCAN_COLOR: u8 = 0x01;
    const AC_COLOR_PLANE_ENABLE: u8 = 0x02;
    const AC_HORIZONTAL_PIXEL_PANNING: u8 = 0x03;
    const AC_COLOR_SELECT: u8 = 0x04;
    const AC_MODE_CONTROL_2: u8 = 0x05;
    const AC_SCROLL: u8 = 0x06;
    const AC_GRAPHICS_MODE: u8 = 0x07;
    const AC_LINE_GRAPHICS: u8 = 0x08;
    const AC_FOREGROUND_COLOR: u8 = 0x09;
    const AC_MODE_CONTROL_256_COLORS: u8 = 0x10;
    const AC_OVERSCAN_COLOR_BORDER: u8 = 0x11;
    const AC_COLOR_PLANE_ENABLE_2: u8 = 0x12;
    const AC_HORIZONTAL_PIXEL_PANNING_2: u8 = 0x13;
    const AC_COLOR_SELECT_2: u8 = 0x14;

    const AC_VALUES: &[(u8, u8)] = &[
        (AC_MODE_CONTROL_1, 0x00),             // Mode control 1
        (AC_OVERSCAN_COLOR, 0x00),             // Overscan color
        (AC_COLOR_PLANE_ENABLE, 0x0F),         // Color plane enable
        (AC_HORIZONTAL_PIXEL_PANNING, 0x00),   // Horizontal pixel panning
        (AC_COLOR_SELECT, 0x00),               // Color select
        (AC_MODE_CONTROL_2, 0x00),             // Mode control 2
        (AC_SCROLL, 0x00),                     // Scroll
        (AC_GRAPHICS_MODE, 0x00),              // Graphics mode
        (AC_LINE_GRAPHICS, 0xFF),              // Line graphics
        (AC_FOREGROUND_COLOR, 0x00),           // Foreground color
        (AC_MODE_CONTROL_256_COLORS, 0x41),    // Mode control (for 256 colors)
        (AC_OVERSCAN_COLOR_BORDER, 0x00),      // Overscan color (border)
        (AC_COLOR_PLANE_ENABLE_2, 0x0F),       // Color plane enable
        (AC_HORIZONTAL_PIXEL_PANNING_2, 0x00), // Horizontal pixel panning
        (AC_COLOR_SELECT_2, 0x00),             // Color select
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

        dac_index_port.write(0x00u8); // Start at color index 0

        for i in 0..256 {
            // Create a simple grayscale palette (6-bit values)
            let val = (i * 63 / 255) as u8;
            dac_data_port.write(val); // Red
            dac_data_port.write(val); // Green
            dac_data_port.write(val); // Blue
        }
    }
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
