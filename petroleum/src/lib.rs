#![no_std]

extern crate alloc;

pub const FALLBACK_HEAP_START_ADDR: u64 = 0x100000;

#[macro_use]
pub mod common;
pub mod arch;
pub mod error;
pub mod graphics;
pub mod page_table;

// Platform surface: shared code names the platform layer through the crate
// root (`crate::serial`, `crate::halt_loop`, ...). Which tree answers is
// decided once, by directory, in `arch/` - the cfg never leaks past here.
#[cfg(target_arch = "aarch64")]
pub use arch::aarch64::*;
#[cfg(not(target_arch = "aarch64"))]
pub use arch::x86_64::*;

pub use common::logging::{SystemError, SystemResult};
pub use common::memory::*;
pub use common::syscall::*;
pub use common::{check_memory_initialized, set_memory_initialized};
pub use error::MemoryError;
pub use graphics::*;
pub use page_table::page_buf::PageBuf;

use crate::common::EfiSystemTable;
use crate::common::uefi::FullereneFramebufferConfig;
use spin::{Mutex, Once};

pub static FULLERENE_FRAMEBUFFER_CONFIG: Once<Mutex<Option<FullereneFramebufferConfig>>> =
    Once::new();

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub usize);

impl UefiSystemTablePtr {
    /// Return the firmware pointer at the narrow UEFI boundary.
    pub unsafe fn as_ptr(self) -> *mut EfiSystemTable {
        self.0 as *mut EfiSystemTable
    }
}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

pub fn init_uefi_system_table(system_table: *mut EfiSystemTable) {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table as usize));
}

pub fn clear_line_range<B: TextBufferOperations + ?Sized>(
    buffer: &mut B,
    start_row: usize,
    end_row: usize,
    col_start: usize,
    col_end: usize,
    blank_char: ScreenChar,
) {
    buffer_ops!(
        clear_line_range,
        buffer,
        start_row,
        end_row,
        col_start,
        col_end,
        blank_char
    );
}
