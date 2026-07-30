use crate::graphics::color::{FramebufferInfo, PixelType, rgb_pixel};
use embedded_graphics::{
    geometry::{Point, Size},
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::Rgb888,
    prelude::*,
    text::Text,
};
use sealant::{FramebufferRegion, Permissions, VolatileRead};

// Helper macro for delegate calls to reduce duplication
macro_rules! delegate_call {
    ($self:expr, $method:ident $(, $args:expr)*) => {
        match $self {
            UefiFramebuffer::Uefi32(fb) => fb.$method($($args),*),
            UefiFramebuffer::Vga8(fb) => fb.$method($($args),*),
        }
    };
}

pub trait FramebufferLike:
    DrawTarget<Color = Rgb888, Error = core::convert::Infallible> + Send + Sync
{
    fn put_pixel(&self, x: u32, y: u32, color: u32);
    /// Fill a rectangle by writing color to each pixel directly into the framebuffer.
    /// Default implementation calls put_pixel per pixel; backends should override with
    /// a bulk-memory fill for performance.
    fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        for dy in 0..height {
            let row = y + dy;
            if row >= self.get_height() {
                break;
            }
            for dx in 0..width {
                let col = x + dx;
                if col >= self.get_width() {
                    break;
                }
                self.put_pixel(col, row, color);
            }
        }
    }
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

#[derive(Clone)]
pub enum UefiFramebufferWriter {
    Uefi32(FramebufferWriter<u32>),
    Vga8(FramebufferWriter<u8>),
}

pub type UefiWriterMutex = spin::Mutex<UefiFramebufferWriter>;

pub fn create_uefi_writer_mutex(writer: UefiFramebufferWriter) -> UefiWriterMutex {
    spin::Mutex::new(writer)
}

impl core::fmt::Write for UefiFramebufferWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.write_str(s),
            UefiFramebufferWriter::Vga8(w) => w.write_str(s),
        }
    }
}

impl crate::graphics::Console for UefiFramebufferWriter {
    fn write_char(&mut self, c: char, color: u32) {
        use embedded_graphics::{
            mono_font::{MonoTextStyle, ascii::FONT_6X10},
            prelude::*,
            text::Text,
        };

        let style = MonoTextStyle::new(&FONT_6X10, crate::graphics::color::u32_to_rgb888(color));

        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        let s_str = unsafe { core::str::from_utf8_unchecked(&s.as_bytes()) };

        match self {
            UefiFramebufferWriter::Uefi32(w) => {
                let pos = Point::new(w.get_position().0 as i32, w.get_position().1 as i32);
                let _ = Text::new(s_str, pos, style).draw(w);
                w.set_position(w.get_position().0 + 6, w.get_position().1);
            }
            UefiFramebufferWriter::Vga8(w) => {
                let pos = Point::new(w.get_position().0 as i32, w.get_position().1 as i32);
                let _ = Text::new(s_str, pos, style).draw(w);
                w.set_position(w.get_position().0 + 6, w.get_position().1);
            }
        }
    }

    fn clear(&mut self) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.clear_screen(),
            UefiFramebufferWriter::Vga8(w) => w.clear_screen(),
        }
    }

    fn set_cursor(&mut self, x: usize, y: usize) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.set_position(x as u32, y as u32),
            UefiFramebufferWriter::Vga8(w) => w.set_position(x as u32, y as u32),
        }
    }

    fn scroll(&mut self) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.scroll_up(),
            UefiFramebufferWriter::Vga8(w) => w.scroll_up(),
        }
    }

    fn set_color(&mut self, color: u32) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.current_color = color,
            UefiFramebufferWriter::Vga8(w) => w.current_color = color,
        }
    }
}

impl UefiFramebufferWriter {
    pub fn get_info(&self) -> &FramebufferInfo {
        match self {
            UefiFramebufferWriter::Uefi32(w) => &w.info,
            UefiFramebufferWriter::Vga8(w) => &w.info,
        }
    }

    pub fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.fill_rect(x, y, width, height, color),
            UefiFramebufferWriter::Vga8(w) => w.fill_rect(x, y, width, height, color),
        }
    }
}

impl crate::graphics::Renderer for UefiFramebufferWriter {
    fn draw_pixel(&mut self, x: i32, y: i32, color: u32) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => w.put_pixel(x as u32, y as u32, color),
            UefiFramebufferWriter::Vga8(w) => w.put_pixel(x as u32, y as u32, color),
        }
    }

    fn draw_rect(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32) {
        self.fill_rect(x.max(0) as u32, y.max(0) as u32, width, height, color);
    }

    fn draw_text(&mut self, x: i32, y: i32, text: &str, color: u32) {
        use embedded_graphics::{
            mono_font::{MonoTextStyle, ascii::FONT_6X10},
            prelude::*,
            text::Text,
        };
        let style = MonoTextStyle::new(&FONT_6X10, crate::graphics::color::u32_to_rgb888(color));
        let pos = Point::new(x, y);
        match self {
            UefiFramebufferWriter::Uefi32(w) => {
                let _ = Text::new(text, pos, style).draw(w);
            }
            UefiFramebufferWriter::Vga8(w) => {
                let _ = Text::new(text, pos, style).draw(w);
            }
        }
    }

    fn clear(&mut self, color: u32) {
        // FramebufferWriter::clear_screen uses internal bg color.
        // To clear with a specific color, we draw a large rectangle.
        let (w, h) = self.get_resolution();
        self.draw_rect(0, 0, w, h, color);
    }

    fn get_resolution(&self) -> (u32, u32) {
        match self {
            UefiFramebufferWriter::Uefi32(w) => (w.get_width(), w.get_height()),
            UefiFramebufferWriter::Vga8(w) => (w.get_width(), w.get_height()),
        }
    }
}

