use alloc::string::{String, ToString};
use super::framebuffer::FramebufferLike;
use embedded_graphics::{
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
};
use petroleum::serial::debug_print_str_to_com1 as debug_print_str;

use super::text; // For re-exporting statics or accessing

// Simple GUI element definitions
#[derive(Clone)]
pub struct Button {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub text: String,
    pub bg_color: u32,
    pub text_color: u32,
}

impl Button {
    pub fn new(x: u32, y: u32, width: u32, height: u32, text: &str) -> Self {
        Self {
            x, y, width, height,
            text: text.to_string(),
            bg_color: 0xE0E0E0, // Light gray
            text_color: 0x000000, // Black
        }
    }

    pub fn draw<W: FramebufferLike>(&self, writer: &mut W) {
        // Draw button background
        let bg_rgb = super::u32_to_rgb888(self.bg_color);
        let style = PrimitiveStyleBuilder::new()
            .fill_color(bg_rgb)
            .stroke_color(super::u32_to_rgb888(0x808080))
            .stroke_width(2)
            .build();
        let rect = Rectangle::new(
            Point::new(self.x as i32, self.y as i32),
            Size::new(self.width, self.height)
        );
        rect.into_styled(style).draw(writer).ok();

        // Draw button text
        let text_rgb = super::u32_to_rgb888(self.text_color);
        let text_style = MonoTextStyle::new(&FONT_6X10, text_rgb);
        let text = Text::new(
            &self.text,
            Point::new(
                self.x as i32 + (self.width as i32 / 2) - ((self.text.len() as i32 * 6) / 2), // Center text
                self.y as i32 + (self.height as i32 / 2) - 5 // Vertically center
            ),
            text_style
        );
        text.draw(writer).ok();
    }

