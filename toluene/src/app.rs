//! Application runtime for Toluene SDK.
//!
//! Provides a simple event loop and application context that user-space
//! programs can use without dealing with raw syscalls.

use crate::sys;

/// Application context passed to the app's main function.
pub struct AppContext;

impl AppContext {
    /// Print a line to stdout.
    pub fn println(&self, s: &str) {
        sys::println(s);
    }

    /// Print to stdout without newline.
    pub fn print(&self, s: &str) {
        sys::print(s);
    }

    /// Get the current process ID.
    pub fn pid(&self) -> usize {
        sys::current_pid()
    }

    /// Yield to the scheduler.
    pub fn yield_now(&self) {
        sys::yield_now();
    }
}

/// Run a simple application function with an AppContext.
pub fn run_app<F: FnOnce(&AppContext)>(f: F) {
    let ctx = AppContext;
    f(&ctx);
}