//! Boot module containing UEFI and BIOS entry points and boot-specific logic

// Submodules for boot functionality
pub mod constants;
#[macro_use]
pub mod macros;
pub mod utils;
pub mod uefi_entry;
pub mod bios_entry;

// Re-exports for compatibility
pub use constants::*;
pub use uefi_entry::*;
pub use bios_entry::*;