#[derive(Clone)]
pub enum UefiFramebuffer {
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
        delegate_call!(self, put_pixel, x, y, color);
    }

    fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        delegate_call!(self, fill_rect, x, y, width, height, color);
    }

    fn clear_screen(&self) {
        delegate_call!(self, clear_screen);
    }

    fn get_width(&self) -> u32 {
        delegate_call!(self, get_width)
    }

    fn get_height(&self) -> u32 {
        delegate_call!(self, get_height)
    }

    fn get_fg_color(&self) -> u32 {
        delegate_call!(self, get_fg_color)
    }

    fn get_bg_color(&self) -> u32 {
        delegate_call!(self, get_bg_color)
    }

    fn set_position(&mut self, x: u32, y: u32) {
        delegate_call!(self, set_position, x, y);
    }

    fn get_position(&self) -> (u32, u32) {
        delegate_call!(self, get_position)
    }

    fn scroll_up(&self) {
        delegate_call!(self, scroll_up);
    }

    fn get_stride(&self) -> u32 {
        delegate_call!(self, get_stride)
    }

    fn is_vga(&self) -> bool {
        delegate_call!(self, is_vga)
    }
}

#[derive(Clone)]
pub struct FramebufferWriter<T: PixelType> {
    pub info: FramebufferInfo,
    framebuffer: FramebufferRegion<'static>,
    x_pos: u32,
    y_pos: u32,
    pub current_color: u32,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: PixelType + VolatileRead> DrawTarget for FramebufferWriter<T> {
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
    /// Construct a writer for a directly mapped framebuffer.
    ///
    /// # Safety
    ///
    /// `info.address..info.address + info.stride * info.height` must be a
    /// mapped, writable framebuffer for the lifetime of the returned writer.
    pub unsafe fn new(info: FramebufferInfo) -> Self {
        let framebuffer_size = (info.stride as usize)
            .checked_mul(info.height as usize)
            .expect("framebuffer size overflow");
        let framebuffer = unsafe {
            FramebufferRegion::from_address(
                info.address as usize,
                framebuffer_size,
                Permissions::READ_WRITE,
            )
            .expect("invalid framebuffer region")
        };
        Self {
            current_color: info.colors.fg,
            info,
            framebuffer,
            x_pos: 0,
            y_pos: 0,
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn rgb888_to_pixel_format(&self, color: Rgb888) -> u32 {
        // Map Rgb888 to the u32 value that produces correct bytes in
        // little-endian framebuffer memory for the given pixel format.
        //
        // rgb_pixel(r,g,b) = (r<<16)|(g<<8)|b
        //   → LE memory: [b, g, r, 0]
        //   → BGR hardware (byte0=B): B=b, G=g, R=r  ✓
        //
        // rgb_pixel(b,g,r) = (b<<16)|(g<<8)|r
        //   → LE memory: [r, g, b, 0]
        //   → RGB hardware (byte0=R): R=r, G=g, B=b  ✓
        if let Some(format) = self.info.pixel_format {
            match format {
                // BGR format: byte0=Blue, byte1=Green, byte2=Red
                crate::common::EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor
                // PixelBitMask on Intel GOP is almost always BGR byte order.
                | crate::common::EfiGraphicsPixelFormat::PixelBitMask => {
                    rgb_pixel(color.r(), color.g(), color.b())
                }
                // RGB format: byte0=Red, byte1=Green, byte2=Blue
                _ => {
                    rgb_pixel(color.b(), color.g(), color.r())
                }
            }
        } else {
            // No format specified (e.g. VGA), assume BGR (most common on UEFI)
            rgb_pixel(color.r(), color.g(), color.b())
        }
    }
}

// Text rendering function for framebuffers
fn write_text<W: FramebufferLike>(writer: &mut W, s: &str) -> core::fmt::Result {
    const CHAR_WIDTH: i32 = FONT_6X10.character_size.width as i32;
    const CHAR_HEIGHT: i32 = FONT_6X10.character_size.height as i32;

    let fg_color = crate::graphics::color::u32_to_rgb888(writer.get_fg_color());

    let style = MonoTextStyle::new(&FONT_6X10, fg_color);
    let lines = s.split_inclusive('\n');
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

impl<T: PixelType + VolatileRead> core::fmt::Write for FramebufferWriter<T> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
    }
}

impl<T: PixelType + VolatileRead> FramebufferLike for FramebufferWriter<T> {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let offset = self.info.calculate_offset(x, y);
        let _ = self
            .framebuffer
            .write_volatile_at(offset, T::from_u32(color));
        // Force memory barrier to ensure write is visible to the display controller
        unsafe { core::arch::x86_64::_mm_sfence() };
    }

    /// Optimised bulk fill: writes `color` into every pixel of the rectangle
    /// using aligned `T`-sized stores, one scan line at a time.
    fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let x_end = x.saturating_add(width).min(self.info.width);
        let y_end = y.saturating_add(height).min(self.info.height);
        if x >= x_end || y >= y_end {
            return;
        }
        let pixel_val = T::from_u32(color);
        let bpp = core::mem::size_of::<T>() as u32;
        for row in y..y_end {
            let row_base = (row as usize * self.info.stride as usize) + (x as usize * bpp as usize);
            let count = (x_end - x) as usize;
            for col in 0..count {
                let _ = self
                    .framebuffer
                    .write_volatile_at(row_base + col * bpp as usize, pixel_val);
            }
        }
    }

