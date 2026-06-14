#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
extern crate alloc;

// Define panic and alloc error handlers using petroleum's macros
petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

/// Called by the panic handler to attempt persisting the kernel log to VFS.
///
/// This is a best-effort operation — if the VFS lock is poisoned or I/O
/// fails, the error is silently swallowed.
#[unsafe(no_mangle)]
pub extern "Rust" fn _fullerene_panic_flush() {
    crate::boot_stage::set_boot_stage(crate::boot_stage::BootStage::Panic);
    crate::klog::flush_to_vfs_safe();
}

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
