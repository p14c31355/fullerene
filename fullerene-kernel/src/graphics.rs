// Generic port writer struct to reduce unsafe block repetition and improve type safety
pub struct PortWriter<T> {
    port: Port<T>,
}

impl<T> PortWriter<T> {
    pub fn new(port_addr: u16) -> Self {
        Self {
            port: Port::new(port_addr),
        }
    }

    pub unsafe fn write(&mut self, value: T)
    where
        T: x86_64::instructions::port::PortWrite,
    {
        self.port.write(value);
    }

    pub unsafe fn read(&mut self) -> T
    where
        T: x86_64::instructions::port::PortRead,
    {
        self.port.read()
    }
}

// Specialized VGA port operations
struct VgaPortOps {
    index_writer: PortWriter<u8>,
    data_writer: PortWriter<u8>,
}

impl VgaPortOps {
    fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_writer: PortWriter::new(index_port),
            data_writer: PortWriter::new(data_port),
        }
    }

    fn write_register(&mut self, index: u8, value: u8) {
        unsafe {
            self.index_writer.write(index);
            self.data_writer.write(value);
        }
    }

    fn write_sequence(&mut self, configs: &[RegisterConfig]) {
        for reg in configs {
            self.write_register(reg.index, reg.value);
        }
    }
}

// Enhanced macro for writing port sequences with automatic port management
macro_rules! write_port_sequence {
    ($($config:expr, $index_port:expr, $data_port:expr);*$(;)?) => {{
        $(
            let mut vga_ports = VgaPortOps::new($index_port, $data_port);
            vga_ports.write_sequence($config);
        )*
    }};
}

// Simplified macro for single register writes
macro_rules! write_vga_register {
    ($index_port:expr, $data_port:expr, $index:expr, $data:expr) => {{
        let mut vga_ports = VgaPortOps::new($index_port, $data_port);
        vga_ports.write_register($index, $data);
    }};
}

// Helper functions for color operations

use alloc::boxed::Box; // Import Box
use core::fmt::{self, Write};
use core::marker::{Send, Sync};
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

struct FramebufferInfo {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: Option<EfiGraphicsPixelFormat>,
    colors: ColorScheme,
}

impl FramebufferInfo {
    fn width_or_stride(&self) -> u32 {
        #[cfg(target_os = "uefi")]
        {
            self.stride
        }
        #[cfg(not(target_os = "uefi"))]
        {
            self.width
        }
    }

    fn calculate_offset(&self, x: u32, y: u32) -> usize {
        #[cfg(target_os = "uefi")]
        {
            ((y * self.stride + x) * self.bytes_per_pixel()) as usize
        }
        #[cfg(not(target_os = "uefi"))]
        {
            ((y * self.width + x) * 1) as usize
        } // 1 byte per pixel for VGA
    }

    fn bytes_per_pixel(&self) -> u32 {
        match self.pixel_format {
            Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor)
            | Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor) => 4,
            Some(_) => panic!("Unsupported pixel format"),
            None => 1, // VGA
        }
    }
}

#[cfg(target_os = "uefi")]
impl FramebufferInfo {
    fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        Self {
            address: fb_config.address,
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: ColorScheme::UEFI_WHITE_ON_BLACK,
        }
    }
}

impl FramebufferInfo {
    fn new_vga(config: &VgaFramebufferConfig) -> Self {
        Self {
            address: config.address,
            width: config.width,
            height: config.height,
            stride: config.width,
            pixel_format: None,
            colors: ColorScheme::VGA_WHITE_ON_BLACK,
        }
    }
}

// Generic pixel type trait for type safety
trait PixelType: Copy {
    fn bytes_per_pixel() -> u32;
    fn from_u32(color: u32) -> Self;
    fn to_generic(color: u32) -> u32;
}

impl PixelType for u32 {
    fn bytes_per_pixel() -> u32 {
        4
    }
    fn from_u32(color: u32) -> Self {
        color
    }
    fn to_generic(color: u32) -> u32 {
        color
    }
}

