use super::framebuffer::{FramebufferLike, FramebufferWriter};
use crate::{
    COLOR_BLACK, COLOR_DARK_GRAY, COLOR_LIGHT_BLUE, COLOR_LIGHT_GRAY, COLOR_TASKBAR, COLOR_WHITE,
    COLOR_WINDOW_BG, calc_text_width, draw_border_rect, draw_filled_rect,
    serial::debug_print_str_to_com1 as debug_print_str,
};
use alloc::string::{String, ToString};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    prelude::*,
    text::Text,
};

// Assuming framebuffer mod is in petroleum

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
    let text_width = calc_text_width(text);
    let text_x = x + (width as i32 / 2) - (text_width as i32 / 2);
    let text_obj = Text::new(text, Point::new(text_x, y), style);
    text_obj.draw(writer).ok();
}

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
            x,
            y,
            width,
            height,
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

    pub fn draw<W: FramebufferLike>(&self, writer: &mut W) {
        draw_filled_rect!(
            writer,
            self.x as i32,
            self.y as i32,
            self.width,
            self.height,
            self.bg_color
        );
        draw_centered_text(
            writer,
            &self.text,
            self.x as i32,
            self.y as i32 + (self.height as i32 / 2) - 5,
            self.width,
            self.text_color,
        );
    }

    pub fn contains_point(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

pub fn draw_os_desktop<W: FramebufferLike>(writer: &mut W) {
    let mode = if cfg!(target_os = "uefi") {
        "UEFI"
    } else {
        "BIOS"
    };
    draw_desktop_internal(writer, mode);
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
}

fn fill_background<W: FramebufferLike>(writer: &mut W, color: u32) {
    draw_filled_rect!(writer, 0, 0, writer.get_width(), writer.get_height(), color);
}

fn draw_menu_bar<W: FramebufferLike>(writer: &mut W) {
    draw_filled_rect!(writer, 0, 0, writer.get_width(), 25, COLOR_LIGHT_BLUE);

    let style = MonoTextStyle::new(&FONT_6X10, super::u32_to_rgb888(COLOR_BLACK));
    Text::new("Fullerene OS", Point::new(10, 8), style)
        .draw(writer)
        .ok();

    let time_text = "12:34";
    let time_x = writer.get_width() as i32 - (time_text.len() as i32 * 6) - 10;
    Text::new(time_text, Point::new(time_x, 8), style)
        .draw(writer)
        .ok();
}

fn draw_main_window<W: FramebufferLike>(writer: &mut W) {
    draw_border_rect!(writer, 50, 80, 200, 100, 255, 64, 1);
}

fn draw_icons<W: FramebufferLike>(writer: &mut W) {
    draw_filled_rect!(writer, 65, 100, 48, 48, 96);
    draw_filled_rect!(writer, 125, 100, 48, 48, 160);
}

fn draw_taskbar_with_buttons<W: FramebufferLike>(writer: &mut W) {
    let height = writer.get_height();
    let bar_height = 40;
    draw_filled_rect!(
        writer,
        0,
        (height - bar_height) as i32,
        writer.get_width() as u32,
        bar_height,
        COLOR_TASKBAR
    );

    let start_y = height - bar_height + 5;
    Button::new(5, start_y, 80, 30, "Start").draw(writer);
    Button::new(100, start_y, 150, 30, "Terminal").draw(writer);
    Button::new(260, start_y, 150, 30, "File Mgr").draw(writer);
}

fn draw_application_windows<W: FramebufferLike>(writer: &mut W) {
    draw_app_window(writer, 300, 80, 250, 150, "File Manager");
    draw_shell_window(writer, 100, 250, 350, 120);
}

fn draw_app_window<W: FramebufferLike>(
    writer: &mut W,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    title: &str,
) {
    draw_window_shell!(writer, x as i32, y as i32, width, height, title, {});
}

fn draw_shell_window<W: FramebufferLike>(writer: &mut W, x: u32, y: u32, width: u32, height: u32) {
    draw_window_shell!(writer, x as i32, y as i32, width, height, "Shell", {
        let text_style = MonoTextStyle::new(&FONT_6X10, super::u32_to_rgb888(COLOR_BLACK));
        Text::new(
            "fullerene> ",
            Point::new(x as i32 + 15, y as i32 + 40),
            text_style,
        )
        .draw(writer)
        .ok();
        Text::new(
            "Welcome to Fullerene OS Shell",
            Point::new(x as i32 + 15, y as i32 + 55),
            text_style,
        )
        .draw(writer)
        .ok();
    });
}
