// Submodules for petroleum common utilities
pub mod macros;
pub mod uefi;
pub mod bios;
pub mod utils;
pub mod error;

// Re-exports to maintain compatibility
pub use bios::*;
pub use uefi::*;
pub use utils::*;
pub use error::*;
