#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
extern crate alloc;

// ---- Custom panic handler (replaces petroleum::define_panic_handler!) ----
// We define our own so we can call klog::flush_to_vfs_safe() directly,
// without going through the extern "Rust" indirection that would conflict
// with petroleum's empty stub.
#[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
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
/// Physical address of the VGA text-mode buffer.
/// Currently unused but kept as a reference for potential fallback debugging.
#[allow(unused)]
pub const VGA_BUFFER_ADDRESS: usize = 0xb8000;

// Exported globals
pub use heap::MEMORY_MAP;

// Module declarations
// pub mod ahci;  // disabled: unused on current hardware targets
pub mod app_runner;
pub mod badapple;
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
pub mod nvme;
pub mod process;
pub mod scheduler;
pub mod shell;
pub mod slab;
pub mod syscall;
pub mod task;
pub mod tracing;
pub mod vfs;
pub mod virtio_gpu;
