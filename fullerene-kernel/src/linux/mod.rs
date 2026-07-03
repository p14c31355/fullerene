//! Linux ABI emulation layer for Fullerene.
//!
//! This module implements a Linux x86_64 syscall ABI translation layer.
//! Linux ELF binaries loaded through the loader get attached to a
//! `LinuxRuntime` which translates Linux syscalls into Fullerene kernel
//! operations.
//!
//! # Architecture
//!
//! ```text
//! Linux ELF → Loader → Process + LinuxRuntime
//!                          │
//!                     syscall instruction
//!                          │
//!                     handle_syscall()
//!                          │
//!                     LinuxRuntime::dispatch()
//!                          │
//!                    ┌─────┴──────┐
//!                    │  fs  │ mem │
//!                    │ proc │ sig │
//!                    │ time │ misc│
//!                    └────────────┘
//!                          │
//!                    Kernel services
//!                    (VFS, process, memory)
//! ```

pub mod fs;
pub mod launch;
pub mod memory;
pub mod misc;
pub mod numbers;
pub mod process;
pub mod runtime;
pub mod signal;
pub mod test_binary;
pub mod time;
pub mod types;

pub use numbers::*;
pub use runtime::{DispatchMode, LinuxRuntime, errno_code};
