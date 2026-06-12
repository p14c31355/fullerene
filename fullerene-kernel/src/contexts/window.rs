//! WindowContext — Window management state for the desktop.
//!
//! Consolidates:
//! - Window list, focus, z-order
//! - Cursor state (position, visibility)
//! - This is intentionally thin; the heavy window-manager logic lives
//!   in `lattice::wm` and `lattice::desktop`.  This context holds the
//!   kernel-side state that crosses the kernel/user boundary.
//!
//! # Design
//!
//! ```rust,ignore
//! let focus = window_ctx.focused();
//! window_ctx.raise(win_id);
//! ```

use alloc::vec::Vec;
use spin::Mutex;

/// A window identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const INVALID: Self = Self(0);
}

/// Window state tracked by the kernel.
#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub title: alloc::string::String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub z: i32,
}

impl Window {
    pub fn new(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            id,
            title: alloc::string::String::from(title),
            x,
            y,
            width,
            height,
            visible: true,
            z: 0,
        }
    }
}

/// Kernel-side window management context.
///
/// The actual compositing and event routing lives in `solvent` and
/// `lattice`.  This context is the kernel's view of window state.
pub struct WindowContext {
    /// Ordered list of windows (by z-order, ascending).
    pub windows: Vec<Window>,

    /// Currently focused window, or `INVALID`.
    pub focused: WindowId,

    /// Cursor visible flag.
    pub cursor_visible: bool,

    /// Cursor position (screen coordinates).
    pub cursor_x: i32,
    pub cursor_y: i32,

    /// Next window ID to assign.
    next_id: u64,
}

impl WindowContext {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            focused: WindowId::INVALID,
            cursor_visible: true,
            cursor_x: 512,
            cursor_y: 384,
            next_id: 1,
        }
    }

    /// Allocate a new window ID.
    pub fn next_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Add a window at the top of the z-order.
    pub fn add_window(&mut self, mut win: Window) {
        let max_z = self.windows.iter().map(|w| w.z).max().unwrap_or(0);
        win.z = max_z + 1;
        self.windows.push(win);
    }

    /// Remove a window by ID.
    pub fn remove_window(&mut self, id: WindowId) {
        self.windows.retain(|w| w.id != id);
        if self.focused == id {
            self.focused = WindowId::INVALID;
        }
    }

    /// Focus a window (raise to top).
    pub fn focus(&mut self, id: WindowId) {
        if self.focused == id {
            return;
        }
        self.focused = id;
        // Compute max_z before mutating to avoid borrow conflicts.
        let max_z = self.windows.iter().map(|w| w.z).max().unwrap_or(0);
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
            win.z = max_z + 1;
        }
    }

    /// Get the focused window, if any.
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == self.focused)
    }

    /// Return the window at screen coordinate (x, y), respecting z-order (top first).
    pub fn window_at(&self, x: i32, y: i32) -> Option<&Window> {
        let mut candidates: Vec<&Window> = self
            .windows
            .iter()
            .filter(|w| {
                w.visible
                    && x >= w.x
                    && x < w.x + w.width as i32
                    && y >= w.y
                    && y < w.y + w.height as i32
            })
            .collect();
        candidates.sort_by_key(|w| -w.z);
        candidates.into_iter().next()
    }

    /// Window count.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Is the window list empty?
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

/// Global window context.
static WINDOW_CONTEXT: Mutex<Option<WindowContext>> = Mutex::new(None);

/// Initialise the global window context.
pub fn init_window_context() {
    *WINDOW_CONTEXT.lock() = Some(WindowContext::new());
}

/// Get a reference to the global window context.
pub fn get_window_context() -> &'static Mutex<Option<WindowContext>> {
    &WINDOW_CONTEXT
}

/// Convenience: execute a closure with a mutable reference.
pub fn with_window_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut WindowContext) -> R,
{
    WINDOW_CONTEXT.lock().as_mut().map(f)
}

/// Convenience: execute a closure with a shared reference.
pub fn with_window<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&WindowContext) -> R,
{
    WINDOW_CONTEXT.lock().as_ref().map(f)
}