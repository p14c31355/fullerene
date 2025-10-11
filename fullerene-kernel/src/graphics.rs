#![feature(non_exhaustive_omitted_patterns_lint)]

use petroleum::graphics::init_vga_graphics;

use alloc::boxed::Box; // Import Box
use core::fmt::{self, Write};
use core::marker::{Send, Sync};
use embedded_graphics::{
    geometry::{Point, Size},
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig}; // Import missing types
use petroleum::serial::debug_print_str_to_com1 as debug_print_str;
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};
use spin::{Mutex, Once};

// Optimized text rendering using embedded-graphics
// Batcher processing for efficiency and reduced code complexity
fn write_text<W: FramebufferLike>(
    writer: &mut W,
    s: &str,
) -> core::fmt::Result {
    const CHAR_WIDTH: i32 = FONT_6X10.character_size.width as i32;
    const CHAR_HEIGHT: i32 = FONT_6X10.character_size.height as i32;

    let fg_color = u32_to_rgb888(writer.get_fg_color());

    let style = MonoTextStyle::new(&FONT_6X10, fg_color);
    let mut lines = s.split_inclusive('\n');
    let mut current_pos = Point::new(
        writer.get_position().0 as i32,
        writer.get_position().1 as i32,
    );

    for line_with_newline in lines {
        // Handle the line (including newline if present)
        let has_newline = line_with_newline.ends_with('\n');
        let line_content = if has_newline {
            &line_with_newline[..line_with_newline.len() - 1]
        } else {
            line_with_newline
        };

        // Render the entire line at once for efficiency
        if !line_content.is_empty() {
            let text = Text::new(line_content, current_pos, style);
            text.draw(writer).ok();

            // Advance position by the rendered text width
            current_pos.x += CHAR_WIDTH * line_content.chars().count() as i32;
        }

        if has_newline {
            current_pos.x = 0;
            current_pos.y += CHAR_HEIGHT; // Font height

            // Handle scrolling if needed
            if current_pos.y + CHAR_HEIGHT > writer.get_height() as i32 {
                writer.scroll_up();
                current_pos.y -= CHAR_HEIGHT;
            }
        } else {
            // Handle line wrapping for lines without explicit newlines
            if current_pos.x >= writer.get_width() as i32 {
                current_pos.x = 0;
                current_pos.y += CHAR_HEIGHT;
                if current_pos.y + CHAR_HEIGHT > writer.get_height() as i32 {
                    writer.scroll_up();
                    current_pos.y -= CHAR_HEIGHT;
                }
            }
        }
    }

    writer.set_position(current_pos.x as u32, current_pos.y as u32);
    Ok(())
}

/// Generic framebuffer operations (shared functions in petroleum crate)

struct ColorScheme {
    fg: u32,
    bg: u32,
}

impl ColorScheme {
    const UEFI_GREEN_ON_BLACK: Self = Self {
        fg: 0x00FF00u32,
        bg: 0x000000u32,
    };
    const VGA_GREEN_ON_BLACK: Self = Self {
        fg: 0x02u32,
        bg: 0x00u32,
    };
}

fn rgb_pixel(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

fn u32_to_rgb888(color: u32) -> Rgb888 {
    Rgb888::new(
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    )
}

fn grayscale_intensity(color: Rgb888) -> u32 {
    ((color.r() as u32 * 77 + color.g() as u32 * 150 + color.b() as u32 * 29) / 256).min(255)
}

fn unsupported_pixel_format_log() {
    petroleum::serial::serial_log(format_args!("Warning: Pixel format not supported, using RGB fallback\n"));
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
            | Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor)
            | Some(EfiGraphicsPixelFormat::PixelBitMask) => 4,
            Some(EfiGraphicsPixelFormat::PixelBltOnly)
            | Some(EfiGraphicsPixelFormat::PixelFormatMax) => 0, // Invalid for direct access
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
            colors: ColorScheme::UEFI_GREEN_ON_BLACK,
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
            colors: ColorScheme::VGA_GREEN_ON_BLACK,
        }
    }
}

// Generic pixel type trait for type safety
trait PixelType: Copy + Send + Sync {
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

trait FramebufferLike: DrawTarget<Color = Rgb888, Error = core::convert::Infallible> + Send + Sync {
    fn put_pixel(&self, x: u32, y: u32, color: u32);
    fn clear_screen(&self);
    fn get_width(&self) -> u32;
    fn get_height(&self) -> u32;
    fn get_fg_color(&self) -> u32;
    fn get_bg_color(&self) -> u32;
    fn set_position(&mut self, x: u32, y: u32);
    fn get_position(&self) -> (u32, u32);
    fn scroll_up(&self);
    fn get_stride(&self) -> u32;
    fn is_vga(&self) -> bool;
}

enum UefiFramebuffer {
    Uefi32(FramebufferWriter<u32>),
    Vga8(FramebufferWriter<u8>),
}

impl DrawTarget for UefiFramebuffer {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.draw_iter(pixels),
            UefiFramebuffer::Vga8(fb) => fb.draw_iter(pixels),
        }
    }
}

