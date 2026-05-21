use crate::window::{Window, WindowId};

/// Drag state machine.
///
/// The WM owns exactly one `DragState` — no need for a complex event system yet.
#[derive(Debug, Clone)]
pub enum DragState {
    /// No window is being dragged.
    None,
    /// A window is being moved.
    Moving {
        window: WindowId,
        /// Offset from the window's top‑left to the mouse cursor
        /// (so the window doesn't jump to the cursor on grab).
        offset_x: i32,
        offset_y: i32,
    },
}

/// Window manager — owns all windows and their z‑order.
///
/// The WM is deliberately **stateless with respect to rendering**.
/// It does not touch the framebuffer; it only manages logical state:
///
/// - Window positions & sizes
/// - Z‑order (vector order = bottom to top)
/// - Hit testing
/// - Focus
/// - Drag gesture state
pub struct WindowManager {
    /// Windows in z‑order: index 0 = bottom (backmost), last = top.
    windows: Vec<Window>,
    /// Currently focused window ID (bottom of the z‑order? no — top).
    focused: Option<WindowId>,
    /// Next ID to assign.
    next_id: u64,
    /// Current drag state.
    drag: DragState,
}

impl WindowManager {
    /// Create an empty window manager.
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            focused: None,
            next_id: 1,
            drag: DragState::None,
        }
    }

    // ── window list access ───────────────────────────────────

    /// All windows, bottom (index 0) to top.
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

    // ── window lifecycle ─────────────────────────────────────

    /// Create a new window and push it to the top of the z‑order.
    pub fn create_window(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;

        let window = Window::new(id, x, y, width, height, color);
        self.windows.push(window);
        self.focused = Some(id);
        id
    }

    /// Remove a window by ID. Returns `true` if it existed.
    pub fn remove_window(&mut self, id: WindowId) -> bool {
        let len_before = self.windows.len();
        self.windows.retain(|w| w.id != id);
        let removed = self.windows.len() != len_before;

        if removed && self.focused == Some(id) {
            // Focus the topmost remaining window, if any.
            self.focused = self.windows.last().map(|w| w.id);
        }

        removed
    }

    // ── z‑order ──────────────────────────────────────────────

    /// Raise `id` to the top of the z‑order.
    pub fn raise_to_top(&mut self, id: WindowId) {
        let Some(idx) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let window = self.windows.remove(idx);
        self.windows.push(window);
        self.focused = Some(id);
    }

    // ── hit testing ──────────────────────────────────────────

    /// Return the topmost window containing (x, y), or `None`.
    ///
    /// Iterates in **reverse** z‑order (topmost first) so the front window
    /// wins when windows overlap.
    pub fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        self.windows
            .iter()
            .rev()
            .find(|w| w.contains(x, y))
            .map(|w| w.id)
    }

    // ── dragging ─────────────────────────────────────────────

    /// Called on mouse button press.
    ///
    /// If `(x, y)` hits a window, begin dragging it.
    /// Otherwise do nothing.
    pub fn on_mouse_down(&mut self, x: i32, y: i32) {
        let Some(hit) = self.window_at(x, y) else {
            self.drag = DragState::None;
            return;
        };

        // Raise the clicked window to the top.
        self.raise_to_top(hit);

        // Record the grab offset so the window doesn't jump.
        let offset = self.windows.iter().rev().find(|w| w.id == hit).map(|w| {
            (x - w.x, y - w.y)
        });

        if let Some((ox, oy)) = offset {
            self.drag = DragState::Moving {
                window: hit,
                offset_x: ox,
                offset_y: oy,
            };
        }
    }

    /// Called on mouse move.
    pub fn on_mouse_move(&mut self, x: i32, y: i32) {
        match self.drag {
            DragState::Moving { window, offset_x, offset_y } => {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == window) {
                    w.x = x - offset_x;
                    w.y = y - offset_y;
                }
            }
            DragState::None => {}
        }
    }

    /// Called on mouse button release.
    pub fn on_mouse_up(&mut self) {
        self.drag = DragState::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wm() -> WindowManager {
        let mut wm = WindowManager::new();
        wm.create_window(0, 0, 100, 100, 0xFF0000); // red
        wm.create_window(50, 50, 100, 100, 0x0000FF); // blue
        wm
    }

    #[test]
    fn test_window_at_topmost() {
        let wm = test_wm();
        // Point (60, 60) is inside both windows; should return the topmost (blue, id=2)
        let hit = wm.window_at(60, 60);
        assert_eq!(hit, Some(WindowId(2)));
    }

    #[test]
    fn test_window_at_miss() {
        let wm = test_wm();
        assert_eq!(wm.window_at(200, 200), None);
    }

    #[test]
    fn test_drag_moves_window() {
        let mut wm = test_wm();
        let blue = WindowId(2);

        // Click on the blue window (topmost) at (60, 60).
        // offset = (60-50, 60-50) = (10, 10)
        wm.on_mouse_down(60, 60);
        assert!(matches!(wm.drag, DragState::Moving { window, .. } if window == blue));

        // Drag to (100, 100) — blue should move so its top‑left is at (90, 90)
        wm.on_mouse_move(100, 100);
        let blue_w = wm.windows.iter().find(|w| w.id == blue).unwrap();
        assert_eq!(blue_w.x, 90);
        assert_eq!(blue_w.y, 90);

        // Release
        wm.on_mouse_up();
        assert!(matches!(wm.drag, DragState::None));
    }

    #[test]
    fn test_raise_on_click() {
        let mut wm = test_wm();
        // Click on the red window (bottom) at (10, 10) — it should come to the top
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
}