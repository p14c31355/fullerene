#![no_std]
#![feature(never_type)]
#![feature(alloc_error_handler)]

extern crate alloc;

/// Macro to define panic handler using petroleum's serial output.
/// Use this in binary crates (kernel, bootloader).
/// Prints panic location (file, line, column), a stack backtrace (RBP chain),
/// and (with `vga_panic` feature) a red panic screen via VGA text-mode buffer.
#[macro_export]
macro_rules! define_panic_handler {
    () => {
        #[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            use core::fmt::Write;

            // ── 1. Serial output (always) ───────────────────────
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

            // ── 2. Stack backtrace (RBP chain) ────────────────
            $crate::serial::_print(format_args!("\nStack backtrace:\n"));
            unsafe {
                let mut rbp: usize;
                core::arch::asm!("mov {}, rbp", out(reg) rbp);
                for frame_idx in 0..32 {
                    if rbp == 0 || rbp < 0x1000 {
                        break;
                    }
                    let rip = *(rbp as *const usize).add(1);
                    if rip == 0 || rip < 0x1000 {
                        break;
                    }
                    $crate::serial::_print(format_args!(
                        "  #{:<2} rbp=0x{:016x} rip=0x{:016x}\n",
                        frame_idx, rbp, rip
                    ));
                    rbp = *(rbp as *const usize);
                }
            }

            // ── 3. VGA panic screen (optional feature) ──────────
            #[cfg(feature = "vga_panic")]
            {
                let vga = 0xb8000 as *mut u16;
                // Clear screen: 80×25 red background with white text
                for i in 0..(80 * 25) {
                    unsafe { vga.add(i).write_volatile(0x4F20u16) }; // red bg, space
                }
                // Print panic header
                let header = b"*** KERNEL PANIC ***";
                for (i, &b) in header.iter().enumerate() {
                    unsafe { vga.add(i).write_volatile(b as u16 | 0x4F00u16) };
                }
                // Print file:line
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
                // Print message (truncated, stack-allocated to avoid OOM re-panic)
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

            // ── 4. Halt forever ─────────────────────────────────
            loop {
                unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
            }
        }
    };
}

/// Macro to define alloc error handler.
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

// Fallback heap start address constant for when no suitable memory is found
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
pub mod vga_debug;
pub mod transition;
pub mod uefi_helpers;
pub mod virtio;
pub use apic::{configure_io_apic_for_legacy_irqs, init_io_apic};
pub use nitrogen::ioapic::{IoApic, IoApicRedirectionEntry};
// Macros with #[macro_export] are automatically available at root, no need to re-export
pub use common::logging::{SystemError, SystemResult};
pub use common::memory::*;
pub use common::syscall::*;
pub use common::{check_memory_initialized, set_memory_initialized};
// Re-export UEFI graphics protocol detection functions from graphics::uefi module.
// These are the canonical implementations — do NOT re-define them here.
pub use graphics::uefi::*;
pub use graphics::*;
pub use nitrogen::port::{MsrHelper, PortOperations, PortWriter, RegisterConfig};

// pub use serial::SERIAL_PORT_WRITER as SERIAL1; // Refactored
// pub use serial::{COM1_DATA_PORT, COM1_STATUS_PORT}; // Refactored
// pub use serial::{Com1Ports, SERIAL_PORT_WRITER, SerialPort, SerialPortOps}; // Refactored

// Buffer operation wrappers (used by petroleum internally)
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

// Heap allocation exports
pub use page_table::allocator::{BitmapFrameAllocator, bitmap};
#[cfg(not(feature = "std"))]
pub use page_table::heap::ALLOCATOR;
pub use page_table::heap::allocate_heap_from_map;
pub use page_table::heap::init_global_heap;
// Removed reinit_page_table export - implemented in higher-level crates
// UEFI helper exports
pub use uefi_helpers::{initialize_graphics_with_config, kernel_fallback_framebuffer_detection};

// ── Backward-compat macro wrappers ───────────────────────────────────

/// Deprecated: Use `page_table::map_identity_range` instead.
#[macro_export]
macro_rules! map_identity_range_checked {
    ($($arg:tt)*) => { $crate::page_table::map_identity_range($($arg)*) };
}

/// Deprecated: Use `page_table::map_range_with_log_macro` instead.
#[macro_export]
macro_rules! map_range_with_log_macro {
    ($($arg:tt)*) => { $crate::page_table::map_range_with_log_macro($($arg)*) };
}

/// Deprecated: Use `page_table::map_to_higher_half_with_log_macro` instead.
#[macro_export]
macro_rules! map_to_higher_half_with_log_macro {
    ($($arg:tt)*) => { $crate::page_table::map_to_higher_half_with_log_macro($($arg)*) };
}

/// Deprecated: Use `page_table::map_range_4kiB` instead.
#[macro_export]
macro_rules! map_page_range {
    ($($arg:tt)*) => { $crate::page_table::map_range_4kiB($($arg)*) };
}

