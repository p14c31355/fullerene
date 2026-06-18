//! ShellContext — unified shell state for the kernel's shell subsystem.
//!
//! Bundles shell-related state that was previously scattered across:
//! - `shell.rs` (nozzle hooks, kernel terminal)
//! - `scheduler.rs` (LAUNCH_SHELL flag)
//!
//! The nozzle hooks (FsHooks, SysHooks) are installed once during init,
//! but the context struct provides a clear mental model of shell state.

use alloc::string::String;
use spin::Mutex;

// ── ShellContext ────────────────────────────────────────────────────

/// Kernel shell context.
///
/// Holds the state needed by the shell subsystem: current working
/// directory and the launch-on-demand flag.
pub struct ShellContext {
    /// Current working directory (mirrors VFS cwd for quick access).
    pub cwd: Mutex<String>,

    /// Whether a shell launch has been requested (by AppGrid / menu).
    /// Set by `request_launch()`, consumed by the scheduler loop.
    pub launch_requested: Mutex<bool>,

    /// Whether the shell subsystem has been initialised.
    pub initialized: Mutex<bool>,
}

unsafe impl Send for ShellContext {}
unsafe impl Sync for ShellContext {}

impl ShellContext {
    pub fn new() -> Self {
        Self {
            cwd: Mutex::new(String::from("/")),
            launch_requested: Mutex::new(false),
            initialized: Mutex::new(false),
        }
    }

    /// Request a shell launch (called from AppGrid / solvent callback).
    pub fn request_launch(&self) {
        *self.launch_requested.lock() = true;
    }

    /// Check and clear the launch request flag.
    pub fn take_launch_request(&self) -> bool {
        let mut flag = self.launch_requested.lock();
        let was = *flag;
        *flag = false;
        was
    }
}

// The canonical ShellContext lives inside KernelContext.shell.
// No separate global singleton is needed — use `kernel.shell` instead.
