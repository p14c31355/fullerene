use alloc::boxed::Box; // Import Box
use core::fmt::{self, Write};
use core::marker::{Send, Sync};
#[cfg(not(target_os = "uefi"))]
use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig}; // Import missing types
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

use crate::font::FONT_8X8;

// A simple 8x8 PC screen font (Code Page 437).
static FONT: [[u8; 8]; 128] = FONT_8X8;

// Helper struct to reduce position update boilerplate
struct TextPosition {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl TextPosition {
    fn new(fb: &impl FramebufferLike) -> Self {
        let (x, y) = fb.get_position();
        Self {
            x,
            y,
            width: fb.get_width(),
            height: fb.get_height(),
        }
    }

    fn new_line(&mut self, fb: &mut impl FramebufferLike) {
        self.y += 8; // Font height
        self.x = 0;
        if self.y >= self.height {
            fb.scroll_up();
            self.y -= 8;
        }
        fb.set_position(self.x, self.y);
    }

    fn advance_char(&mut self, fb: &mut impl FramebufferLike) {
        self.x += 8;
        if self.x + 8 > self.width {
            self.new_line(fb);
        } else {
            fb.set_position(self.x, self.y);
        }
    }
}

/// Generic function to draw a character on any framebuffer
fn draw_char(fb: &impl FramebufferLike, c: char, x: u32, y: u32) {
    let char_idx = c as usize;
    if char_idx < 128 && c.is_ascii() {
        let font_char = &FONT[char_idx];
        let fg = fb.get_fg_color();
        let bg = fb.get_bg_color();

        for (row, &byte) in font_char.iter().enumerate() {
            for col in 0..8 {
                let color = if byte & (0x80 >> col) != 0 { fg } else { bg };
                fb.put_pixel(x + col as u32, y + row as u32, color);
            }
        }
    }
}

fn write_text<W: FramebufferLike>(writer: &mut W, s: &str) -> core::fmt::Result {
    let mut pos = TextPosition::new(writer);

    for c in s.chars() {
        if c == '\n' {
            pos.new_line(writer);
        } else {
            draw_char(writer, c, pos.x, pos.y);
            pos.advance_char(writer);
        }
    }
    Ok(())
}

/// Generic framebuffer operations (shared functions in petroleum crate)

struct ColorScheme {
    fg: u32,
    bg: u32,
}

impl ColorScheme {
    const UEFI_WHITE_ON_BLACK: Self = Self {
        fg: 0xFFFFFFu32,
        bg: 0x000000u32,
    };
    const VGA_WHITE_ON_BLACK: Self = Self {
        fg: 0x0Fu32,
        bg: 0x00u32,
    };
}

#[cfg(target_os = "uefi")]
struct FramebufferInfo {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: EfiGraphicsPixelFormat,
    colors: ColorScheme,
}

#[cfg(not(target_os = "uefi"))]
struct FramebufferInfo {
    address: u64,
    width: u32,
    height: u32,
    colors: ColorScheme,
}

#[cfg(target_os = "uefi")]
impl FramebufferInfo {
    fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        Self {
            address: fb_config.address,
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: fb_config.pixel_format,
            colors: ColorScheme::UEFI_WHITE_ON_BLACK,
        }
    }

    fn bytes_per_pixel(&self) -> u32 {
        match self.pixel_format {
            EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor
            | EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => 4,
            _ => panic!("Unsupported pixel format"),
        }
    }
}

#[cfg(not(target_os = "uefi"))]
impl FramebufferInfo {
    fn new(config: &VgaFramebufferConfig) -> Self {
        Self {
            address: config.address,
            width: config.width,
            height: config.height,
            colors: ColorScheme::VGA_WHITE_ON_BLACK,
        }
    }
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
    info: FramebufferInfo,
    x_pos: u32,
    y_pos: u32,
}

#[cfg(target_os = "uefi")]
impl FramebufferWriter {
    fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        Self {
            info: FramebufferInfo::new(fb_config),
            x_pos: 0,
            y_pos: 0,
        }
    }

    fn bytes_per_pixel(&self) -> u32 {
        self.info.bytes_per_pixel()
    }

    fn scroll_up(&self) {
        unsafe {
            scroll_buffer_pixels::<u32>(
                self.info.address,
                self.info.stride,
                self.info.height,
                self.info.colors.bg,
            );
        }
    }
}

#[cfg(not(target_os = "uefi"))]
struct FramebufferWriter {
    info: FramebufferInfo,
    x_pos: u32,
    y_pos: u32,
}

#[cfg(not(target_os = "uefi"))]
impl FramebufferWriter {
    fn new(config: &VgaFramebufferConfig) -> Self {
        Self {
            info: FramebufferInfo::new(config),
            x_pos: 0,
            y_pos: 0,
        }
    }

