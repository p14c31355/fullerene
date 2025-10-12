use core::marker::{Send, Sync};
use embedded_graphics::{geometry::Size, pixelcolor::Rgb888, prelude::*};
use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig};
use petroleum::graphics::{grayscale_intensity, rgb_pixel};
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};

#[derive(Clone, Copy)]
pub struct ColorScheme {
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

#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: Option<EfiGraphicsPixelFormat>,
    colors: ColorScheme,
}

impl FramebufferInfo {
    pub fn width_or_stride(&self) -> u32 {
        #[cfg(target_os = "uefi")]
        {
            self.stride
        }
        #[cfg(not(target_os = "uefi"))]
        {
            self.width
        }
    }

    pub fn calculate_offset(&self, x: u32, y: u32) -> usize {
        #[cfg(target_os = "uefi")]
        {
            ((y * self.stride + x) * self.bytes_per_pixel()) as usize
        }
        #[cfg(not(target_os = "uefi"))]
        {
            ((y * self.width + x) * 1) as usize
        } // 1 byte per pixel for VGA
    }

    pub fn bytes_per_pixel(&self) -> u32 {
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
    pub fn new(fb_config: &FullereneFramebufferConfig) -> Self {
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
    pub fn new_vga(config: &VgaFramebufferConfig) -> Self {
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
pub trait PixelType: Copy + Send + Sync {
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

// Implemented using individual match statements for clarity
// Could be macro-ized in future but delegation complexity justifies explicit handling
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
