extern crate alloc;

use crate::scene::DirtyRect;
use crate::window::{Window, WindowId};
use alloc::string::String;
use alloc::vec::Vec;

/// Drag state machine.
#[derive(Debug, Clone)]
pub enum DragState {
    None,
    Moving {
        window: WindowId,
        offset_x: i32,
        offset_y: i32,
    },
    Resizing {
        window: WindowId,
        orig_x: i32,
        orig_y: i32,
        orig_width: u32,
        orig_height: u32,
    },
}

pub struct WindowManager {
    windows: Vec<Window>,
    focused: Option<WindowId>,
    next_id: u64,
    drag: DragState,
    resize_handle: u32,
    /// Accumulated dirty rectangles since last `consume_dirty_rects`.
    pub(crate) dirty_rects: Vec<DirtyRect>,
}

const RESIZE_HANDLE_SIZE: u32 = 16;
const MIN_WINDOW_W: u32 = 80;
const MIN_WINDOW_H: u32 = 40;

/// TITLE_BAR_HEIGHT from compositor — kept in sync manually.
const TITLE_BAR_H: u32 = 20;
const BORDER: u32 = 2;

/// Build a dirty rect for the full decorated area of a window.
pub(crate) fn window_dirty_rect(w: &Window) -> DirtyRect {
    let x0 = w.x.saturating_sub(BORDER as i32).max(0) as u32;
    let y0 = w.y.saturating_sub(BORDER as i32).max(0) as u32;
    let ww = w.width + BORDER * 2;
    let wh = w.height + TITLE_BAR_H + BORDER * 2;
    DirtyRect::new(x0, y0, ww, wh)
}

impl WindowManager {
    pub fn new() -> Self {
        Self { windows: Vec::new(), focused: None, next_id: 1, drag: DragState::None, resize_handle: RESIZE_HANDLE_SIZE, dirty_rects: Vec::new() }
    }

    pub fn windows(&self) -> &[Window] { &self.windows }
    pub fn windows_mut(&mut self) -> &mut [Window] { &mut self.windows }
    pub fn drag_state(&self) -> &DragState { &self.drag }
    pub fn focused(&self) -> Option<WindowId> { self.focused }

    // ── dirty rects ─────────────────────────────────────────

    /// Consume all accumulated dirty rects and clear the internal list.
    pub fn consume_dirty_rects(&mut self) -> Vec<DirtyRect> {
        let mut out = Vec::new();
        core::mem::swap(&mut out, &mut self.dirty_rects);
        out
    }

    pub fn create_window(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        self.create_with_id(id, x, y, width, height, color)
    }

