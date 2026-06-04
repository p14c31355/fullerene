#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

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
            $crate::serial::_print(format_args!("====================================\n"));

            // Stack backtrace via BacktraceCollector (reuses debug.rs)
            $crate::serial::_print(format_args!("\nStack backtrace:\n"));
            {
                let mut collector = $crate::debug::BacktraceCollector::new();
                collector.capture();
                for (i, entry) in collector.entries().iter().enumerate() {
                    $crate::serial::_print(format_args!(
                        "  #{:<2} rbp=0x{:016x} rip=0x{:016x}\n",
                        i, entry.sp, entry.ip
                    ));
                }
            }

            #[cfg(feature = "vga_panic")]
            {
                let vga = 0xb8000 as *mut u16;
                for i in 0..(80 * 25) {
                    unsafe { vga.add(i).write_volatile(0x4F20u16) };
                }
                let header = b"*** KERNEL PANIC ***";
                for (i, &b) in header.iter().enumerate() {
                    unsafe { vga.add(i).write_volatile(b as u16 | 0x4F00u16) };
                }
                if let Some(loc) = info.location() {
                    use core::fmt::Write as _;
                    let mut line_buf = heapless::String::<128>::new();
                    let _ = write!(line_buf, "{}:{}", loc.file(), loc.line());
                    for (i, b) in line_buf.bytes().enumerate() {
                        let off = 80 * 2 + i;
                        if off < 80 * 25 {
                            unsafe { vga.add(off).write_volatile(b as u16 | 0x4F00u16) };
                        }
                    }
                }
                use core::fmt::Write as _;
                let mut msg_buf = heapless::String::<128>::new();
                let _ = write!(msg_buf, "{}", info);
                for (i, b) in msg_buf.bytes().enumerate() {
                    let off = 80 * 3 + i;
                    if off < 80 * 25 {
                        let ch = if b < 0x20 || b > 0x7e { b' ' } else { b };
                        unsafe { vga.add(off).write_volatile(ch as u16 | 0x4F00u16) };
                    }
                }
            }

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

pub const FALLBACK_HEAP_START_ADDR: u64 = 0x100000;

pub mod apic;
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
pub use apic::{configure_io_apic_for_legacy_irqs, init_io_apic};
pub use common::logging::{SystemError, SystemResult};
pub use common::memory::*;
pub use common::syscall::*;
pub use common::{check_memory_initialized, set_memory_initialized};
pub use graphics::framebuffer_mapper::{CacheMode, FramebufferMapper};
pub use graphics::uefi::*;
pub use graphics::*;
pub use nitrogen::ioapic::{IoApic, IoApicRedirectionEntry};
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
pub use page_table::heap::allocate_heap_from_map;
pub use page_table::heap::init_global_heap;
pub use uefi_helpers::{initialize_graphics_with_config, kernel_fallback_framebuffer_detection};

// ── Backward-compat deprecated macro wrappers ──────────────────

#[macro_export]
macro_rules! map_identity_range_checked {
    ($($arg:tt)*) => { $crate::page_table::map_identity_range($($arg)*) };
}
#[macro_export]
macro_rules! map_range_with_log_macro {
    ($($arg:tt)*) => { $crate::page_table::map_range_with_log_macro($($arg)*) };
}
#[macro_export]
macro_rules! map_to_higher_half_with_log_macro {
    ($($arg:tt)*) => { $crate::page_table::map_to_higher_half_with_log_macro($($arg)*) };
}
#[macro_export]
macro_rules! map_page_range {
    ($($arg:tt)*) => { $crate::page_table::map_range_4kiB($($arg)*) };
}
#[macro_export]
macro_rules! unmap_page_range {
    ($($arg:tt)*) => { $crate::page_table::unmap_page_range($($arg)*) };
}
#[macro_export]
macro_rules! get_memory_stats {
    () => {
        (0usize, 0usize, 0usize)
    };
}

use crate::common::EfiSystemTable;
use crate::common::uefi::FullereneFramebufferConfig;
use spin::{Mutex, Once};

#[derive(Clone, Copy)]
pub struct LocalApicAddress(pub *mut u32);
unsafe impl Send for LocalApicAddress {}
unsafe impl Sync for LocalApicAddress {}

pub static LOCAL_APIC_ADDRESS: Mutex<LocalApicAddress> =
    Mutex::new(LocalApicAddress(core::ptr::null_mut()));
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
        cpu_pause();
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

pub unsafe fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    #[cfg(not(feature = "std"))]
    unsafe {
        serial::write_serial_bytes(port, status_port, bytes);
    }
    #[cfg(feature = "std")]
    {}
}

#[macro_export]
macro_rules! write_serial_bytes {
    ($port:expr, $status:expr, $bytes:expr) => {
        unsafe {
            $crate::write_serial_bytes($port, $status, $bytes);
        }
    };
}

#[derive(Clone, Copy)]
pub struct QemuConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}
