#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::c_void;
use core::{ptr, slice};
use linked_list_allocator::LockedHeap;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Alloc error handler required when using `alloc` in no_std.
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

/// Panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

// Minimal ELF header for kernel loading (kept for future use)
#[repr(C)]
struct ElfHeader {
    magic: [u8; 4],
    _rest: [u8; 12],
    entry: u64,
}

// Load kernel ELF and return an entry point function pointer (kept for future use)
fn load_kernel(kernel: &[u8]) -> Option<extern "C" fn() -> !> {
    if kernel.len() < 24 || &kernel[0..4] != b"\x7fELF" {
        return None;
    }
    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    Some(unsafe { core::mem::transmute(header.entry) })
}

/// Entry point for the bare-metal bootloader.
/// This function is called by the bootloader (e.g., GRUB, or a custom bootloader).
/// It should not return.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Initialize the heap
    // For a real bootloader, you'd get memory map information here.
    // For now, we'll just initialize a dummy heap.
    unsafe {
        ALLOCATOR.lock().init(0x500_000 as *mut u8, HEAP_SIZE); // Dummy address for now
    }

    // TODO: Implement actual kernel loading and jumping here.
    // For now, just loop indefinitely to prevent returning.
    loop {}
}