    fn create_with_id(&mut self, id: WindowId, x: i32, y: i32, width: u32, height: u32, color: u32) -> WindowId {
        let mut window = Window::new(id, x, y, width, height, color);
        if let Some(prev) = self.focused {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev) {
                w.focused = false;
                self.dirty_rects.push(window_dirty_rect(w));
            }
        }
        window.focused = true;
        self.focused = Some(id);
        self.dirty_rects.push(window_dirty_rect(&window));
        self.windows.push(window);
        id
    }

    pub fn create_titled_window(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32, title: impl Into<String>) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let mut window = Window::new_with_title(id, x, y, width, height, color, title);
        if let Some(prev) = self.focused {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev) {
                w.focused = false;
                self.dirty_rects.push(window_dirty_rect(w));
            }
        }
        window.focused = true;
        self.focused = Some(id);
        self.dirty_rects.push(window_dirty_rect(&window));
        self.windows.push(window);
        id
    }

    pub fn remove_window(&mut self, id: WindowId) -> bool {
        // Capture dirty rect before removing the window.
        if let Some(w) = self.windows.iter().find(|w| w.id == id) {
            self.dirty_rects.push(window_dirty_rect(w));
        }
        let len_before = self.windows.len();
        self.windows.retain(|w| w.id != id);
        let removed = self.windows.len() != len_before;
        if removed && self.focused == Some(id) {
            let new_id = { self.windows.last().map(|w| w.id) };
            if let Some(nid) = new_id {
                self.focused = Some(nid);
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == nid) {
                    w.focused = true;
                    self.dirty_rects.push(window_dirty_rect(w));
                }
            } else { self.focused = None; }
        }
        removed
    }

    pub fn raise_to_top(&mut self, id: WindowId) {
        let Some(idx) = self.windows.iter().position(|w| w.id == id) else { return };
        // Record the previous focus before removing the window
        let prev_focus = self.focused;
        let mut window = self.windows.remove(idx);
        window.focused = true;
        // Mark the previously-focused window as unfocused and dirty
        if let Some(prev) = prev_focus {
            if prev != id {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev) {
                    w.focused = false;
                    self.dirty_rects.push(window_dirty_rect(w));
                }
            }
        }
        self.focused = Some(id);
        self.dirty_rects.push(window_dirty_rect(&window));
        self.windows.push(window);
    }

    pub fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        self.windows.iter().rev().find(|w| w.contains(x, y) || w.contains_title_bar(x, y)).map(|w| w.id)
    }

    pub fn resize_handle_at(&self, x: i32, y: i32) -> Option<WindowId> {
        self.windows.iter().rev().find(|w| {
            let rw = w.x + w.width as i32;
            let rh = w.y + w.height as i32;
            x >= rw - self.resize_handle as i32 && x < rw && y >= rh - self.resize_handle as i32 && y < rh
        }).map(|w| w.id)
    }

    pub fn on_mouse_down(&mut self, x: i32, y: i32) {
        if let Some(hit) = self.resize_handle_at(x, y) {
            self.raise_to_top(hit);
            if let Some(w) = self.windows.iter().find(|win| win.id == hit) {
                self.drag = DragState::Resizing { window: hit, orig_x: w.x, orig_y: w.y, orig_width: w.width, orig_height: w.height };
            }
            return;
        }
        for window in self.windows.iter().rev() {
            if window.contains_title_bar(x, y) {
                let id = window.id;
                self.raise_to_top(id);
                if let Some(w) = self.windows.iter().find(|win| win.id == id) {
                    self.drag = DragState::Moving { window: id, offset_x: x - w.x, offset_y: y - w.y };
                }
                return;
            }
        }
        if let Some(hit) = self.window_at(x, y) {
            self.raise_to_top(hit);
            self.drag = DragState::None;
            return;
        }
        self.focused = None;
        for w in &mut self.windows {
            w.focused = false;
            self.dirty_rects.push(window_dirty_rect(w));
        }
        self.drag = DragState::None;
    }

    pub fn on_mouse_move(&mut self, x: i32, y: i32) {
        match self.drag {
            DragState::Moving { window, offset_x, offset_y } => {
                // Capture dirty rect BEFORE mutating the window.
                let dirty_before = {
                    self.windows.iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == window) {
                    w.x = x - offset_x; w.y = y - offset_y;
                }
                let dirty_after = {
                    self.windows.iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(r) = dirty_before { self.dirty_rects.push(r); }
                if let Some(r) = dirty_after  { self.dirty_rects.push(r); }
            }
            DragState::Resizing { window, orig_x, orig_y, .. } => {
                // Capture dirty rect BEFORE mutating the window.
                let dirty_before = {
                    self.windows.iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == window) {
                    let nw = ((x - orig_x) as u32).max(MIN_WINDOW_W);
                    let nh = ((y - orig_y) as u32).max(MIN_WINDOW_H);
                    w.width = nw; w.height = nh;
                    w.surface = crate::surface::Surface::new(nw, nh, w.surface.get_pixel(0, 0).unwrap_or(0));
                }
                let dirty_after = {
                    self.windows.iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(r) = dirty_before { self.dirty_rects.push(r); }
                if let Some(r) = dirty_after  { self.dirty_rects.push(r); }
            }
            DragState::None => {}
        }
    }

    pub fn on_mouse_up(&mut self) { self.drag = DragState::None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wm() -> WindowManager {
        let mut wm = WindowManager::new();
        wm.create_window(0, 0, 100, 100, 0xFF0000);
        wm.create_window(50, 50, 100, 100, 0x0000FF);
        wm
    }

    #[test]
    fn test_window_at_topmost() {
        let wm = test_wm();
        assert_eq!(wm.window_at(60, 60), Some(WindowId(2)));
    }
    #[test]
    fn test_window_at_miss() {
        assert_eq!(test_wm().window_at(200, 200), None);
    }
    #[test]
    fn test_drag_moves_window() {
        let mut wm = test_wm();
        wm.on_mouse_down(60, 60);
        assert!(matches!(wm.drag, DragState::None));
        wm.on_mouse_up();
    }
    #[test]
    fn test_raise_on_click() {
        let mut wm = test_wm();
        wm.on_mouse_down(10, 10);
        assert_eq!(wm.windows.last().unwrap().id, WindowId(1));
        assert_eq!(wm.focused, Some(WindowId(1)));
    }
    #[test]
    fn test_cursor_outside_no_drag() {
        let mut wm = test_wm();
        wm.on_mouse_down(999, 999);
        assert!(matches!(wm.drag, DragState::None));
    }
    #[test]
    fn test_resize_handle() {
        let mut wm = WindowManager::new();
        wm.create_window(0, 0, 100, 100, 0xFF0000);
        assert_eq!(wm.resize_handle_at(95, 95), Some(WindowId(1)));
        assert_eq!(wm.resize_handle_at(50, 50), None);
    }
    #[test]
    fn test_resize_drag() {
        let mut wm = WindowManager::new();
        wm.create_window(0, 0, 100, 100, 0xFF0000);
        wm.on_mouse_down(95, 95);
        assert!(matches!(wm.drag, DragState::Resizing { window: WindowId(1), .. }));
        wm.on_mouse_move(150, 150);
        assert_eq!(wm.windows.iter().find(|w| w.id == WindowId(1)).unwrap().width, 150);
        assert_eq!(wm.windows.iter().find(|w| w.id == WindowId(1)).unwrap().height, 150);
        wm.on_mouse_up();
        assert!(matches!(wm.drag, DragState::None));
    }
    #[test]
    fn test_resize_min_size() {
        let mut wm = WindowManager::new();
        wm.create_window(0, 0, 100, 100, 0xFF0000);
        wm.on_mouse_down(95, 95);
        wm.on_mouse_move(10, 10);
        let w = wm.windows.iter().find(|w| w.id == WindowId(1)).unwrap();
        assert!(w.width >= MIN_WINDOW_W && w.height >= MIN_WINDOW_H);
    }
    #[test]
    fn test_title_bar_drag() {
        let mut wm = WindowManager::new();
        wm.create_titled_window(10, 10, 100, 100, 0xFF0000, "Test");
        // y=20 is inside title bar (y=10..30, TITLE_BAR_HEIGHT=20)
        wm.on_mouse_down(50, 20);
        assert!(matches!(wm.drag, DragState::Moving { window: WindowId(1), .. }));
        wm.on_mouse_move(100, 50);
        let w = wm.windows.iter().find(|w| w.id == WindowId(1)).unwrap();
        // offset=(50-10,20-10)=(40,10), new=(100-40,50-10)=(60,40)
        assert_eq!(w.x, 60);
        assert_eq!(w.y, 40);
        wm.on_mouse_up();
    }
    #[test]
    fn test_focus_transfer() {
        let mut wm = test_wm();
        assert_eq!(wm.focused, Some(WindowId(2)));
        wm.on_mouse_down(10, 10);
        assert_eq!(wm.focused, Some(WindowId(1)));
        assert!(!wm.windows.iter().find(|w| w.id == WindowId(2)).unwrap().focused);
    }
    #[test]
    fn test_dirty_rects_accumulated() {
        let mut wm = WindowManager::new();
        wm.create_window(10, 10, 100, 100, 0xFF0000);
        let rects = wm.consume_dirty_rects();
        assert!(!rects.is_empty());
        assert!(wm.consume_dirty_rects().is_empty());
    }
}