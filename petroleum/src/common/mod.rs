// Submodules for petroleum common utilities
pub mod bios;
pub mod error;
pub mod logging;
pub mod macros;
pub mod uefi;
pub mod utils;

// Re-exports to maintain compatibility and new macros
pub use bios::*;
pub use macros::*;
pub use error::*;
pub use uefi::*;
pub use utils::*;
