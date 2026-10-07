//! Common utilities shared across petroleum submodules.

// x86-only glue; see `x86_64/mod.rs` for what lives there.
#[cfg(not(target_arch = "aarch64"))]
mod x86_64;
#[cfg(not(target_arch = "aarch64"))]
pub use x86_64::*;

/// A custom error type for the bootloader (UEFI/BIOS).
#[derive(Debug, Clone, Copy)]
pub enum BellowsError {
    Efi { status: uefi::EfiStatus },
    FileIo(&'static str),
    PeParse(&'static str),
    AllocationFailed(&'static str),
    InvalidState(&'static str),
    ProtocolNotFound(&'static str),
}

impl From<uefi::EfiStatus> for BellowsError {
    fn from(status: uefi::EfiStatus) -> Self {
        Self::Efi { status }
    }
}

pub type Result<T> = core::result::Result<T, BellowsError>;

pub mod logging;
#[macro_use]
pub mod macros;
pub mod memory;
pub mod syscall;
pub mod uefi;
pub mod utils;

#[repr(C)]
pub struct VgaFramebufferConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

use core::sync::atomic::{AtomicBool, Ordering};

static MEMORY_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn check_memory_initialized() -> bool {
    MEMORY_INITIALIZED.load(Ordering::SeqCst)
}

pub fn set_memory_initialized(initialized: bool) {
    MEMORY_INITIALIZED.store(initialized, Ordering::SeqCst);
}

pub use macros::*;
pub use memory::*;
pub use syscall::*;
pub use uefi::*;
pub use utils::*;
