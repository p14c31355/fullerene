#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

// Define panic and alloc error handlers using petroleum's macros
petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

// Constants
/// Physical address of the VGA text-mode buffer.
/// Currently unused but kept as a reference for potential fallback debugging.
#[allow(unused)]
pub const VGA_BUFFER_ADDRESS: usize = 0xb8000;

// Exported globals
pub use heap::MEMORY_MAP;

// Module declarations
pub mod ahci;
pub mod app_runner;
pub mod badapple;
pub mod boot;
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
pub mod keyboard;
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
