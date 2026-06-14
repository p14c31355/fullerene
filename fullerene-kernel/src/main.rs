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

    // Best-effort VFS flush
    crate::boot_stage::set_boot_stage(crate::boot_stage::BootStage::Panic);
    crate::klog::flush_to_vfs_safe();

    loop {
        x86_64::instructions::hlt();
    }
}

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

/// Weak symbol called by `petroleum::serial::_print` to forward output
/// to the xHCI Debug Capability (USB debug).
///
/// If DbC is not initialized, this is a no-op.
#[unsafe(no_mangle)]
pub extern "Rust" fn _dbc_try_write_str(ptr: *const u8, len: usize) {
    if nitrogen::xhci_dbc::is_ready() {
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        nitrogen::xhci_dbc::dbc_write_bytes(slice);
    }
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