#![no_std]

extern crate alloc;

pub mod compositor;
pub mod cursor;
pub mod desktop;
pub mod desktop_icons;
pub mod editor;
pub mod font;
pub mod menu;
pub mod network_menu;
pub mod renderer;
pub mod scene;
pub mod shell_overlay;
pub mod surface;
pub mod taskbar;
pub mod terminal_surface;
pub mod theme;
pub mod top_panel;
pub mod wallpaper;
pub mod window;
pub mod wm;

#[cfg(test)]
mod tests {
    use crate::cursor::Cursor;
    use crate::renderer::VecFramebuffer;
    use crate::scene::{DirtyRect, Scene};
    use crate::window::{Window, WindowId};

    #[test]
    fn test_dirty_rect_intersects() {
        let a = DirtyRect::new(10, 10, 50, 50);
        let b = DirtyRect::new(30, 30, 50, 50);
        let c = DirtyRect::new(100, 100, 10, 10);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn test_dirty_rect_merge() {
        let mut a = DirtyRect::new(10, 10, 20, 20);
        let b = DirtyRect::new(30, 30, 20, 20);
        a.merge(&b);
        assert_eq!(a.x, 10);
        assert_eq!(a.y, 10);
        assert_eq!(a.width, 40);
        assert_eq!(a.height, 40);
    }

    #[test]
    fn test_dirty_rect_full() {
        let r = DirtyRect::full(800, 600);
        assert_eq!(r.x, 0);
        assert_eq!(r.y, 0);
        assert_eq!(r.width, 800);
        assert_eq!(r.height, 600);
    }

    #[test]
    fn test_scene_new() {
        let windows = [Window::new(WindowId(1), 0, 0, 100, 100, 0xFF0000)];
        let cursor = Cursor::new(50, 50);
        let scene = Scene::new(&windows, Some(&cursor), 0x000000);
        assert_eq!(scene.windows.len(), 1);
        assert_eq!(scene.bg_color, 0x000000);
        assert!(scene.cursor.is_some());
    }

    #[test]
    fn test_scene_empty_windows() {
        let scene = Scene::new(&[], None, 0x1a1a2e);
        assert!(scene.windows.is_empty());
        assert!(scene.cursor.is_none());
    }

    #[test]
    fn test_vec_framebuffer_creation() {
        let fb = VecFramebuffer::new(64, 48);
        assert_eq!(fb.width, 64);
        assert_eq!(fb.height, 48);
        assert_eq!(fb.pixels.len(), 64 * 48);
    }

    #[test]
    fn test_ppm_output_consistency() {
        let fb = VecFramebuffer::new(4, 4);
        let ppm = fb.to_ppm_bytes();
        let header = b"P6\n4 4\n255\n";
        assert!(ppm.starts_with(header));
        assert_eq!(ppm.len(), header.len() + 4 * 4 * 3);
    }

    #[test]
    fn test_ppm_pixel_encoding() {
        let mut fb = VecFramebuffer::new(2, 1);
        fb.pixels[0] = 0xFF8800;
        fb.pixels[1] = 0x00FF44;
        let ppm = fb.to_ppm_bytes();
        let pixels_start = ppm.len() - 2 * 3;
        assert_eq!(&ppm[pixels_start..], &[0xFF, 0x88, 0x00, 0x00, 0xFF, 0x44]);
    }

    #[test]
    fn test_cursor_creation() {
        let cursor = Cursor::new(100, 200);
        assert_eq!(cursor.x, 100);
        assert_eq!(cursor.y, 200);
        assert!(cursor.visible);
    }
}
