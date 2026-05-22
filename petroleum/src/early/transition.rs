//! # Early Transition: World Switch from Bootloader to Kernel
//!
//! **EARLY-ONLY**: This module is ONLY valid during the boot → kernel transition.
//! After the kernel is running in the higher half, this code must NOT be referenced.
//!
//! The world switch is the most critical phase boundary in the OS:
//! - Before: identity mapping, BootServices available, single-thread
//! - After:  higher-half paging, no firmware, interrupts, scheduler
//!
//! All state constructed here becomes **stale** after the CR3 reload.
//! The kernel must reconstruct its own GDT, IDT, allocator, etc.
//!
//! ## Usage (in bootloader/boot code)
//!
//! ```ignore
//! use petroleum::early::transition::{
//!     WorldSwitch, WorldSwitchBuilder, KernelTransition, UefiToHigherHalf,
//!     landing_zone_logic, TRANSITION_GDT, KERNEL_ARGS, TRANSITION_KERNEL_ENTRY,
//! };
//! ```
//!
//! ## Migration
//!
//! Currently re-exports from `crate::transition` for backward compatibility.
//! Over time, the actual implementation should migrate fully to this module.

// Re-export all items from the original transition module.
// This allows kernel boot code to use `petroleum::early::transition::*`
// while the original implementation still lives in `crate::transition`.
// Once the migration is complete, the original `transition.rs` will be removed.

pub use crate::transition::*;