    fn clear_screen(&self) {
        let bpp = core::mem::size_of::<T>();
        let count = (self.info.stride as usize / bpp).saturating_mul(self.info.height as usize);
        let value = T::from_u32(self.info.colors.bg);
        for index in 0..count {
            let _ = self
                .framebuffer
                .write_volatile_at(index.saturating_mul(bpp), value);
        }
    }

    fn get_width(&self) -> u32 {
        self.info.width
    }
    fn get_height(&self) -> u32 {
        self.info.height
    }
    fn get_fg_color(&self) -> u32 {
        self.current_color
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
        let bpp = core::mem::size_of::<T>();
        let pixels_per_line = self.info.stride as usize / bpp;
        // One text row is FONT_6X10.height (10 px). Shift and clear must use
        // the same height, otherwise a stale band is left between them.
        let row_pixels = 10usize.saturating_mul(pixels_per_line);
        let total_pixels = pixels_per_line.saturating_mul(self.info.height as usize);
        for index in 0..total_pixels.saturating_sub(row_pixels) {
            let source = (row_pixels + index).saturating_mul(bpp);
            let destination = index.saturating_mul(bpp);
            let value = self
                .framebuffer
                .read_volatile_at(source)
                .unwrap_or(T::from_u32(self.info.colors.bg));
            let _ = self.framebuffer.write_volatile_at(destination, value);
        }
        let clear_start = self.info.height.saturating_sub(10) as usize * pixels_per_line;
        let clear_count = row_pixels;
        let value = T::from_u32(self.info.colors.bg);
        for index in 0..clear_count {
            let _ = self
                .framebuffer
                .write_volatile_at((clear_start + index).saturating_mul(bpp), value);
        }
    }

    fn get_stride(&self) -> u32 {
        self.info.stride
    }

    fn is_vga(&self) -> bool {
        self.info.pixel_format.is_none()
    }
}

/// Generic framebuffer buffer clear operation
pub unsafe fn clear_buffer_pixels<T: Copy>(address: u64, stride: u32, height: u32, bg_color: T) {
    let bytes_per_pixel = core::mem::size_of::<T>() as u32;
    let elements_per_line = (stride / bytes_per_pixel) as usize;
    let count = elements_per_line * height as usize;
    let region = unsafe {
        FramebufferRegion::from_address(
            address as usize,
            (stride as usize) * (height as usize),
            Permissions::READ_WRITE,
        )
        .expect("invalid framebuffer region")
    };
    for i in 0..count {
        let _ = region.write_volatile_at(i * core::mem::size_of::<T>(), bg_color);
    }
}

/// Generic framebuffer buffer scroll up operation.
///
/// Shifts the entire framebuffer up by one text row (10 scan lines, matching
/// FONT_6X10) using `T`-sized volatile accesses (much fewer operations than
/// byte-by-byte). The freed 10 scan lines are filled with `bg_color`.
pub unsafe fn scroll_buffer_pixels<T: Copy + VolatileRead>(
    address: u64,
    stride: u32,
    height: u32,
    bg_color: T,
) {
    let bpp = core::mem::size_of::<T>() as u32;
    let pixels_per_line = (stride / bpp) as usize;
    let row_pixels = 10 * pixels_per_line;
    let total_pixels = pixels_per_line * height as usize;
    let region = unsafe {
        FramebufferRegion::from_address(
            address as usize,
            (stride as usize) * (height as usize),
            Permissions::READ_WRITE,
        )
        .expect("invalid framebuffer region")
    };

    // Use volatile copy for MMIO (wider T reduces loop count)
    for i in 0..(total_pixels.saturating_sub(row_pixels)) {
        let value: T = region
            .read_volatile_at((row_pixels + i) * core::mem::size_of::<T>())
            .expect("invalid framebuffer read");
        let _ = region.write_volatile_at(i * core::mem::size_of::<T>(), value);
    }

    // Clear the freed last text row (10 lines)
    let clear_start = (height.saturating_sub(10) as usize) * pixels_per_line;
    let clear_count = row_pixels;
    for i in 0..clear_count {
        let _ = region.write_volatile_at((clear_start + i) * core::mem::size_of::<T>(), bg_color);
    }
}