/// Deprecated: Use `page_table::unmap_page_range` instead.
#[macro_export]
macro_rules! unmap_page_range {
    ($($arg:tt)*) => { $crate::page_table::unmap_page_range($($arg)*) };
}

/// Deprecated: Returns (0, 0, 0).
#[macro_export]
macro_rules! get_memory_stats {
    () => {
        (0usize, 0usize, 0usize)
    };
}

use spin::{Mutex, Once};

use crate::common::EfiSystemTable;
use crate::common::uefi::FullereneFramebufferConfig;

// ── RUNTIME GLOBAL STATE ──────────────────────────────────────────────
// These statics are safe for both early boot and runtime kernel use.
// They are either:
//   - Set once during kernel init and never change (PHYSICAL_MEMORY_OFFSET)
//   - Protected by Mutex/Once and lazily initialised

/// Wrapper for Local APIC address pointer to make it Send/Sync
#[derive(Clone, Copy)]
pub struct LocalApicAddress(pub *mut u32);

unsafe impl Send for LocalApicAddress {}
unsafe impl Sync for LocalApicAddress {}

/// Global storage for Local APIC address
pub static LOCAL_APIC_ADDRESS: Mutex<LocalApicAddress> =
    Mutex::new(LocalApicAddress(core::ptr::null_mut()));

/// Global framebuffer config storage for kernel use after exit_boot_services
pub static FULLERENE_FRAMEBUFFER_CONFIG: Once<Mutex<Option<FullereneFramebufferConfig>>> =
    Once::new();

pub const QEMU_CONFIGS: [QemuConfig; 9] = [
    // Standard QEMU std-vga framebuffer (Bochs VBE) - common in QEMU q35
    QemuConfig {
        address: 0xFC000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // QEMU std-vga at high memory
    QemuConfig {
        address: 0xFD000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Alternative high memory framebuffer
    QemuConfig {
        address: 0xE0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Common QEMU std-vga mode
    QemuConfig {
        address: 0xC0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // i440fx vga
    QemuConfig {
        address: 0xF0000000,
        width: 1024,
        height: 768,
        bpp: 32,
    }, // Alternative QEMU framebuffer
    QemuConfig {
        address: 0xE0000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // 800x600 mode
    QemuConfig {
        address: 0xF0000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // Alternative 800x600
    QemuConfig {
        address: 0xFD000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // High memory 800x600
    QemuConfig {
        address: 0xC0000000,
        width: 800,
        height: 600,
        bpp: 32,
    }, // i440fx vga 800x600
];

#[derive(Clone, Copy)]
pub struct UefiSystemTablePtr(pub *mut EfiSystemTable);

unsafe impl Send for UefiSystemTablePtr {}
unsafe impl Sync for UefiSystemTablePtr {}

// ── EARLY-ONLY GLOBAL STATE ──────────────────────────────────────────
// These statics are only valid DURING the UEFI boot phase (before
// ExitBootServices is called). After that, the pointers they hold
// become invalid. The runtime kernel MUST NOT read them.

pub static UEFI_SYSTEM_TABLE: Mutex<Option<UefiSystemTablePtr>> = Mutex::new(None);

/// Helper to initialize UEFI system table
///
/// # EARLY ONLY
/// This MUST only be called during UEFI boot phase, before ExitBootServices.
pub fn init_uefi_system_table(system_table: *mut EfiSystemTable) {
    let _ = UEFI_SYSTEM_TABLE
        .lock()
        .insert(UefiSystemTablePtr(system_table));
}

pub fn halt_loop() -> ! {
    loop {
        // Use pause instruction which is more QEMU-friendly than hlt
        cpu_pause();
    }
}

/// Helper function to pause CPU for brief moment (used for busy waits and yielding)
#[inline(always)]
pub fn cpu_pause() {
    unsafe {
        core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

/// Helper function to halt CPU
#[inline(always)]
pub fn cpu_halt() {
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

/// Helper to initialize serial for bootloader
pub unsafe fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    #[cfg(not(feature = "std"))]
    unsafe {
        serial::write_serial_bytes(port, status_port, bytes);
    }
    #[cfg(feature = "std")]
    {
        // In std environment, we avoid direct port I/O to prevent SIGSEGV
        // and optionally print to stdout for debugging.
        // println!("Serial write: {:?}", core::str::from_utf8(bytes).unwrap_or("invalid utf8"));
    }
}

/// macro for bootloader serial logging
#[macro_export]
macro_rules! write_serial_bytes {
    ($port:expr, $status:expr, $bytes:expr) => {
        unsafe {
            $crate::write_serial_bytes($port, $status, $bytes);
        }
    };
}

/// Shared struct for QEMU configuration testing
#[derive(Clone, Copy)]
pub struct QemuConfig {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}
