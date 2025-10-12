//! Boot module containing UEFI and BIOS entry points and boot-specific logic

// Macro to reduce repetitive serial logging
macro_rules! kernel_log {
    ($($arg:tt)*) => {{
        let mut serial = petroleum::serial::SERIAL_PORT_WRITER.lock();
        let _ = core::fmt::write(&mut *serial, format_args!($($arg)*));
        let _ = core::fmt::write(&mut *serial, format_args!("\n"));
    }};
}

// Submodules for boot functionality
pub mod constants;
pub mod macros;
pub mod utils;
pub mod uefi_entry;
pub mod bios_entry;

// Re-exports for compatibility
pub use constants::*;
pub use uefi_entry::*;
pub use bios_entry::*;
