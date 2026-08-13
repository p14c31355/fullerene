//! ShellContext — compatibility state for kernel-side shell smoke paths.
//!
//! Bundles shell-related state that was previously scattered across:
//! - `shell.rs` (nozzle hooks, kernel terminal)
//! - `scheduler.rs` (legacy launch callback)
//!
//! The nozzle hooks (FsHooks, SysHooks) are installed once during init,
//! but the context struct provides a clear mental model of shell state.

use alloc::string::String;
use spin::Mutex;

// ── ShellContext ────────────────────────────────────────────────────

/// Kernel shell context.
///
/// Holds the state needed by kernel shell test/support paths. The production
/// shell is a user ELF owned by launchd and does not use this context.
pub struct ShellContext {
    /// Current working directory (mirrors VFS cwd for quick access).
    pub cwd: Mutex<String>,

    /// Whether the shell subsystem has been initialised.
    pub initialized: Mutex<bool>,
}

unsafe impl Send for ShellContext {}
unsafe impl Sync for ShellContext {}

impl ShellContext {
    pub fn new() -> Self {
        Self {
            cwd: Mutex::new(String::from("/")),
            initialized: Mutex::new(false),
        }
    }
}

// The canonical ShellContext lives inside KernelContext.shell.
// No separate global singleton is needed — use `kernel.shell` instead.
