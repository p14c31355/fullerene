#![no_std]

//! Toluene — Fullerene OS Userspace SDK
//!
//! Provides high-level APIs for building Fullerene desktop applications:
//! - System info (PID, memory, processes)
//! - File I/O (read, write, list, create)
//! - GUI primitives (window creation, drawing)
//! - Shell command execution
//! - Theme / wallpaper management
//! - Clock and calendar utilities
//! - Calculator engine
//!
//! # Example
//!
//! ```ignore
//! use toluene::sys::getpid;
//! use toluene::app::run_app;
//!
//! run_app(|ctx| {
//!     ctx.println("Hello from Toluene!");
//! });
//! ```

extern crate alloc;

pub mod app;
pub mod calc;
pub mod clock;
pub mod exec;
pub mod sys;
pub mod ui;

/// Simple add function for integrated testing (legacy).
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// SDK version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");