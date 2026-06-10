extern crate alloc;

use crate::scene::DirtyRect;
use crate::window::{Window, WindowId};
use alloc::string::String;
use alloc::vec::Vec;

/// Tiling layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilingMode {
    /// Windows are free‑floating (default).
    Floating,
    /// Master‑stack layout: focused window on the left, others stacked on the right.
    MasterStack,
}

impl TilingMode {
    pub fn toggle(&self) -> Self {
        match self {
            TilingMode::Floating => TilingMode::MasterStack,
            TilingMode::MasterStack => TilingMode::Floating,
        }
    }
}

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
    /// Current tiling mode.
    pub tiling_mode: TilingMode,
    /// Saved floating positions for tiled windows: (x, y, w, h).
    floating_restore: alloc::collections::BTreeMap<WindowId, (i32, i32, u32, u32)>,
    /// Cached work area dimensions (width, height) for retiling.
    work_area: Option<(u32, u32)>,
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
        Self {
            windows: Vec::new(),
            focused: None,
            next_id: 1,
            drag: DragState::None,
            resize_handle: RESIZE_HANDLE_SIZE,
            dirty_rects: Vec::new(),
            tiling_mode: TilingMode::Floating,
            floating_restore: alloc::collections::BTreeMap::new(),
            work_area: None,
        }
    }

    /// Set the work area dimensions for retiling.
    pub fn set_work_area(&mut self, width: u32, height: u32) {
        self.work_area = Some((width, height));
    }

    pub fn windows(&self) -> &[Window] {
        &self.windows
    }
    pub fn windows_mut(&mut self) -> &mut [Window] {
        &mut self.windows
    }
    pub fn drag_state(&self) -> &DragState {
        &self.drag
    }
    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }

    // ── dirty rects ─────────────────────────────────────────

    /// Consume all accumulated dirty rects and clear the internal list.
    pub fn consume_dirty_rects(&mut self) -> Vec<DirtyRect> {
        let mut out = Vec::new();
        core::mem::swap(&mut out, &mut self.dirty_rects);
        out
    }

    /// Returns `true` if there are pending dirty rectangles that need
    /// compositing.
    pub fn has_dirty_rects(&self) -> bool {
        !self.dirty_rects.is_empty()
    }

    pub fn create_window(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        color: u32,
    ) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        self.create_with_id(id, x, y, width, height, color)
    }

    fn create_with_id(
        &mut self,
        id: WindowId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        color: u32,
    ) -> WindowId {
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

    pub fn create_titled_window(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        color: u32,
        title: impl Into<String>,
    ) -> WindowId {
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

        // Retile if in MasterStack mode
        if self.tiling_mode == TilingMode::MasterStack {
            if let Some((ww, wh)) = self.work_area {
                self.retile(ww, wh);
            }
        }

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
            } else {
                self.focused = None;
            }
        }

        // Retile if in MasterStack mode
        if removed && self.tiling_mode == TilingMode::MasterStack {
            if let Some((ww, wh)) = self.work_area {
                self.retile(ww, wh);
            }
        }

        removed
    }

    pub fn raise_to_top(&mut self, id: WindowId) {
        let Some(idx) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
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

        // Retile if in MasterStack mode (Z-order changed)
        if self.tiling_mode == TilingMode::MasterStack {
            if let Some((ww, wh)) = self.work_area {
                self.retile(ww, wh);
            }
        }
    }

    pub fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        self.windows
            .iter()
            .rev()
            .find(|w| w.contains(x, y) || w.contains_title_bar(x, y))
            .map(|w| w.id)
    }

    pub fn resize_handle_at(&self, x: i32, y: i32) -> Option<WindowId> {
        self.windows
            .iter()
            .rev()
            .find(|w| {
                let rw = w.x + w.width as i32;
                let rh = w.y + w.height as i32;
                x >= rw - self.resize_handle as i32
                    && x < rw
                    && y >= rh - self.resize_handle as i32
                    && y < rh
            })
            .map(|w| w.id)
    }

    pub fn on_mouse_down(&mut self, x: i32, y: i32) {
        if let Some(hit) = self.resize_handle_at(x, y) {
            self.raise_to_top(hit);
            if let Some(w) = self.windows.iter().find(|win| win.id == hit) {
                self.drag = DragState::Resizing {
                    window: hit,
                    orig_x: w.x,
                    orig_y: w.y,
                    orig_width: w.width,
                    orig_height: w.height,
                };
            }
            return;
        }
        for window in self.windows.iter().rev() {
            if window.contains_title_bar(x, y) {
                let id = window.id;
                self.raise_to_top(id);
                if let Some(w) = self.windows.iter().find(|win| win.id == id) {
                    self.drag = DragState::Moving {
                        window: id,
                        offset_x: x - w.x,
                        offset_y: y - w.y,
                    };
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
            DragState::Moving {
                window,
                offset_x,
                offset_y,
            } => {
                // Capture dirty rect BEFORE mutating the window.
                let dirty_before = {
                    self.windows
                        .iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == window) {
                    w.x = x - offset_x;
                    w.y = y - offset_y;
                }
                let dirty_after = {
                    self.windows
                        .iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(r) = dirty_before {
                    self.dirty_rects.push(r);
                }
                if let Some(r) = dirty_after {
                    self.dirty_rects.push(r);
                }
            }
            DragState::Resizing {
                window,
                orig_x,
                orig_y,
                ..
            } => {
                // Capture dirty rect BEFORE mutating the window.
                let dirty_before = {
                    self.windows
                        .iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == window) {
                    let nw = ((x - orig_x) as u32).max(MIN_WINDOW_W);
                    let nh = ((y - orig_y) as u32).max(MIN_WINDOW_H);
                    w.width = nw;
                    w.height = nh;
                    w.surface = crate::surface::Surface::new(
                        nw,
                        nh,
                        w.surface.get_pixel(0, 0).unwrap_or(0),
                    );
                }
                let dirty_after = {
                    self.windows
                        .iter()
                        .find(|w| w.id == window)
                        .map(window_dirty_rect)
                };
                if let Some(r) = dirty_before {
                    self.dirty_rects.push(r);
                }
                if let Some(r) = dirty_after {
                    self.dirty_rects.push(r);
                }
            }
            DragState::None => {}
        }
    }

    pub fn on_mouse_up(&mut self) {
        self.drag = DragState::None;
    }

    // ── Window actions ─────────────────────────────────────

    /// Minimize a window (hide it).  Focus moves to the next visible window.
    pub fn minimize_window(&mut self, id: WindowId) -> bool {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        if w.minimized {
            return false;
        }
        w.minimized = true;
        w.focused = false;
        self.dirty_rects.push(window_dirty_rect(w));

        // Move focus to the last visible window.
        if self.focused == Some(id) {
            let new_focus = self
                .windows
                .iter()
                .rev()
                .find(|w| !w.minimized && w.id != id)
                .map(|w| w.id);
            self.focused = new_focus;
            if let Some(nid) = new_focus {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == nid) {
                    w.focused = true;
                    self.dirty_rects.push(window_dirty_rect(w));
                }
            }
        }

        // Retile if in MasterStack mode (visibility changed)
        if self.tiling_mode == TilingMode::MasterStack {
            if let Some((ww, wh)) = self.work_area {
                self.retile(ww, wh);
            }
        }

        true
    }

    /// Restore a minimized window.
    pub fn restore_window(&mut self, id: WindowId) -> bool {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        if !w.minimized {
            return false;
        }
        w.minimized = false;
        self.raise_to_top(id);

        // Note: raise_to_top already calls retile if needed, so no need to call it again here

        true
    }

    /// Toggle maximize for a window.
    ///
    /// The window is moved to 0,0 and expanded to fill the work area.
    /// Surface is **not** resized — the compositor clips the surface
    /// at its native size.  The caller (render loop) should adjust the
    /// terminal grid (cols × rows) to match the new window dimensions
    /// and recreate the surface only for the grid, not for the full
    /// window.  This avoids OOM on small heaps (e.g. 4 MiB kernel heap).
    pub fn toggle_maximize(&mut self, id: WindowId, work_width: u32, work_height: u32) -> bool {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        match w.maximized {
            false => {
                w.restore_rect = Some((w.x, w.y, w.width, w.height));
                w.x = 0;
                w.y = 0;
                w.width = work_width;
                w.height = work_height;
                w.maximized = true;
            }
            true => {
                if let Some((rx, ry, rw, rh)) = w.restore_rect.take() {
                    w.x = rx;
                    w.y = ry;
                    w.width = rw;
                    w.height = rh;
                }
                w.maximized = false;
            }
        }
        self.dirty_rects.push(window_dirty_rect(w));
        true
    }

    /// Close (remove) a window.
    pub fn close_window(&mut self, id: WindowId) -> bool {
        self.remove_window(id)
    }

    /// Get a list of window IDs in Z-order (bottom to top).
    pub fn z_order(&self) -> alloc::vec::Vec<WindowId> {
        self.windows.iter().map(|w| w.id).collect()
    }

    // ── Tiling ────────────────────────────────────────────

    /// Toggle tiling mode and re‑arrange windows.
    pub fn toggle_tiling(&mut self) {
        match self.tiling_mode {
            TilingMode::Floating => {
                self.tiling_mode = TilingMode::MasterStack;
            }
            TilingMode::MasterStack => {
                self.tiling_mode = TilingMode::Floating;
                // Restore floating positions
                self.restore_floating();
                return;
            }
        }
    }

    /// Arrange visible (non‑minimized) windows in master‑stack layout.
    ///
    /// Called after any window operation that may change the set of
    /// visible windows: create, close, focus change, or maximize toggle.
    ///
    /// `work_width` / `work_height` define the usable area (screen minus taskbar).
    pub fn retile(&mut self, work_width: u32, work_height: u32) {
        if self.tiling_mode != TilingMode::MasterStack {
            return;
        }

        let visible: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|w| !w.minimized)
            .map(|w| w.id)
            .collect();

        if visible.is_empty() {
            return;
        }

        // Save floating positions before tiling.
        for &id in &visible {
            self.floating_restore.entry(id).or_insert_with(|| {
                self.windows
                    .iter()
                    .find(|w| w.id == id)
                    .map(|w| (w.x, w.y, w.width, w.height))
                    .unwrap_or((0, 0, 80, 40))
            });
        }

        let margin = 4u32;
        let gap = 4u32;
        let left = margin as i32;
        let top = margin as i32;
        let usable_w = work_width.saturating_sub(margin * 2);
        let usable_h = work_height.saturating_sub(margin * 2);

        // Master occupies left 60%
        let master_w = usable_w * 3 / 5;
        let stack_w = usable_w - master_w - gap;
        let stack_left = left + master_w as i32 + gap as i32;

        // Master window = last in Z‑order (most recently focused).
        let master_id = *visible.last().unwrap();

        if let Some(mw) = self.windows.iter_mut().find(|w| w.id == master_id) {
            mw.x = left;
            mw.y = top;
            mw.width = master_w;
            mw.height = usable_h;
            self.dirty_rects.push(window_dirty_rect(mw));
        }

        // Stack: remaining windows tiled vertically on the right.
        let stack_ids: Vec<WindowId> = visible
            .iter()
            .copied()
            .filter(|&id| id != master_id)
            .collect();

        if !stack_ids.is_empty() {
            let count = stack_ids.len() as u32;
            let each_h = (usable_h.saturating_sub(gap * (count.saturating_sub(1)))) / count;

            for (i, &id) in stack_ids.iter().enumerate() {
                let stack_y = top + ((i as u32) * (each_h + gap)) as i32;
                if let Some(sw) = self.windows.iter_mut().find(|w| w.id == id) {
                    sw.x = stack_left;
                    sw.y = stack_y;
                    sw.width = stack_w;
                    sw.height = each_h;
                    self.dirty_rects.push(window_dirty_rect(sw));
                }
            }
        }
    }

    /// Restore windows to their saved floating positions and clear the cache.
    fn restore_floating(&mut self) {
        if self.floating_restore.is_empty() {
            return;
        }
        for (&id, &(x, y, w, h)) in &self.floating_restore {
            if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
                win.x = x;
                win.y = y;
                win.width = w;
                win.height = h;
                self.dirty_rects.push(window_dirty_rect(win));
            }
        }
        self.floating_restore.clear();
    }
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
        assert!(matches!(
            wm.drag,
            DragState::Resizing {
                window: WindowId(1),
                ..
            }
        ));
        wm.on_mouse_move(150, 150);
        assert_eq!(
            wm.windows
                .iter()
                .find(|w| w.id == WindowId(1))
                .unwrap()
                .width,
            150
        );
        assert_eq!(
            wm.windows
                .iter()
                .find(|w| w.id == WindowId(1))
                .unwrap()
                .height,
            150
        );
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
        assert!(matches!(
            wm.drag,
            DragState::Moving {
                window: WindowId(1),
                ..
            }
        ));
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
        assert!(
            !wm.windows
                .iter()
                .find(|w| w.id == WindowId(2))
                .unwrap()
                .focused
        );
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
