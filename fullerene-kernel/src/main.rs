#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
extern crate alloc;

// ---- Custom panic handler (replaces petroleum::define_panic_handler!) ----
#[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    crate::boot_stage::set_boot_stage(crate::boot_stage::BootStage::Panic);
    petroleum::serial::_print(format_args!("\n========== KERNEL PANIC ==========\n"));
    if let Some(loc) = info.location() {
        petroleum::serial::_print(format_args!(
            "  at {}:{}:{}\n",
            loc.file(),
            loc.line(),
            loc.column()
        ));
    }
    petroleum::serial::_print(format_args!("  {}\n", info));
    petroleum::serial::_print(format_args!("==================================\n"));

    loop {
        x86_64::instructions::hlt();
    }
}

petroleum::define_alloc_error_handler!();

// Constants
pub const VGA_BUFFER_ADDRESS: usize = 0xb8000;

// Exported globals
pub use heap::MEMORY_MAP;

// Module declarations
// ── Applications (user-visible programs, demos) ────────────────────
pub mod apps;

// ── Drivers (storage, GPU, network) ───────────────────────────────
pub mod drivers;

// ── DriverContext bridge (kernel → nitrogen) ──────────────────────
pub mod driver_context_impl;

// ── Kernel core ────────────────────────────────────────────────────
pub mod boot;
pub mod boot_stage;
pub mod context_switch;
pub mod contexts;
pub mod fs;
pub mod gdt;
pub mod graphics;
pub mod gui;
pub mod hardware;
pub mod heap;
pub mod init;
pub mod interrupts;
pub mod klog;
pub mod loader;
pub mod memory_management;
pub mod process;
pub mod scheduler;
pub mod shell;
pub mod slab;
pub mod syscall;
pub mod task;
// tracing.rs is now a thin re-export of resonance::tracing
pub mod tracing;
pub mod vfs;