    pub fn contains_point(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

pub fn draw_os_desktop() {
    let mode = if cfg!(target_os = "uefi") { "UEFI" } else { "BIOS" };
    crate::graphics::_print(format_args!("Graphics: draw_os_desktop() called\n"));
    debug_print_str("Graphics: draw_os_desktop() started\n");
    debug_print_str("Graphics: checking framebuffer...\n");

    #[cfg(target_os = "uefi")]
    let fb_option = text::FRAMEBUFFER_UEFI.get();
    #[cfg(not(target_os = "uefi"))]
    let fb_option = text::FRAMEBUFFER_BIOS.get();

    if let Some(fb_writer) = fb_option {
        debug_print_str("Graphics: Obtained framebuffer writer\n");
        let mut locked = fb_writer.lock();
        debug_print_str("Graphics: Framebuffer writer locked\n");
        draw_desktop_internal(&mut *locked, mode);
    } else {
        crate::graphics::_print(format_args!(
            "Graphics: ERROR - {} framebuffer not initialized\n", mode
        ));
        debug_print_str("Graphics: ERROR - framebuffer not initialized\n");
    }
}

fn draw_desktop_internal(fb_writer: &mut impl FramebufferLike, mode: &str) {
    let is_vga = fb_writer.is_vga();
    if is_vga {
        petroleum::serial::serial_log(format_args!(
            "Graphics: Framebuffer size: {}x{}, VGA mode\n",
            fb_writer.get_width(),
            fb_writer.get_height()
        ));
    } else {
        petroleum::serial::serial_log(format_args!(
            "Graphics: Framebuffer size: {}x{}, stride: {}\n",
            fb_writer.get_width(),
            fb_writer.get_height(),
            fb_writer.get_stride()
        ));
    }
    let bg_color = 32u32; // Dark gray
    debug_print_str("Graphics: Filling background...\n");
    fill_background(fb_writer, bg_color);
    debug_print_str("Graphics: Background filled\n");

    // Draw menu bar at the top
    debug_print_str("Graphics: Drawing menu bar...\n");
    draw_menu_bar(fb_writer);

    debug_print_str("Graphics: Drawing windows and icons...\n");
    draw_window(fb_writer, 50, 80, 200, 100, 255u32, 64u32); // Main window
    draw_icon(fb_writer, 65, 100, 96u32); // File manager icon
    draw_icon(fb_writer, 125, 100, 160u32); // Terminal icon

    debug_print_str("Graphics: Drawing taskbar and buttons...\n");
    draw_taskbar_with_buttons(fb_writer);

    // Draw application windows
    debug_print_str("Graphics: Drawing application windows...\n");
    draw_app_window(fb_writer, 300, 80, 250, 150, "File Manager");

    debug_print_str("Graphics: Drawing shell window...\n");
    draw_shell_window(fb_writer, 100, 250, 350, 120);

    crate::graphics::_print(format_args!(
        "Graphics: {} desktop drawing completed\n",
        mode
    ));
    debug_print_str("Graphics: desktop drawing completed\n");
}

fn fill_background(writer: &mut impl FramebufferLike, color: u32) {
    let color_rgb = super::u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(
        embedded_graphics::geometry::Point::new(0, 0),
        embedded_graphics::geometry::Size::new(writer.get_width(), writer.get_height()),
    );
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
    let bg_rgb = super::u32_to_rgb888(bg_color);
    let border_rgb = super::u32_to_rgb888(border_color);
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

    let color_rgb = super::u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(
        embedded_graphics::geometry::Point::new(0, (height - taskbar_height) as i32),
        embedded_graphics::geometry::Size::new(writer.get_width(), taskbar_height),
    );
    rect.into_styled(style).draw(writer).ok();

    // Simple start button
    draw_window(
        writer,
        0,
        height - taskbar_height + 5,
        80,
        30,
        0xE0E0E0u32,
        0x000000u32,
    );
}

fn draw_icon<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, color: u32) {
    const ICON_SIZE: u32 = 48;
    let color_rgb = super::u32_to_rgb888(color);
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(
        Point::new(x as i32, y as i32),
        Size::new(ICON_SIZE, ICON_SIZE),
    );
    rect.into_styled(style).draw(writer).ok();
}

fn draw_menu_bar<W: FramebufferLike>(writer: &mut W) {
    // Menu bar height
    let menu_height = 25;

    // Draw menu bar background (light blue)
    let bg_rgb = super::u32_to_rgb888(0xADD8E6);
    let style = PrimitiveStyleBuilder::new().fill_color(bg_rgb).build();
    let rect = Rectangle::new(
        Point::new(0, 0),
        Size::new(writer.get_width(), menu_height),
    );
    rect.into_styled(style).draw(writer).ok();

    // Draw menu text "Fullerene OS"
    let text_rgb = super::u32_to_rgb888(0x000000);
    let text_style = MonoTextStyle::new(&FONT_6X10, text_rgb);
    let text = Text::new(
        "Fullerene OS",
        Point::new(10, 8), // Left side, centered in bar
        text_style
    );
    text.draw(writer).ok();

    // Draw menu items
    let time_text = "12:34"; // Would be actual time in real implementation
    let time_x = (writer.get_width() - (time_text.len() as u32 * 6) - 10) as i32;
    let time_text = Text::new(
        time_text,
        Point::new(time_x, 8),
        text_style
    );
    time_text.draw(writer).ok();
}

fn draw_taskbar_with_buttons<W: FramebufferLike>(writer: &mut W) {
    let height = writer.get_height();
    let taskbar_height = 40;

    // Draw taskbar background
    let color_rgb = super::u32_to_rgb888(0xC0C0C0); // Light gray
    let style = PrimitiveStyleBuilder::new().fill_color(color_rgb).build();
    let rect = Rectangle::new(
        Point::new(0, (height - taskbar_height) as i32),
        Size::new(writer.get_width(), taskbar_height),
    );
    rect.into_styled(style).draw(writer).ok();

    // Create and draw buttons
    let start_button = Button::new(5, height - taskbar_height + 5, 80, 30, "Start");
    start_button.draw(writer);

    // Task buttons
    let task_button1 = Button::new(100, height - taskbar_height + 5, 150, 30, "Terminal");
    task_button1.draw(writer);

    let task_button2 = Button::new(260, height - taskbar_height + 5, 150, 30, "File Mgr");
    task_button2.draw(writer);
}

fn draw_app_window<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, width: u32, height: u32, title: &str) {
    // Window background
    let bg_rgb = super::u32_to_rgb888(0xFFFFFF);
    let style = PrimitiveStyleBuilder::new()
        .fill_color(bg_rgb)
        .stroke_color(super::u32_to_rgb888(0x000000))
        .stroke_width(2)
        .build();
    let rect = Rectangle::new(Point::new(x as i32, y as i32), Size::new(width, height));
    rect.into_styled(style).draw(writer).ok();

    // Title bar (darker background for title)
    let title_height = 25;
    let title_bg_rgb = super::u32_to_rgb888(0xA0A0A0);
    let title_style = PrimitiveStyleBuilder::new().fill_color(title_bg_rgb).build();
    let title_rect = Rectangle::new(
        Point::new(x as i32, y as i32),
        Size::new(width, title_height)
    );
    title_rect.into_styled(title_style).draw(writer).ok();

    // Window title
    let text_rgb = super::u32_to_rgb888(0x000000);
    let text_style = MonoTextStyle::new(&FONT_6X10, text_rgb);
    let text = Text::new(
        title,
        Point::new(x as i32 + 10, y as i32 + 8),
        text_style
    );
    text.draw(writer).ok();

    // Content area
    let content_bg_rgb = super::u32_to_rgb888(0xF8F8F8);
    let content_style = PrimitiveStyleBuilder::new().fill_color(content_bg_rgb).build();
    let content_rect = Rectangle::new(
        Point::new(x as i32 + 5, y as i32 + title_height as i32 + 5),
        Size::new(width - 10, height - title_height - 10)
    );
    content_rect.into_styled(content_style).draw(writer).ok();
}

fn draw_shell_window<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, width: u32, height: u32) {
    draw_app_window(writer, x, y, width, height, "Shell");

    // Add a simple prompt and some sample output
    let text_rgb = super::u32_to_rgb888(0x000000);
    let text_style = MonoTextStyle::new(&FONT_6X10, text_rgb);

    let prompt_y = y + 35;
    let prompt = Text::new(
        "fullerene> ",
        Point::new(x as i32 + 15, prompt_y as i32),
        text_style
    );
    prompt.draw(writer).ok();

    let output_y = prompt_y + 15;
    let output = Text::new(
        "Welcome to Fullerene OS Shell",
        Point::new(x as i32 + 15, output_y as i32),
        text_style
    );
    output.draw(writer).ok();
}
