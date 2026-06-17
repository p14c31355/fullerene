//! Kernel tracing — delegates to `resonance::tracing`.
//!
//! The trace buffer previously lived in this module and has been moved to
//! the `resonance` crate per AGENTS.md §4 (Resonance owns event definitions
//! and tracing primitives).

// Re-export the entire resonance tracing module so existing `crate::tracing::*`
// imports continue to work.
pub use resonance::tracing::*;
