//! DisplayContext — framebuffer, window, cursor management.
//!
//! Aggregates the three display-related contexts that were previously
//! scattered across `FramebufferContext`, `WindowContext`, and
//! manual cursor tracking.

use super::framebuffer::FramebufferContext;
use super::window::WindowContext;

use spin::Mutex;

/// Cursor state (previously embedded in WindowContext).
#[derive(Debug, Clone, Copy)]
pub struct CursorContext {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
}

impl CursorContext {
    pub const fn new() -> Self {
        Self {
            visible: true,
            x: 512,
            y: 384,
        }
    }
}

/// Aggregated display context.
pub struct DisplayContext {
    /// Low-level framebuffer (GOP / VGA / VirtIO GPU)
    pub framebuffer: FramebufferContext,
    /// Window list, focus, z-order
    pub windows: WindowContext,
    /// Cursor state
    pub cursor: CursorContext,
}

// DisplayContext lives behind a Mutex; interior Send+Sync covered by sub-fields.
unsafe impl Send for DisplayContext {}
unsafe impl Sync for DisplayContext {}

impl DisplayContext {
    pub fn new() -> Self {
        Self {
            framebuffer: FramebufferContext::new(),
            windows: WindowContext::new(),
            cursor: CursorContext::new(),
        }
    }

    /// True when any display output is ready.
    pub fn is_available(&self) -> bool {
        self.framebuffer.is_available()
    }

    /// Synchronise cursor position between WindowContext and CursorContext.
    pub fn sync_cursor(&mut self) {
        self.cursor.x = self.windows.cursor_x;
        self.cursor.y = self.windows.cursor_y;
        self.cursor.visible = self.windows.cursor_visible;
    }
}

// ── Global singleton ──────────────────────────────────────────
static DISPLAY: Mutex<Option<DisplayContext>> = Mutex::new(None);

pub fn init_display() {
    *DISPLAY.lock() = Some(DisplayContext::new());
}

pub fn get_display() -> &'static Mutex<Option<DisplayContext>> {
    &DISPLAY
}

pub fn with_display_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut DisplayContext) -> R,
{
    DISPLAY.lock().as_mut().map(f)
}

pub fn with_display<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&DisplayContext) -> R,
{
    DISPLAY.lock().as_ref().map(f)
}