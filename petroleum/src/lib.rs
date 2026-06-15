#![no_std]

extern crate alloc;

pub const FALLBACK_HEAP_START_ADDR: u64 = 0x100000;

pub mod assembly;
pub mod bare_metal_graphics_detection;
pub mod bare_metal_pci;
pub mod early;
#[macro_use]
pub mod common;
pub mod boot;
pub mod debug;
pub mod filesystem;
pub mod graphics;
pub mod graphics_alternatives;
pub mod hardware;
pub mod initializer;
pub mod page_table;
pub mod serial;
pub mod transition;
pub mod uefi_helpers;
pub mod vga_debug;
pub mod virtio;
pub use common::logging::{SystemError, SystemResult};
pub use common::memory::*;
pub use common::syscall::*;
pub use common::{check_memory_initialized, set_memory_initialized};
pub use graphics::framebuffer_mapper::{CacheMode, FramebufferMapper};
pub use graphics::uefi::*;
pub use graphics::*;
pub use nitrogen::ioapic::IoApicRedirectionEntry;
pub use nitrogen::port::{MsrHelper, PortOperations, PortWriter, RegisterConfig};

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

pub use page_table::allocator::{BitmapFrameAllocator, bitmap};
#[cfg(not(feature = "std"))]
pub use page_table::heap::ALLOCATOR;
pub use page_table::heap::HeapStats;
pub use page_table::heap::allocate_heap_from_map;
pub use page_table::heap::extend_global_heap;
pub use page_table::heap::heap_stats;
pub use page_table::heap::heap_top;
pub use page_table::heap::init_global_heap;

use crate::common::EfiSystemTable;
use crate::common::uefi::FullereneFramebufferConfig;
use spin::{Mutex, Once};

pub static FULLERENE_FRAMEBUFFER_CONFIG: Once<Mutex<Option<FullereneFramebufferConfig>>> =
    Once::new();

pub const QEMU_CONFIGS: [QemuConfig; 9] = [
    QemuConfig {
        address: 0xFC000000,
        width: 1024,
        height: 768,
        bpp: 32,
    },
    QemuConfig {
        address: 0xFD000000,
        width: 1024,
        height: 768,
        bpp: 32,
    },
    QemuConfig {
        address: 0xE0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    },
    QemuConfig {
        address: 0xC0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    },
    QemuConfig {
        address: 0xF0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    },
    QemuConfig {
        address: 0xE0000000,
        width: 800,
        height: 600,
        bpp: 32,
    },
    QemuConfig {
        address: 0xF0000000,
        width: 800,
        height: 600,
        bpp: 32,
    },
    QemuConfig {
        address: 0xFD000000,
        width: 800,
        height: 600,
        bpp: 32,
    },
    QemuConfig {
        address: 0xC0000000,
        width: 800,
        height: 600,
        bpp: 32,
    },
];

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);
unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

pub fn init_uefi_system_table(system_table: *mut EfiSystemTable) {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table));
}

pub fn halt_loop() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
pub fn cpu_pause() {
    core::hint::spin_loop();
}

#[inline(always)]
pub fn cpu_halt() {
    x86_64::instructions::hlt();
}

/// Write raw bytes to a serial port. Used by early-boot and debug code.
/// This function is safe to call; the unsafe port I/O is encapsulated internally.
pub fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    #[cfg(not(feature = "std"))]
    unsafe {
        serial::write_serial_bytes(port, status_port, bytes);
    }
    #[cfg(feature = "std")]
    {
        let _ = (port, status_port, bytes);
    }
}

#[macro_export]
macro_rules! define_panic_handler {
    () => {
        #[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            use core::fmt::Write;
            $crate::serial::_print(format_args!("\n========== KERNEL PANIC ==========\n"));
            if let Some(loc) = info.location() {
                $crate::serial::_print(format_args!(
                    "  at {}:{}:{}\n",
                    loc.file(),
                    loc.line(),
                    loc.column()
                ));
            }
            $crate::serial::_print(format_args!("  {}\n", info));
            $crate::serial::_print(format_args!("==================================\n"));

            loop {
                x86_64::instructions::hlt();
            }
        }
    };
}

#[macro_export]
macro_rules! define_alloc_error_handler {
    () => {
        #[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
        #[alloc_error_handler]
        fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
            $crate::serial::_print(format_args!("ALLOC ERROR: {:?}\n", layout));
            loop {}
        }
    };
}

/// Returns (used, total, free) memory in bytes from the global page allocator.
#[macro_export]
macro_rules! get_memory_stats {
    () => {{
        let allocator = $crate::page_table::ALLOCATOR.lock();
        let used = allocator.used();
        let total = allocator.size();
        (used, total, total.saturating_sub(used))
    }};
}

#[derive(Clone, Copy)]
pub struct QemuConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}
