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

pub use fullerene_abi as abi;

pub mod app;
pub mod calc;
pub mod clock;
pub mod exec;
pub mod sys;
pub mod ui;

/// SDK version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
