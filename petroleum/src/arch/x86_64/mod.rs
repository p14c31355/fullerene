//! x86_64 (PC/UEFI) platform layer: the full driver and support set.
//!
//! Only compiled on x86 targets. lib.rs globs this tree into the crate root,
//! so historic paths (`petroleum::serial`, `crate::early`, ...) are all
//! preserved without any cfg in the shared code.

pub mod assembly;
pub mod bare_metal_pci;
pub mod debug;
pub mod early;
pub mod filesystem;
pub mod initializer;
pub mod io;
pub mod serial;
pub mod transition;
pub mod uefi_helpers;
pub mod vdso;
pub mod vga_debug;

pub use crate::initializer::InitSequence;
pub use crate::page_table::allocator::{BitmapFrameAllocator, bitmap};
#[cfg(not(feature = "std"))]
pub use crate::page_table::heap::ALLOCATOR;
pub use crate::page_table::heap::{
    HeapStats, allocate_heap_from_map, configure_heap_extension, extend_global_heap, heap_stats,
    heap_top, init_global_heap, set_heap_extension_guard, try_extend_global_heap,
};
pub use sealant;
pub use vdso::*;

pub fn halt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
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

/// Write raw bytes to a serial port. Safe wrapper; port I/O stays internal.
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

/// Legacy hardware cursor position hook used by the shared text console.
pub fn set_hardware_cursor(pos: usize) {
    crate::update_vga_cursor!(pos);
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
            // GlobalAlloc has already performed its bounded extension
            // transaction.  Re-entering allocation here would hide the real
            // OOM behind an infinite retry.  Keep this kernel-fatal path
            // allocation-free and stop the CPU until the fault supervisor can
            // report/restart the owning domain.
            $crate::serial::_print(format_args!("ALLOC ERROR (no retry): {:?}\n", layout));
            loop {
                x86_64::instructions::interrupts::disable();
                x86_64::instructions::hlt();
            }
        }
    };
}

/// Returns (used, total, free) memory in bytes from the global page allocator.
#[macro_export]
macro_rules! get_memory_stats {
    () => {{
        let stats = $crate::page_table::heap_stats();
        (stats.used, stats.total, stats.free)
    }};
}