impl OriginDimensions for UefiFramebuffer {
    fn size(&self) -> Size {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.size(),
            UefiFramebuffer::Vga8(fb) => fb.size(),
        }
    }
}

impl FramebufferLike for UefiFramebuffer {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.put_pixel(x, y, color),
            UefiFramebuffer::Vga8(fb) => fb.put_pixel(x, y, color),
        }
    }

    fn clear_screen(&self) {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.clear_screen(),
            UefiFramebuffer::Vga8(fb) => fb.clear_screen(),
        }
    }

    fn get_width(&self) -> u32 {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_width(),
            UefiFramebuffer::Vga8(fb) => fb.get_width(),
        }
    }

    fn get_height(&self) -> u32 {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_height(),
            UefiFramebuffer::Vga8(fb) => fb.get_height(),
        }
    }

    fn get_fg_color(&self) -> u32 {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_fg_color(),
            UefiFramebuffer::Vga8(fb) => fb.get_fg_color(),
        }
    }

    fn get_bg_color(&self) -> u32 {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_bg_color(),
            UefiFramebuffer::Vga8(fb) => fb.get_bg_color(),
        }
    }

    fn set_position(&mut self, x: u32, y: u32) {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.set_position(x, y),
            UefiFramebuffer::Vga8(fb) => fb.set_position(x, y),
        }
    }

    fn get_position(&self) -> (u32, u32) {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_position(),
            UefiFramebuffer::Vga8(fb) => fb.get_position(),
        }
    }

    fn scroll_up(&self) {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.scroll_up(),
            UefiFramebuffer::Vga8(fb) => fb.scroll_up(),
        }
    }

    fn get_stride(&self) -> u32 {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.get_stride(),
            UefiFramebuffer::Vga8(fb) => fb.get_stride(),
        }
    }

    fn is_vga(&self) -> bool {
        match self {
            UefiFramebuffer::Uefi32(fb) => fb.is_vga(),
            UefiFramebuffer::Vga8(fb) => fb.is_vga(),
        }
    }
}

struct FramebufferWriter<T: PixelType> {
    info: FramebufferInfo,
    x_pos: u32,
    y_pos: u32,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: PixelType> DrawTarget for FramebufferWriter<T> {
    type Color = Rgb888;

    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            if coord.x >= 0 && coord.y >= 0 {
                let x = coord.x as u32;
                let y = coord.y as u32;
                if x < self.info.width && y < self.info.height {
                    let pixel_color = self.rgb888_to_pixel_format(color);
                    self.put_pixel(x, y, pixel_color);
                }
            }
        }
        Ok(())
    }
}

impl<T: PixelType> OriginDimensions for FramebufferWriter<T> {
    fn size(&self) -> Size {
        Size::new(self.info.width, self.info.height)
    }
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

        fn rgb888_to_pixel_format(&self, color: Rgb888) -> u32 {
        let rgb = || rgb_pixel(color.r(), color.g(), color.b());
        let bgr = || rgb_pixel(color.b(), color.g(), color.r());
        #[allow(non_exhaustive_omitted_patterns)]
        match self.info.pixel_format {
            Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor) => rgb(),
            Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor) => bgr(),
            // Cirrus VGA commonly reports PixelBitMask but expects RGB format
            Some(EfiGraphicsPixelFormat::PixelBitMask) |
            Some(_) => rgb(), // Treat all unknown formats as RGB
            None => grayscale_intensity(color), // UEFI mode shouldn't be using grayscale
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
        unsafe {
            scroll_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
            );
        }
    }

    fn get_stride(&self) -> u32 {
        self.info.stride
    }

    fn is_vga(&self) -> bool {
        self.info.pixel_format.is_none()
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

#[cfg(target_os = "uefi")]
pub static FRAMEBUFFER_UEFI: Once<Mutex<UefiFramebuffer>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static FRAMEBUFFER_BIOS: Once<Mutex<FramebufferWriter<u8>>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    petroleum::serial::serial_log(format_args!("Graphics: Initializing UEFI framebuffer: {}x{}, stride: {}, pixel_format: {:?}\n",
        config.width, config.height, config.stride, config.pixel_format));
    let writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
    let fb_writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
    FRAMEBUFFER_UEFI.call_once(|| Mutex::new(UefiFramebuffer::Uefi32(fb_writer)));
}

// VgaPorts is imported from petroleum

/// Initializes VGA graphics mode 13h (320x200, 256 colors).
///
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga(config: &VgaFramebufferConfig) {
    init_vga_graphics(); // Use petroleum function

    let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config));
    writer.clear_screen();
    #[cfg(target_os = "uefi")]
    {
        WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
        FRAMEBUFFER_UEFI.call_once(|| Mutex::new(UefiFramebuffer::Vga8(FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config)))));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let text_writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config));
        let fb_writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config));
        WRITER_BIOS.call_once(|| Mutex::new(Box::new(text_writer)));
        FRAMEBUFFER_BIOS.call_once(|| Mutex::new(fb_writer));
    }
}

// All VGA setup is handled by petroleum's init_vga_graphics

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

