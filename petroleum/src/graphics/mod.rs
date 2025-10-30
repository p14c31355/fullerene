
/// Generic border rectangle drawing
#[macro_export]
macro_rules! draw_border_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $fill_color:expr, $stroke_color:expr, $stroke_width:expr) => {{
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        let rect = Rectangle::new(Point::new($x, $y), Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($fill_color))
            .stroke_color($crate::u32_to_rgb888($stroke_color))
            .stroke_width($stroke_width)
            .build();
        rect.into_styled(style).draw($writer).ok();
    }};
}

/// Generic window drawing macro for desktop elements to reduce boilerplate
#[macro_export]
macro_rules! draw_window_shell {
    ($writer:expr, $x:expr, $y:expr, $width:expr, $height:expr, $title:expr, $content:block) => {{
        $crate::draw_window_base!($writer, $x, $y, $width, $height, $title);
        $content
    }};
}

/// Base window drawing macro
#[macro_export]
macro_rules! draw_window_base {
    ($writer:expr, $x:expr, $y:expr, $width:expr, $height:expr, $title:expr) => {{
        use embedded_graphics::mono_font::{MonoTextStyle, ascii::FONT_6X10};
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        use embedded_graphics::{prelude::*, text::Text};

        let rect = Rectangle::new(Point::new($x as i32, $y as i32), Size::new($width, $height));
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_WHITE))
            .stroke_color($crate::u32_to_rgb888($crate::COLOR_BLACK))
            .stroke_width(2)
            .build();
        rect.into_styled(style).draw($writer).ok();

        let title_rect = Rectangle::new(Point::new($x as i32, $y as i32), Size::new($width, 25));
        let title_style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_DARK_GRAY))
            .build();
        title_rect.into_styled(title_style).draw($writer).ok();

        let title_text_style =
            MonoTextStyle::new(&FONT_6X10, $crate::u32_to_rgb888($crate::COLOR_BLACK));
        let title_width = $crate::calc_text_width($title);
        let title_x = $x as i32 + (($width as i32 / 2) - (title_width as i32 / 2));
        Text::new($title, Point::new(title_x, $y as i32 + 8), title_text_style)
            .draw($writer)
            .ok();

        let content_rect = Rectangle::new(
            Point::new($x as i32 + 5, $y as i32 + 30),
            Size::new($width.saturating_sub(10), $height.saturating_sub(35)),
        );
        let content_style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_WINDOW_BG))
            .build();
        content_rect.into_styled(content_style).draw($writer).ok();
    }};
}

pub mod color;
pub mod constants;
pub mod desktop;
pub mod framebuffer;
#[macro_use]
pub mod ports;
pub mod registers;
pub mod setup;
pub mod text;

// VGA constants
pub use constants::*;

// Re-exports for public API
pub use color::*;
pub use ports::{HardwarePorts, PortWriter, VgaPortOps};
// VGA graphics modes
pub use setup::{
    detect_and_init_vga_graphics, detect_cirrus_vga, init_vga_graphics, init_vga_text_mode,
    setup_cirrus_vga_mode,
};

// VGA text operations
pub use text::{Color, ColorCode, ScreenChar, TextBufferOperations};

// Desktop drawing
pub use desktop::*;

// Framebuffer
pub use framebuffer::*;
