use crate::common::EfiGraphicsPixelFormat;
use crate::graphics::color::{FramebufferInfo, PixelType, rgb_pixel, vga_color_index};
use crate::{clear_buffer_pixels, scroll_buffer_pixels};
use embedded_graphics::{
    geometry::{Point, Size},
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::Rgb888,
    prelude::*,
    text::Text,
};
use spin::{Mutex, Once};

// Generic type aliases for cleaner code
type FramebufferWriter32 = FramebufferWriter<u32>;
type FramebufferWriter8 = FramebufferWriter<u8>;

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
    pub fn new(info: FramebufferInfo) -> Self {
        Self {
            info,
            x_pos: 0,
            y_pos: 0,
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn rgb888_to_pixel_format(&self, color: Rgb888) -> u32 {
        let rgb = || rgb_pixel(color.r(), color.g(), color.b());
        let bgr = || rgb_pixel(color.b(), color.g(), color.r());
        #[allow(non_exhaustive_omitted_patterns)]
        match self.info.pixel_format {
            Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor) => rgb(),
            Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor) => bgr(),
            // Cirrus VGA commonly reports PixelBitMask but expects RGB format
            Some(EfiGraphicsPixelFormat::PixelBitMask) | Some(_) => rgb(), // Treat all unknown formats as RGB
            None => {
                // VGA mode (8-bit indexed color) - convert RGB to VGA palette index
                // Simple palette approximation: map RGB to closest VGA color
                vga_color_index(color.r(), color.g(), color.b())
            }
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

impl<T: PixelType> core::fmt::Write for FramebufferWriter<T> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
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
