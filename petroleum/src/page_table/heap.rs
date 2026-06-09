//! Heap allocator initialization and management
//!
//! Provides the global heap allocator for the kernel, initialized from
//! a static buffer to avoid dependencies on UEFI memory services after
//! exit_boot_services.

use crate::page_table::memory_map::descriptor::MemoryMapDescriptor;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::PhysAddr;

/// Maximum number of memory map descriptors
pub const MAX_DESCRIPTORS: usize = 2048;

/// Buffer for memory map descriptors to avoid heap allocation during init
pub static mut MEMORY_MAP_BUFFER: [MemoryMapDescriptor; MAX_DESCRIPTORS] = [const {
    MemoryMapDescriptor {
        ptr: core::ptr::null(),
        descriptor_size: 0,
    }
}; MAX_DESCRIPTORS];

/// Flag to track heap initialization state
///
/// # Note
/// In bare-metal environments, .bss may not be zeroed by the bootloader.
/// We use a workaround by checking if HEAP_START is non-zero instead.
pub static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global heap allocator instance
#[cfg(all(not(feature = "std"), not(test)))]
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

/// Global heap allocator instance (test environment)
#[cfg(all(not(feature = "std"), test))]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

/// Check if the heap has been initialized
///
/// Uses HEAP_START value as a more reliable indicator than AtomicBool
/// in bare-metal environments where .bss may not be zeroed.
pub fn is_heap_initialized() -> bool {
    // Use HEAP_START as a more reliable indicator
    // If HEAP_START is non-zero, the heap has been initialized
    crate::common::memory::HEAP_START.load(core::sync::atomic::Ordering::SeqCst) != 0
}

/// Initializes the global heap allocator.
///
/// # Safety
///
/// The caller must ensure that the provided pointer `ptr` points to a valid
/// memory region of at least `size` bytes, and that this region is not
/// used elsewhere.
///
/// # Arguments
///
/// * `ptr` - Pointer to the start of the heap memory region
/// * `size` - Size of the heap memory region in bytes
pub unsafe fn init_global_heap(ptr: *mut u8, size: usize) { unsafe {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        // Check if already initialized by testing if allocator is empty
        // (LockedHeap::empty() creates an allocator with size 0)
        if ALLOCATOR.lock().size() > 0 {
            return;
        }

        // Debug output
        let mut buf = [0u8; 16];
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [init_global_heap] ptr: 0x");
        let len = crate::serial::format_hex_to_buffer(ptr as u64, &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b", size: 0x");
        let len = crate::serial::format_hex_to_buffer(size as u64, &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        // Initialize the allocator
        ALLOCATOR.lock().init(ptr, size);

        // NOTE: Do NOT call set_heap_range here because this is called before the world switch.
        // The heap range will be set in init_common after the world switch.

        // Mark as initialized
        HEAP_INITIALIZED.store(true, Ordering::SeqCst);
    }
}}

/// Allocate heap memory from EFI memory map
///
/// # Arguments
///
/// * `start_addr` - Physical address of the start of the memory region
/// * `heap_size` - Size of the heap in bytes
///
/// # Returns
///
/// The aligned physical address suitable for heap allocation
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> PhysAddr {
    const FRAME_SIZE: u64 = 4096;
    let _heap_frames = heap_size.div_ceil(FRAME_SIZE as usize);

    let aligned_start = if start_addr.as_u64().is_multiple_of(FRAME_SIZE) {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    aligned_start
}

/// Extend the global heap by `additional` bytes.
///
/// # Safety
///
/// The caller must ensure the memory region from `ALLOCATOR.lock().top()`
/// to `ALLOCATOR.lock().top() + additional` is a valid, free, mapped
/// memory region with `'static` lifetime.
///
/// # Panics
///
/// Panics if the heap has not been initialized.
pub unsafe fn extend_global_heap(additional: usize) { unsafe {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        let mut alloc = ALLOCATOR.lock();
        let old_top = alloc.top() as usize;
        alloc.extend(additional);
        // Update the tracked heap range so page-fault detection still works.
        let new_top = alloc.top() as usize;
        drop(alloc);
        crate::common::memory::set_heap_range(
            crate::common::memory::HEAP_START.load(core::sync::atomic::Ordering::SeqCst),
            new_top - crate::common::memory::HEAP_START.load(core::sync::atomic::Ordering::SeqCst),
        );

        // Debug output
        let mut buf = [0u8; 16];
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [extend_global_heap] old_top=0x");
        let len = crate::serial::format_hex_to_buffer(old_top as u64, &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b" new_top=0x");
        let len = crate::serial::format_hex_to_buffer(new_top as u64, &mut buf, 16);
        crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        crate::write_serial_bytes(0x3F8, 0x3FD, b"\n");
    }

    #[cfg(any(feature = "std", test))]
    {
        let _ = additional;
    }
}}

/// Return the current top address of the global heap.
///
/// Returns the pointer just past the end of the usable heap.
pub fn heap_top() -> *mut u8 {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        ALLOCATOR.lock().top()
    }
    #[cfg(any(feature = "std", test))]
    {
        core::ptr::null_mut()
    }
}

/// Heap usage statistics.
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    /// Total usable size of the heap in bytes.
    pub total: usize,
    /// Currently allocated (used) bytes.
    pub used: usize,
    /// Currently free bytes.
    pub free: usize,
}

/// Query the current heap usage.
pub fn heap_stats() -> HeapStats {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        let alloc = ALLOCATOR.lock();
        HeapStats {
            total: alloc.size(),
            used: alloc.used(),
            free: alloc.free(),
        }
    }
    #[cfg(any(feature = "std", test))]
    {
        HeapStats {
            total: 0,
            used: 0,
            free: 0,
        }
    }
}
