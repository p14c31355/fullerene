//! GuiContext — unified GUI / compositor state.
//!
//! Bundles the GUI-related state that was previously scattered across:
//! - `gui.rs` (framebuffer slice, renderer, solvent bridge)
//! - `solvent` crate (compositor, theme, wallpaper, cursor state)
//!
//! The context provides a single entry point for the scheduler to say
//! `kernel.gui.render()` rather than calling scattered free functions.

use spin::Mutex;

// ── GuiContext ──────────────────────────────────────────────────────

/// Kernel GUI / compositor context.
///
/// Owns the high-level GUI state: initialization flags and framebuffer
/// dimensions.  The actual rendering is delegated to `solvent` via the
/// framebuffer slice held in `FramebufferContext`.
pub struct GuiContext {
    /// Whether the GUI subsystem has been initialised.
    pub initialized: Mutex<bool>,

    /// Whether the desktop has been rendered at least once.
    pub desktop_shown: Mutex<bool>,

    /// Framebuffer dimensions (pixels).  Set after GOP init.
    pub width: Mutex<u32>,
    pub height: Mutex<u32>,
}

impl GuiContext {
    pub fn new() -> Self {
        Self {
            initialized: Mutex::new(false),
            desktop_shown: Mutex::new(false),
            width: Mutex::new(0),
            height: Mutex::new(0),
        }
    }
}

// The canonical GuiContext lives inside KernelContext.gui.
// No separate global singleton is needed — use `kernel.gui` instead.
