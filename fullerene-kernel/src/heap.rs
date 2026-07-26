//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

use petroleum::page_table::BootInfoFrameAllocator;

/// Initial contiguous heap available before automatic extension.
pub const HEAP_SIZE: usize = 12 * 1024 * 1024;
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB

/// Maximum additional heap exposed automatically when an allocation fails.
pub const HEAP_EXTEND_MAX: usize = 128 * 1024 * 1024;

/// Total static heap buffer: initial 12 MiB + extendable 128 MiB.
pub const HEAP_TOTAL: usize = HEAP_SIZE + HEAP_EXTEND_MAX;

use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::memory_map::MemoryMapDescriptor;
use spin::Mutex;

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Global memory map storage
pub static MEMORY_MAP: Mutex<Option<&'static [MemoryMapDescriptor]>> = Mutex::new(None);

/// Buffer for memory map descriptors to avoid heap allocation during init
pub const MAX_DESCRIPTORS: usize = 2048;

/// Single contiguous static buffer for the global allocator.
///
/// The first [`HEAP_SIZE`] bytes serve as the initial heap. The remaining
/// [`HEAP_EXTEND_MAX`] bytes are exposed lazily by the global allocator.
///
/// This is deliberately not forced into `.data`: because it is zero
/// initialized, the PE/UEFI image can represent it as a loader-zero-filled
/// region (BSS).  The pages are still mapped and zeroed by the UEFI image
/// loader at runtime, but the zero bytes do not occupy space in the ISO.
#[repr(align(4096))]
pub struct TotalHeapBuffer(#[allow(dead_code)] pub(crate) [u8; HEAP_TOTAL]);

/// # Safety
/// The heap buffer is written once (zeroed at compile time, mapped by UEFI),
/// and then used by the kernel allocator which serialises access via spinlock.
/// Only accessed after single‑core boot init is complete.
pub static mut TOTAL_HEAP_BUFFER: TotalHeapBuffer = TotalHeapBuffer([0; HEAP_TOTAL]);

/// # Safety
/// Written once during boot by `MemoryDescriptorValidator`, then read-only.
/// Single-core assumption. Only used in `cfg(target_os = "uefi")` boot path.
#[cfg(target_os = "uefi")]
#[unsafe(link_section = ".data")]
pub(crate) static mut MEMORY_MAP_BUFFER: [MemoryMapDescriptor; MAX_DESCRIPTORS] = [const {
    MemoryMapDescriptor {
        ptr: core::ptr::null(),
        descriptor_size: 0,
    }
};
    MAX_DESCRIPTORS];

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &[impl MemoryDescriptorValidator]) {
    // SAFETY: We are converting a slice of trait objects to a concrete slice of MemoryMapDescriptor.
    // The memory_map is guaranteed to contain valid MemoryMapDescriptor instances, so this is safe.
    let concrete_map = unsafe {
        core::slice::from_raw_parts(
            memory_map.as_ptr() as *const petroleum::page_table::memory_map::MemoryMapDescriptor,
            memory_map.len(),
        )
    };

    let allocator = petroleum::page_table::BitmapFrameAllocator::init_with_memory_map(concrete_map);
    *FRAME_ALLOCATOR.lock() = Some(allocator);
}

/// Configure automatic heap extension after the initial allocator is ready.
pub fn configure_heap_extension() {
    petroleum::configure_heap_extension(HEAP_EXTEND_MAX);
}

/// Extend the kernel heap by `additional` bytes.
///
/// The entire [`TOTAL_HEAP_BUFFER`] (including the extend region starting
/// at offset [`HEAP_SIZE`]) is placed in `.data` and already mapped by
/// the UEFI PE loader with zeroed physical pages.  Therefore we only need
/// to call `petroleum::extend_global_heap` — no additional frame
/// allocation or page-table manipulation is required.
///
/// Returns `Ok(())` if the extension succeeded, or `Err(())` if the
/// configured extension region is exhausted.
///
/// # Safety
///
/// Must only be called after the allocator is initialized and the
/// `TOTAL_HEAP_BUFFER` region is mapped.
pub unsafe fn extend_kernel_heap(additional: usize) -> Result<(), ()> {
    // Round up to page size (4 KiB).
    let pages = (additional + 4095) / 4096;
    let bytes = pages * 4096;

    // The extension region is already mapped (it is part of the static heap);
    // the allocator serializes the extension and tracks its limit.
    unsafe { petroleum::try_extend_global_heap(bytes) }
}

/// Return the number of bytes currently available in the global heap
/// (free space).
pub fn heap_free() -> usize {
    petroleum::heap_stats().free
}
