//! Kernel-level memory mapping.
//!
//! Provides the high-level mapper and kernel space initialization.

pub mod direct_map;
pub mod init;
pub mod mapper;

// Re-export the main types
pub use mapper::{Mapper, MapError, RegionBuilder};