impl PixelType for u8 {
    fn bytes_per_pixel() -> u32 {
        1
    }
    fn from_u32(color: u32) -> Self {
        color as u8
    }
    fn to_generic(color: u32) -> u32 {
        color & 0xFF
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

struct FramebufferWriter<T: PixelType> {
    info: FramebufferInfo,
    x_pos: u32,
    y_pos: u32,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: PixelType> FramebufferWriter<T> {
    fn new(info: FramebufferInfo) -> Self {
        Self {
            info,
            x_pos: 0,
            y_pos: 0,
            _phantom: core::marker::PhantomData,
        }
    }

    fn scroll_up(&self) {
        unsafe {
            scroll_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
            );
        }
    }
}

impl<T: PixelType> FramebufferLike for FramebufferWriter<T> {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let offset = self.info.calculate_offset(x, y);
        unsafe {
            let fb_ptr = self.info.address as *mut u8;
            let pixel_ptr = fb_ptr.add(offset) as *mut T;
            *pixel_ptr = T::from_u32(color);
        }
    }

    fn clear_screen(&self) {
        unsafe {
            clear_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
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

// Convenience type aliases
type UefiFramebufferWriter = FramebufferWriter<u32>;
type VgaFramebufferWriter = FramebufferWriter<u8>;

impl<T: PixelType> core::fmt::Write for FramebufferWriter<T> {
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
    let writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
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

/// Initializes VGA graphics mode 13h (320x200, 256 colors).
///
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga(config: &VgaFramebufferConfig) {
    setup_misc_output();
    setup_registers_from_configs(); // Consolidated setup for sequencer, crtc, graphics
    setup_attribute_controller();
    setup_palette();

    let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config));
    writer.clear_screen();
    #[cfg(target_os = "uefi")]
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
    #[cfg(not(target_os = "uefi"))]
    WRITER_BIOS.call_once(|| Mutex::new(Box::new(writer)));
}

// Macro was used for register setup but is now redundant due to config arrays - kept for backward compatibility

// VGA register configurations using structs for data-driven setup
struct RegisterConfig {
    index: u8,
    value: u8,
}

// VGA Sequencer registers configuration for mode 13h (320x200, 256 colors)
// These control the timing and memory access for the VGA sequencer
const SEQUENCER_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x03,
    }, // Reset register - synchronous reset
    RegisterConfig {
        index: 0x01,
        value: 0x01,
    }, // Clocking mode register - 8/9 dot clocks, use 9th bit in text mode (not applicable in graphics mode)
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Map mask register - enable all planes (15)
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Character map select register - select character sets (not used in graphics mode)
    RegisterConfig {
        index: 0x04,
        value: 0x0E,
    }, // Memory mode register - enable ext memory, enable odd/even in chain 4, use chain 4 addressing mode
];

// VGA CRTC (Cathode Ray Tube Controller) registers configuration for mode 13h
// These control horizontal/vertical synchronization, display size, and timing
const CRTC_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x5F,
    }, // Horizontal total (horizontal display end)
    RegisterConfig {
        index: 0x01,
        value: 0x4F,
    }, // Horizontal display enable end
    RegisterConfig {
        index: 0x02,
        value: 0x50,
    }, // Start horizontal blanking
    RegisterConfig {
        index: 0x03,
        value: 0x82,
    }, // End horizontal blanking
    RegisterConfig {
        index: 0x04,
        value: 0x54,
    }, // Start horizontal retrace pulse
    RegisterConfig {
        index: 0x05,
        value: 0x80,
    }, // End horizontal retrace
    RegisterConfig {
        index: 0x06,
        value: 0xBF,
    }, // Vertical total (vertical display end)
    RegisterConfig {
        index: 0x07,
        value: 0x1F,
    }, // Overflow
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Preset row scan
    RegisterConfig {
        index: 0x09,
        value: 0x41,
    }, // Maximum scan line
    RegisterConfig {
        index: 0x10,
        value: 0x9C,
    }, // Start vertical retrace
    RegisterConfig {
        index: 0x11,
        value: 0x8E,
    }, // End vertical retrace
    RegisterConfig {
        index: 0x12,
        value: 0x8F,
    }, // Vertical display enable end
    RegisterConfig {
        index: 0x13,
        value: 0x28,
    }, // Offset (line offset/logical width - number of bytes per scan line)
    RegisterConfig {
        index: 0x14,
        value: 0x40,
    }, // Underline location
    RegisterConfig {
        index: 0x15,
        value: 0x96,
    }, // Start vertical blanking
    RegisterConfig {
        index: 0x16,
        value: 0xB9,
    }, // End vertical blanking
    RegisterConfig {
        index: 0x17,
        value: 0xA3,
    }, // CRTC mode control
];

