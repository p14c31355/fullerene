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
/// Owns the high-level GUI state: compositor readiness, theme variant,
/// wallpaper mode, and cursor shape.  The actual rendering is delegated
/// to `solvent` via the framebuffer slice held in `FramebufferContext`.
pub struct GuiContext {
    /// Whether the GUI subsystem has been initialised.
    pub initialized: Mutex<bool>,

    /// Whether the desktop has been rendered at least once.
    pub desktop_shown: Mutex<bool>,

    /// Framebuffer dimensions (pixels).  Set after GOP init.
    pub width: Mutex<u32>,
    pub height: Mutex<u32>,
}

unsafe impl Send for GuiContext {}
unsafe impl Sync for GuiContext {}

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

// ── Global singleton ────────────────────────────────────────────────

static GUI_CTX: Mutex<Option<GuiContext>> = Mutex::new(None);

/// Initialise the global GuiContext.
pub fn init_gui_ctx() {
    *GUI_CTX.lock() = Some(GuiContext::new());
}

/// Get the global GuiContext.
pub fn get_gui() -> &'static Mutex<Option<GuiContext>> {
    &GUI_CTX
}

/// Execute a closure over the GuiContext.
pub fn with_gui<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&GuiContext) -> R,
{
    GUI_CTX.lock().as_ref().map(f)
}