// Draw OS-like desktop interface
#[cfg(target_os = "uefi")]
pub fn draw_os_desktop() {
    print!("Graphics: draw_os_desktop() called\n");
    debug_print_str("Graphics: draw_os_desktop() started\n");
    debug_print_str("Graphics: checking UEFI framebuffer...\n");
    if let Some(fb_writer) = FRAMEBUFFER_UEFI.get() {
        debug_print_str("Graphics: Obtained UEFI framebuffer writer\n");
        let mut locked = fb_writer.lock();
        debug_print_str("Graphics: Framebuffer writer locked\n");
        draw_desktop_internal(&mut *locked, "UEFI");
    } else {
        print!("Graphics: ERROR - FRAMEBUFFER_UEFI not initialized\n");
        debug_print_str("Graphics: ERROR - FRAMEBUFFER_UEFI not initialized\n");
    }
}

#[cfg(not(target_os = "uefi"))]
pub fn draw_os_desktop() {
    print!("Graphics: draw_os_desktop() called in BIOS mode\n");
    debug_print_str("Graphics: BIOS mode draw_os_desktop() started\n");
    debug_print_str("Graphics: checking BIOS framebuffer...\n");
    if let Some(fb_writer) = FRAMEBUFFER_BIOS.get() {
        debug_print_str("Graphics: Obtained BIOS framebuffer writer\n");
        let mut locked = fb_writer.lock();
        debug_print_str("Graphics: Framebuffer writer locked\n");
        draw_desktop_internal(&mut *locked, "BIOS");
    } else {
        print!("Graphics: ERROR - BIOS framebuffer not initialized\n");
        debug_print_str("Graphics: ERROR - BIOS framebuffer not initialized\n");
    }
}

fn draw_desktop_internal(fb_writer: &mut impl FramebufferLike, mode: &str) {
    let is_vga = fb_writer.is_vga();
    if is_vga {
        petroleum::serial::serial_log(format_args!("Graphics: Framebuffer size: {}x{}, VGA mode\n", fb_writer.get_width(), fb_writer.get_height()));
    } else {
        petroleum::serial::serial_log(format_args!("Graphics: Framebuffer size: {}x{}, stride: {}\n", fb_writer.get_width(), fb_writer.get_height(), fb_writer.get_stride()));
    }
    let bg_color = 32u32; // Dark gray
    debug_print_str("Graphics: Filling background...\n");
    fill_background(fb_writer, bg_color);
    debug_print_str("Graphics: Background filled\n");
    debug_print_str("Graphics: Drawing test red rectangle...\n");
    draw_window(fb_writer, 10, 10, 50, 50, 0xFF0000u32, 0xFFFFFFu32);
    debug_print_str("Graphics: Test red rectangle drawn\n");
    debug_print_str("Graphics: Drawing window frame...\n");
    draw_window(fb_writer, 50, 50, 220, 120, 255u32, 64u32);
    debug_print_str("Graphics: Window frame drawn\n");
    debug_print_str("Graphics: Drawing taskbar...\n");
    draw_taskbar(fb_writer, 128u32);
    debug_print_str("Graphics: Taskbar drawn\n");
    debug_print_str("Graphics: Drawing icons...\n");
    draw_icon(fb_writer, 65, 60, "Terminal", 96u32);
    draw_icon(fb_writer, 65, 80, "Settings", 160u32);
    debug_print_str("Graphics: Icons drawn\n");
    print!("Graphics: {} desktop drawing completed\n", mode);
    debug_print_str("Graphics: desktop drawing completed\n");
}

fn fill_background(writer: &mut impl FramebufferLike, color: u32) {
    let color_rgb = u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(Point::new(0, 0), Size::new(writer.get_width(), writer.get_height()));
    rect.into_styled(style).draw(writer).ok();
}

fn draw_window<W: FramebufferLike>(
    writer: &mut W,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    bg_color: u32,
    border_color: u32,
) {
    let bg_rgb = u32_to_rgb888(bg_color);
    let border_rgb = u32_to_rgb888(border_color);
    let style = PrimitiveStyleBuilder::new()
        .fill_color(bg_rgb)
        .stroke_color(border_rgb)
        .stroke_width(1)
        .build();
    let rect = Rectangle::new(Point::new(x as i32, y as i32), Size::new(w, h));
    rect.into_styled(style).draw(writer).ok();
}

fn draw_taskbar<W: FramebufferLike>(writer: &mut W, color: u32) {
    let height = writer.get_height();
    let taskbar_height = 40;

    let color_rgb = u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(Point::new(0, (height - taskbar_height) as i32), Size::new(writer.get_width(), taskbar_height));
    rect.into_styled(style).draw(writer).ok();

    // Simple start button
    draw_window(writer, 0, height - taskbar_height + 5, 80, 30, 0xE0E0E0u32, 0x000000u32);
}

fn draw_icon<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, _label: &str, color: u32) {
    const ICON_SIZE: u32 = 48;
    let color_rgb = u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(Point::new(x as i32, y as i32), Size::new(ICON_SIZE, ICON_SIZE));
    rect.into_styled(style).draw(writer).ok();
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