// VGA Graphics Controller registers configuration for mode 13h
// These control how graphics memory is mapped and accessed
const GRAPHICS_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Set/reset register - reset all bits
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Enable set/reset register - disable
    RegisterConfig {
        index: 0x02,
        value: 0x00,
    }, // Color compare register - compare mode
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Data rotate register - no rotate
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Read plane select register - select plane 0
    RegisterConfig {
        index: 0x05,
        value: 0x40,
    }, // Graphics mode register - chain odd/even planes, read mode 0, write mode 0, read plane 0
    RegisterConfig {
        index: 0x06,
        value: 0x05,
    }, // Miscellaneous register - memory map mode A0000-AFFFF (64KB), alphanumerics/text mode disabled, chain odd/even planes disabled
    RegisterConfig {
        index: 0x07,
        value: 0x0F,
    }, // Color don't care register - care about all bits
    RegisterConfig {
        index: 0x08,
        value: 0xFF,
    }, // Bit mask register - enable all bits
];

// Helper function to write a palette value in grayscale
fn write_palette_grayscale(val: u8) {
    unsafe {
        let mut dac_data_port: Port<u8> = Port::new(VgaPorts::DAC_DATA);
        for _ in 0..3 {
            // RGB
            dac_data_port.write(val);
        }
    }
}

/// Configures the Miscellaneous Output Register.
fn setup_misc_output() {
    unsafe {
        let mut misc_output_port = Port::new(VgaPorts::MISC_OUTPUT);
        misc_output_port.write(0x63u8); // Value for enabling VGA in 320x200x256 mode
    }
}

/// Configures the VGA registers using the new macro
fn setup_registers_from_configs() {
    write_port_sequence!(
        SEQUENCER_CONFIG, VgaPorts::SEQUENCER_INDEX, VgaPorts::SEQUENCER_DATA;
        CRTC_CONFIG, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA;
        GRAPHICS_CONFIG, VgaPorts::GRAPHICS_INDEX, VgaPorts::GRAPHICS_DATA
    );
}



// VGA Attribute Controller registers configuration for mode 13h
// These control color mapping and screen display attributes
const ATTRIBUTE_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Palette register 0 (red|green|blue|intensity)
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Palette register 1
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Palette register 2
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Palette register 3
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Palette register 4
    RegisterConfig {
        index: 0x05,
        value: 0x00,
    }, // Palette register 5
    RegisterConfig {
        index: 0x06,
        value: 0x00,
    }, // Palette register 6
    RegisterConfig {
        index: 0x07,
        value: 0x00,
    }, // Palette register 7
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Palette register 8
    RegisterConfig {
        index: 0x09,
        value: 0x00,
    }, // Palette register 9
    RegisterConfig {
        index: 0x0A,
        value: 0x00,
    }, // Palette register A
    RegisterConfig {
        index: 0x0B,
        value: 0x00,
    }, // Palette register B
    RegisterConfig {
        index: 0x0C,
        value: 0x00,
    }, // Palette register C
    RegisterConfig {
        index: 0x0D,
        value: 0x00,
    }, // Palette register D
    RegisterConfig {
        index: 0x0E,
        value: 0x00,
    }, // Palette register E
    RegisterConfig {
        index: 0x0F,
        value: 0x00,
    }, // Palette register F
    RegisterConfig {
        index: 0x10,
        value: 0x41,
    }, // Attr mode control register - enable 256-color mode, enable graphics mode
    RegisterConfig {
        index: 0x11,
        value: 0x00,
    }, // Overscan color register - border color (black)
    RegisterConfig {
        index: 0x12,
        value: 0x0F,
    }, // Color plane enable register - enable all planes
    RegisterConfig {
        index: 0x13,
        value: 0x00,
    }, // Horizontal pixel panning register - no panning
    RegisterConfig {
        index: 0x14,
        value: 0x00,
    }, // Color select register - no color select
];

/// Helper function to write to attribute registers with special sequence
fn write_attribute_registers() {
    unsafe {
        let mut status_port = Port::<u8>::new(VgaPorts::STATUS);
        let mut index_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);
        let mut data_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);

        let _ = status_port.read(); // Reset flip-flop

        for reg in ATTRIBUTE_CONFIG {
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
    write_vga_register!(VgaPorts::DAC_INDEX, VgaPorts::DAC_DATA, 0x00, 0x00); // Start at color index 0

    for i in 0..256 {
        let val = (i * 63 / 255) as u8;
        write_palette_grayscale(val);
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
