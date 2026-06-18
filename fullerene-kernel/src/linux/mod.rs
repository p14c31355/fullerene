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
//!                     Runtime::dispatch()
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

pub mod numbers;
pub mod types;
pub mod runtime;
pub mod fs;
pub mod memory;
pub mod process;
pub mod signal;
pub mod time;
pub mod misc;
pub mod launch;
pub mod test_binary;

pub use runtime::{Runtime, LinuxRuntime, DispatchMode, KernelRequest, KernelResponse, errno_code};
pub use numbers::*;
