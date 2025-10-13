use super::framebuffer::FramebufferLike;
use alloc::string::{String, ToString};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use petroleum::serial::debug_print_str_to_com1 as debug_print_str;

use super::text; // For re-exporting statics or accessing

// Helper macros and functions to reduce code duplication
macro_rules! create_button {
    ($x:expr, $y:expr, $width:expr, $height:expr, $text:expr, $bg:expr, $text_color:expr) => {
        Button::new($x, $y, $width, $height, $text).with_colors($bg, $text_color)
    };
}

macro_rules! draw_filled_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $color:expr) => {
        let rect = Rectangle::new(Point::new($x, $y), Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color(super::u32_to_rgb888($color))
            .build();
        rect.into_styled(style).draw($writer).ok();
    };
}

macro_rules! draw_border_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $bg:expr, $border:expr, $stroke:expr) => {
        let rect = Rectangle::new(Point::new($x, $y), Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color(super::u32_to_rgb888($bg))
            .stroke_color(super::u32_to_rgb888($border))
            .stroke_width($stroke)
            .build();
        rect.into_styled(style).draw($writer).ok();
    };
}

// Common colors as constants
const COLOR_LIGHT_GRAY: u32 = 0xE0E0E0;
const COLOR_BLACK: u32 = 0x000000;
const COLOR_DARK_GRAY: u32 = 0xA0A0A0;
const COLOR_WHITE: u32 = 0xFFFFFF;
const COLOR_LIGHT_BLUE: u32 = 0xADD8E6;
const COLOR_TASKBAR: u32 = 0xC0C0C0;
const COLOR_WINDOW_BG: u32 = 0xF8F8F8;

// Helper function to draw centered text
fn draw_centered_text<W: FramebufferLike>(
    writer: &mut W,
    text: &str,
    x: i32,
    y: i32,
    width: u32,
    color: u32,
) {
    let style = MonoTextStyle::new(&FONT_6X10, super::u32_to_rgb888(color));
    let text_width = text.len() as i32 * 6; // Approximate char width
    let text_x = x + (width as i32 / 2) - (text_width / 2);
    let text_obj = Text::new(text, Point::new(text_x, y), style);
    text_obj.draw(writer).ok();
}

// Generic window drawing trait
trait WindowElement {
    fn draw_element<W: FramebufferLike>(&self, writer: &mut W);
}

// Simple GUI element definitions with trait implementation
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
            bg_color: COLOR_LIGHT_GRAY,
            text_color: COLOR_BLACK,
        }
    }

    pub fn with_colors(mut self, bg: u32, text_color: u32) -> Self {
        self.bg_color = bg;
        self.text_color = text_color;
        self
    }
}

impl WindowElement for Button {
    fn draw_element<W: FramebufferLike>(&self, writer: &mut W) {
        draw_border_rect!(writer, self.x as i32, self.y as i32, self.width, self.height, self.bg_color, 0x808080, 2);
        draw_centered_text(writer, &self.text, self.x as i32, self.y as i32 + (self.height as i32 / 2) - 5, self.width, self.text_color);
    }
}

impl Button {
    pub fn draw<W: FramebufferLike>(&self, writer: &mut W) {
        self.draw_element(writer);
    }