    fn scroll_up(&self) {
        unsafe {
            scroll_buffer_pixels::<u8>(
                self.info.address,
                self.info.width,
                self.info.height,
                self.info.colors.bg as u8,
            );
        }
    }
}

// Generic impl for both UEFI and VGA
impl FramebufferLike for FramebufferWriter {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        #[cfg(target_os = "uefi")]
        {
            let bytes_per_pixel = self.bytes_per_pixel();
            let offset = (y * self.info.stride + x) * bytes_per_pixel;
            let fb_ptr = self.info.address as *mut u8;
            unsafe {
                let pixel_ptr = fb_ptr.add(offset as usize) as *mut u32;
                *pixel_ptr = color;
            }
        }

        #[cfg(not(target_os = "uefi"))]
        {
            let offset = (y * self.info.width + x) as usize;
            unsafe {
                let fb_ptr = self.info.address as *mut u8;
                *fb_ptr.add(offset) = color as u8;
            }
        }
    }

    fn clear_screen(&self) {
        #[cfg(target_os = "uefi")]
        unsafe {
            clear_buffer_pixels::<u32>(
                self.info.address,
                self.info.width,
                self.info.height,
                self.info.colors.bg,
            );
        }

        #[cfg(not(target_os = "uefi"))]
        unsafe {
            clear_buffer_pixels::<u8>(
                self.info.address,
                self.info.width,
                self.info.height,
                self.info.colors.bg as u8,
            );
        }
    }

    fn get_width(&self) -> u32 {
        self.info.width
    }

    fn get_height(&self) -> u32 {
        self.info.height
    }

    fn get_fg_color(&self) -> u32 {
        self.info.colors.fg
    }

    fn get_bg_color(&self) -> u32 {
        self.info.colors.bg
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

impl core::fmt::Write for FramebufferWriter {
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

// VGA port addresses
struct VgaPorts;

impl VgaPorts {
    const MISC_OUTPUT: u16 = 0x3C2;
    const CRTC_INDEX: u16 = 0x3D4;
    const CRTC_DATA: u16 = 0x3D5;
    const STATUS: u16 = 0x3DA;
    const ATTRIBUTE_INDEX: u16 = 0x3C0;
    const DAC_INDEX: u16 = 0x3C8;
    const DAC_DATA: u16 = 0x3C9;
    const GRAPHICS_INDEX: u16 = 0x3CE;
    const GRAPHICS_DATA: u16 = 0x3CF;
    const SEQUENCER_INDEX: u16 = 0x3C4;
    const SEQUENCER_DATA: u16 = 0x3C5;
}

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

    let writer = FramebufferWriter::new(config);
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

// Macro was used for register setup but is now redundant due to config arrays - kept for backward compatibility

// VGA register configurations using structs for data-driven setup
struct RegisterConfig {
    index: u8,
    value: u8,
}

// Unified register configurations for data-driven setup (comments removed to reduce lines)
const SEQUENCER_CONFIG: &[RegisterConfig] = &[
    RegisterConfig { index: 0x00, value: 0x03 },
    RegisterConfig { index: 0x01, value: 0x01 },
    RegisterConfig { index: 0x02, value: 0x0F },
    RegisterConfig { index: 0x03, value: 0x00 },
    RegisterConfig { index: 0x04, value: 0x0E },
];

const CRTC_CONFIG: &[RegisterConfig] = &[
    RegisterConfig { index: 0x00, value: 0x5F },
    RegisterConfig { index: 0x01, value: 0x4F },
    RegisterConfig { index: 0x02, value: 0x50 },
    RegisterConfig { index: 0x03, value: 0x82 },
    RegisterConfig { index: 0x04, value: 0x54 },
    RegisterConfig { index: 0x05, value: 0x80 },
    RegisterConfig { index: 0x06, value: 0xBF },
    RegisterConfig { index: 0x07, value: 0x1F },
    RegisterConfig { index: 0x08, value: 0x00 },
    RegisterConfig { index: 0x09, value: 0x41 },
    RegisterConfig { index: 0x10, value: 0x9C },
    RegisterConfig { index: 0x11, value: 0x8E },
    RegisterConfig { index: 0x12, value: 0x8F },
    RegisterConfig { index: 0x13, value: 0x28 },
    RegisterConfig { index: 0x14, value: 0x40 },
    RegisterConfig { index: 0x15, value: 0x96 },
    RegisterConfig { index: 0x16, value: 0xB9 },
    RegisterConfig { index: 0x17, value: 0xA3 },
];

const GRAPHICS_CONFIG: &[RegisterConfig] = &[
    RegisterConfig { index: 0x00, value: 0x00 },
    RegisterConfig { index: 0x01, value: 0x00 },
    RegisterConfig { index: 0x02, value: 0x00 },
    RegisterConfig { index: 0x03, value: 0x00 },
    RegisterConfig { index: 0x04, value: 0x00 },
    RegisterConfig { index: 0x05, value: 0x40 },
    RegisterConfig { index: 0x06, value: 0x05 },
    RegisterConfig { index: 0x07, value: 0x0F },
    RegisterConfig { index: 0x08, value: 0xFF },
];



// Helper function to write a palette value in grayscale
fn write_palette_grayscale(val: u8) {
    unsafe {
        let mut dac_data_port: Port<u8> = Port::new(VgaPorts::DAC_DATA);
        for _ in 0..3 { // RGB
            dac_data_port.write(val);
        }
    }
}

// Macro to setup multiple registers from a config array
macro_rules! setup_registers_from_config {
    ($config:expr, $index_port:expr, $data_port:expr) => {
        for reg in $config {
            write_indexed($index_port, $data_port, reg.index, reg.value);
        }
    };
}

/// Configures the Miscellaneous Output Register.
fn setup_misc_output() {
    unsafe {
        let mut misc_output_port = Port::new(VgaPorts::MISC_OUTPUT);
        misc_output_port.write(0x63u8); // Value for enabling VGA in 320x200x256 mode
    }
}

/// Configures the VGA Sequencer registers.
fn setup_sequencer() {
    setup_registers_from_config!(
        SEQUENCER_CONFIG,
        VgaPorts::SEQUENCER_INDEX,
        VgaPorts::SEQUENCER_DATA
    );
}

/// Configures the VGA CRTC (Cathode Ray Tube Controller) registers.
fn setup_crtc() {
    setup_registers_from_config!(CRTC_CONFIG, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA);
}

/// Configures the VGA Graphics Controller registers.
fn setup_graphics_controller() {
    setup_registers_from_config!(
        GRAPHICS_CONFIG,
        VgaPorts::GRAPHICS_INDEX,
        VgaPorts::GRAPHICS_DATA
    );
}

// Attribute controller register configuration
/// Attribute controller register configuration
fn get_attribute_config() -> &'static [RegisterConfig] {
    const CONFIG: &[RegisterConfig] = &[
        RegisterConfig { index: 0x00, value: 0x00 }, // Mode control 1
        RegisterConfig { index: 0x01, value: 0x00 }, // Overscan color
        RegisterConfig { index: 0x02, value: 0x0F }, // Color plane enable
        RegisterConfig { index: 0x03, value: 0x00 }, // Horizontal pixel panning
        RegisterConfig { index: 0x04, value: 0x00 }, // Color select
        RegisterConfig { index: 0x05, value: 0x00 }, // Mode control 2
        RegisterConfig { index: 0x06, value: 0x00 }, // Scroll
        RegisterConfig { index: 0x07, value: 0x00 }, // Graphics mode
        RegisterConfig { index: 0x08, value: 0xFF }, // Line graphics
        RegisterConfig { index: 0x09, value: 0x00 }, // Foreground color
        RegisterConfig { index: 0x10, value: 0x41 }, // Mode control (for 256 colors)
        RegisterConfig { index: 0x11, value: 0x00 }, // Overscan color (border)
        RegisterConfig { index: 0x12, value: 0x0F }, // Color plane enable
        RegisterConfig { index: 0x13, value: 0x00 }, // Horizontal pixel panning
        RegisterConfig { index: 0x14, value: 0x00 }, // Color select
    ];
    CONFIG
}

/// Helper function to write to attribute registers with special sequence
fn write_attribute_registers() {
    unsafe {
        let mut status_port = Port::<u8>::new(VgaPorts::STATUS);
        let mut index_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);
        let mut data_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);

        let _ = status_port.read(); // Reset flip-flop

        for reg in get_attribute_config() {
            index_port.write(reg.index);
            data_port.write(reg.value);
        }

        index_port.write(0x20); // Enable video output
    }
}

/// Configures the VGA Attribute Controller registers.
fn setup_attribute_controller() {
    write_attribute_registers();
}

/// Sets up a simple grayscale palette for the 256-color mode.
fn setup_palette() {
    unsafe {
        let mut dac_index_port: Port<u8> = Port::new(VgaPorts::DAC_INDEX);
        dac_index_port.write(0x00u8); // Start at color index 0

        for i in 0..256 {
            let val = (i * 63 / 255) as u8;
            write_palette_grayscale(val);
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
    // Also output to VGA text buffer for reliable visibility
    if let Some(vga) = crate::vga::VGA_BUFFER.get() {
        let mut vga_writer = vga.lock();
        vga_writer.write_fmt(args).ok();
        vga_writer.update_cursor();
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
