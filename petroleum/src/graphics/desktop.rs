use super::renderer::Renderer;
use crate::{
    Button, COLOR_BLACK, COLOR_DARK_GRAY, COLOR_LIGHT_BLUE, COLOR_TASKBAR, calc_text_width,
};

pub fn draw_os_desktop(renderer: &mut dyn Renderer) {
    let mode = if cfg!(target_os = "uefi") {
        "UEFI"
    } else {
        "BIOS"
    };
    draw_desktop_internal(renderer, mode);
}

fn draw_desktop_internal(renderer: &mut dyn Renderer, _mode: &str) {
    let bg_color = 32u32; // Dark gray
    fill_background(renderer, bg_color);

    // Main desktop elements
    draw_menu_bar(renderer);
    draw_main_window(renderer);
    draw_icons(renderer);
    draw_taskbar_with_buttons(renderer);
    draw_application_windows(renderer);
}

fn fill_background(renderer: &mut dyn Renderer, color: u32) {
    let (w, h) = renderer.get_resolution();
    renderer.draw_rect(0, 0, w, h, color);
}

fn draw_menu_bar(renderer: &mut dyn Renderer) {
    let (w, _) = renderer.get_resolution();
    renderer.draw_rect(0, 0, w, 25, COLOR_LIGHT_BLUE);

    renderer.draw_text(10, 8, "Fullerene OS", COLOR_BLACK);

    let time_text = "12:34";
    let time_x = w as i32 - (time_text.len() as i32 * 6) - 10;
    renderer.draw_text(time_x, 8, time_text, COLOR_BLACK);
}

fn draw_main_window(renderer: &mut dyn Renderer) {
    // draw_border_rect is a macro that uses embedded-graphics.
    // For now, we implement a simple border using rectangles.
    renderer.draw_rect(50, 80, 200, 100, 255); // Fill
    renderer.draw_rect(50, 80, 200, 1, 64); // Top
    renderer.draw_rect(50, 179, 200, 1, 64); // Bottom
    renderer.draw_rect(50, 80, 1, 100, 64); // Left
    renderer.draw_rect(249, 80, 1, 100, 64); // Right
}

fn draw_icons(renderer: &mut dyn Renderer) {
    renderer.draw_rect(65, 100, 48, 48, 96);
    renderer.draw_rect(125, 100, 48, 48, 160);
}

fn draw_taskbar_with_buttons(renderer: &mut dyn Renderer) {
    let (_, height) = renderer.get_resolution();
    let bar_height = 40;
    renderer.draw_rect(
        0,
        (height as i32 - bar_height as i32),
        renderer.get_resolution().0,
        bar_height,
        COLOR_TASKBAR,
    );

    let start_y = height as i32 - bar_height as i32 + 5;
    Button::new(5, start_y as u32, 80, 30, "Start").draw(renderer);
    Button::new(100, start_y as u32, 150, 30, "Terminal").draw(renderer);
    Button::new(260, start_y as u32, 150, 30, "File Mgr").draw(renderer);
}

fn draw_application_windows(renderer: &mut dyn Renderer) {
    draw_app_window(renderer, 300, 80, 250, 150, "File Manager");
    draw_shell_window(renderer, 100, 250, 350, 120);
}

fn draw_app_window(
    renderer: &mut dyn Renderer,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    title: &str,
) {
    // Simplified window shell using Renderer
    renderer.draw_rect(x as i32, y as i32, width, height, 255); // BG
    renderer.draw_rect(x as i32, y as i32, width, 25, COLOR_DARK_GRAY); // Title bar
    renderer.draw_text(x as i32 + 10, y as i32 + 8, title, COLOR_BLACK);
}

fn draw_shell_window(renderer: &mut dyn Renderer, x: u32, y: u32, width: u32, height: u32) {
    // Simplified shell window using Renderer
    renderer.draw_rect(x as i32, y as i32, width, height, 255); // BG
    renderer.draw_rect(x as i32, y as i32, width, 25, COLOR_DARK_GRAY); // Title bar
    renderer.draw_text(x as i32 + 10, y as i32 + 8, "Shell", COLOR_BLACK);

    renderer.draw_text(x as i32 + 15, y as i32 + 40, "fullerene> ", COLOR_BLACK);
    renderer.draw_text(
        x as i32 + 15,
        y as i32 + 55,
        "Welcome to Fullerene OS Shell",
        COLOR_BLACK,
    );
}
