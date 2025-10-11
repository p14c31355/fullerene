use petroleum::serial::debug_print_str_to_com1 as debug_print_str;
use super::framebuffer::{FramebufferLike, UefiFramebuffer};
use embedded_graphics::{
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
};

use super::text; // For re-exporting statics or accessing

// Draw OS-like desktop interface
#[cfg(target_os = "uefi")]
pub fn draw_os_desktop() {
    crate::graphics::_print(format_args!("Graphics: draw_os_desktop() called\n"));
    debug_print_str("Graphics: draw_os_desktop() started\n");
    debug_print_str("Graphics: checking UEFI framebuffer...\n");
    if let Some(fb_writer) = text::FRAMEBUFFER_UEFI.get() {
        debug_print_str("Graphics: Obtained UEFI framebuffer writer\n");
        let mut locked = fb_writer.lock();
        debug_print_str("Graphics: Framebuffer writer locked\n");
        draw_desktop_internal(&mut *locked, "UEFI");
    } else {
        crate::graphics::_print(format_args!("Graphics: ERROR - FRAMEBUFFER_UEFI not initialized\n"));
        debug_print_str("Graphics: ERROR - FRAMEBUFFER_UEFI not initialized\n");
    }
}

#[cfg(not(target_os = "uefi"))]
pub fn draw_os_desktop() {
    crate::graphics::_print(format_args!("Graphics: draw_os_desktop() called in BIOS mode\n"));
    debug_print_str("Graphics: BIOS mode draw_os_desktop() started\n");
    debug_print_str("Graphics: checking BIOS framebuffer...\n");
    if let Some(fb_writer) = text::FRAMEBUFFER_BIOS.get() {
        debug_print_str("Graphics: Obtained BIOS framebuffer writer\n");
        let mut locked = fb_writer.lock();
        debug_print_str("Graphics: Framebuffer writer locked\n");
        draw_desktop_internal(&mut *locked, "BIOS");
    } else {
        crate::graphics::_print(format_args!("Graphics: ERROR - BIOS framebuffer not initialized\n"));
        debug_print_str("Graphics: ERROR - BIOS framebuffer not initialized\n");
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
    crate::graphics::_print(format_args!("Graphics: {} desktop drawing completed\n", mode));
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