    pub fn contains_point(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

// Generic draw functions
pub fn draw_os_desktop() {
    #[cfg(target_os = "uefi")]
    let fb_option = text::FRAMEBUFFER_UEFI.get();
    #[cfg(not(target_os = "uefi"))]
    let fb_option = text::FRAMEBUFFER_BIOS.get();

    let mode = if cfg!(target_os = "uefi") { "UEFI" } else { "BIOS" };
    crate::graphics::_print(format_args!("Graphics: draw_os_desktop() called\n"));

    if let Some(fb_writer) = fb_option {
        let mut locked = fb_writer.lock();
        if locked.is_vga() {
            crate::graphics::_print(format_args!("Graphics: VGA text mode active, desktop drawing skipped\n"));
        } else {
            draw_desktop_internal(&mut *locked, mode);
        }
    } else {
        crate::graphics::_print(format_args!("Graphics: ERROR - {} framebuffer not initialized\n", mode));
    }
}

fn draw_desktop_internal<W: FramebufferLike>(writer: &mut W, mode: &str) {
    let bg_color = 32u32; // Dark gray
    fill_background(writer, bg_color);

    // Main desktop elements
    draw_menu_bar(writer);
    draw_main_window(writer);
    draw_icons(writer);
    draw_taskbar_with_buttons(writer);
    draw_application_windows(writer);

    crate::graphics::_print(format_args!("Graphics: {} desktop drawing completed\n", mode));
}

fn fill_background<W: FramebufferLike>(writer: &mut W, color: u32) {
    draw_filled_rect!(writer, 0, 0, writer.get_width(), writer.get_height(), color);
}

fn draw_menu_bar<W: FramebufferLike>(writer: &mut W) {
    draw_filled_rect!(writer, 0, 0, writer.get_width(), 25, COLOR_LIGHT_BLUE);

    let style = MonoTextStyle::new(&FONT_6X10, super::u32_to_rgb888(COLOR_BLACK));
    Text::new("Fullerene OS", Point::new(10, 8), style).draw(writer).ok();

    let time_text = "12:34";
    let time_x = writer.get_width() as i32 - (time_text.len() as i32 * 6) - 10;
    Text::new(time_text, Point::new(time_x, 8), style).draw(writer).ok();
}

fn draw_main_window<W: FramebufferLike>(writer: &mut W) {
    draw_border_rect!(writer, 50, 80, 200, 100, 255, 64, 1);
}

fn draw_icons<W: FramebufferLike>(writer: &mut W) {
    draw_icon(writer, 65, 100, 96); // File manager icon
    draw_icon(writer, 125, 100, 160); // Terminal icon
}

fn draw_taskbar_with_buttons<W: FramebufferLike>(writer: &mut W) {
    let height = writer.get_height();
    let bar_height = 40;
    draw_filled_rect!(writer, 0, (height - bar_height) as i32, writer.get_width() as u32, bar_height, COLOR_TASKBAR);

    let start_y = height - bar_height + 5;
    Button::new(5, start_y, 80, 30, "Start").draw(writer);
    Button::new(100, start_y, 150, 30, "Terminal").draw(writer);
    Button::new(260, start_y, 150, 30, "File Mgr").draw(writer);
}

fn draw_application_windows<W: FramebufferLike>(writer: &mut W) {
    draw_app_window(writer, 300, 80, 250, 150, "File Manager");
    draw_shell_window(writer, 100, 250, 350, 120);
}

fn draw_icon<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, color: u32) {
    draw_filled_rect!(writer, x as i32, y as i32, 48, 48, color);
}

fn draw_app_window<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, width: u32, height: u32, title: &str) {
    draw_border_rect!(writer, x as i32, y as i32, width, height, COLOR_WHITE, COLOR_BLACK, 2);
    draw_filled_rect!(writer, x as i32, y as i32, width, 25, COLOR_DARK_GRAY);
    draw_centered_text(writer, title, x as i32, y as i32 + 8, width, COLOR_BLACK);
    draw_filled_rect!(writer, x as i32 + 5, y as i32 + 30, width.saturating_sub(10), height.saturating_sub(35), COLOR_WINDOW_BG);
}

fn draw_shell_window<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, width: u32, height: u32) {
    draw_app_window(writer, x, y, width, height, "Shell");
    let text_style = MonoTextStyle::new(&FONT_6X10, super::u32_to_rgb888(COLOR_BLACK));
    Text::new("fullerene> ", Point::new(x as i32 + 15, y as i32 + 40), text_style).draw(writer).ok();
    Text::new("Welcome to Fullerene OS Shell", Point::new(x as i32 + 15, y as i32 + 55), text_style).draw(writer).ok();
}
