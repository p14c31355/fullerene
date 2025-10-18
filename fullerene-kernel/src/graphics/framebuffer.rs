use core::marker::{Send, Sync};
use core::ptr::{read_volatile, write_volatile};
use embedded_graphics::{geometry::Size, pixelcolor::Rgb888, prelude::*};
use petroleum::common::EfiGraphicsPixelFormat;
use petroleum::common::FullereneFramebufferConfig;
use petroleum::common::VgaFramebufferConfig;
use petroleum::graphics::rgb_pixel;
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};
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
            ((y * self.stride) + (x * self.bytes_per_pixel() as u32)) as usize
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

/// Convert RGB888 color to VGA palette index (8-bit indexed color)
/// This is a simple approximation that maps common colors to their closest VGA equivalents
pub fn vga_color_index(r: u8, g: u8, b: u8) -> u32 {
    // Standard 16-color EGA/VGA palette with their approximate RGB values.
    const PALETTE: [(u8, u8, u8, u32); 16] = [
        (0, 0, 0, 0),        // Black
        (0, 0, 170, 1),      // Blue
        (0, 170, 0, 2),      // Green
        (0, 170, 170, 3),    // Cyan
        (170, 0, 0, 4),      // Red
        (170, 0, 170, 5),    // Magenta
        (170, 85, 0, 6),     // Brown
        (170, 170, 170, 7),  // Light Gray
        (85, 85, 85, 8),     // Dark Gray
        (85, 85, 255, 9),    // Light Blue
        (85, 255, 85, 10),   // Light Green
        (85, 255, 255, 11),  // Light Cyan
        (255, 85, 85, 12),   // Light Red
        (255, 85, 255, 13),  // Light Magenta
        (255, 255, 85, 14),  // Yellow
        (255, 255, 255, 15), // White
    ];

    let mut min_dist_sq = u32::MAX;
    let mut best_index = 0;

    for &(pr, pg, pb, index) in &PALETTE {
        let dr = r as i32 - pr as i32;
        let dg = g as i32 - pg as i32;
        let db = b as i32 - pb as i32;
        let dist_sq = (dr * dr + dg * dg + db * db) as u32;

        if dist_sq < min_dist_sq {
            min_dist_sq = dist_sq;
            best_index = index;
        }
    }

    best_index
}

/// Global simple framebuffer config (Redox vesad-style)
pub static SIMPLE_FRAMEBUFFER_CONFIG: Once<spin::Mutex<Option<SimpleFramebufferConfig>>> = Once::new();

/// Simple framebuffer config for recreation
#[derive(Clone, Copy)]
pub struct SimpleFramebufferConfig {
    pub base_addr: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize, // bytes per row
    pub bytes_per_pixel: usize,
}

/// Get the simple framebuffer instance (creates it from config if needed)
pub fn get_simple_framebuffer() -> Option<SimpleFramebuffer> {
    SIMPLE_FRAMEBUFFER_CONFIG.get().and_then(|config_mutex| {
        let config = config_mutex.lock();
        config.as_ref().map(|cfg| SimpleFramebuffer::new(*cfg))
    })
}

/// Initialize the simple framebuffer config
pub fn init_simple_framebuffer_config(config: SimpleFramebufferConfig) {
    SIMPLE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
}

/// Simple Framebuffer struct for direct MMIO pixel manipulation (Redox vesad-style)
pub struct SimpleFramebuffer {
    pub base: usize, // Use usize instead of raw pointer to avoid Send/Sync issues
    pub width: usize,
    pub height: usize,
    pub stride: usize, // bytes per row
    pub bytes_per_pixel: usize,
}

impl SimpleFramebuffer {
    /// Create a new framebuffer from GOP config
    pub fn new(config: SimpleFramebufferConfig) -> Self {
        Self {
            base: config.base_addr,
            width: config.width,
            height: config.height,
            stride: config.stride,
            bytes_per_pixel: config.bytes_per_pixel,
        }
    }

    /// Clear the entire framebuffer
    pub fn clear(&mut self, color: u32) {
        let color_bytes = color.to_le_bytes();
        for y in 0..self.height {
            let row_base = self.base + y * self.stride;
            for x in 0..self.width {
                let offset = x * self.bytes_per_pixel;
                let pixel_addr = (row_base + offset) as *mut u8;

                // Check that the calculated pixel_addr is within the valid framebuffer memory region
                let pixel_addr_usize = pixel_addr as usize;
                if pixel_addr_usize < self.base || (pixel_addr_usize + self.bytes_per_pixel) > (self.base + self.height * self.stride) {
                    continue;
                }

                unsafe {
                    for i in 0..self.bytes_per_pixel {
                        if i < color_bytes.len() {
                            write_volatile(pixel_addr.add(i), color_bytes[i]);
                        }
                    }
                }
            }
        }
    }

    /// Draw a single pixel (orbclient-style)
    pub fn draw_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let row_base = self.base + y * self.stride;
        let offset = x * self.bytes_per_pixel;
        let pixel_addr = (row_base + offset) as *mut u8;

        // Check that the calculated pixel_addr is within the valid framebuffer memory region
        let pixel_addr_usize = pixel_addr as usize;
        if pixel_addr_usize < self.base || (pixel_addr_usize + self.bytes_per_pixel) > (self.base + self.height * self.stride) {
            return;
        }

        unsafe {
            let color_bytes = color.to_le_bytes();
            for i in 0..self.bytes_per_pixel {
                if i < color_bytes.len() {
                    write_volatile(pixel_addr.add(i), color_bytes[i]);
                }
            }
        }
    }

    /// Draw a filled rectangle (orbclient-style)
    pub fn draw_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u32) {
        for dy in 0..height {
            if y + dy >= self.height {
                break;
            }
            for dx in 0..width {
                if x + dx >= self.width {
                    break;
                }
                self.draw_pixel(x + dx, y + dy, color);
            }
        }
    }

    /// Read a pixel (for reference, though not used in Redox)
    pub fn get_pixel(&self, x: usize, y: usize) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let row_base = self.base + y * self.stride;
        let offset = x * self.bytes_per_pixel;
        let pixel_addr = (row_base + offset) as *const u32;
        unsafe { read_volatile(pixel_addr) }
    }

    /// Get framebuffer dimensions
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}
