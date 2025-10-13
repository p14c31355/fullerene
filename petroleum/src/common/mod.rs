// Submodules for petroleum common utilities
pub mod bios;
pub mod error;
pub mod macros;
pub mod uefi;
pub mod utils;

// Re-exports to maintain compatibility
pub use bios::*;
pub use error::*;
pub use uefi::*;
pub use utils::*;
