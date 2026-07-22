//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

use petroleum::page_table::BootInfoFrameAllocator;

pub const HEAP_SIZE: usize = 12 * 1024 * 1024; // 12MB heap (allows ~4MB back buffer + overhead)
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB

/// Maximum additional heap that can be requested via `extend_kernel_heap`.
/// Increased to 80 MiB to accommodate large image decode buffers (e.g.
/// 1920x1080x4 = ~8 MiB) plus decoder working memory and terminal/editor surfaces.
const HEAP_EXTEND_MAX: usize = 80 * 1024 * 1024; // 80 MiB

/// Total heap size: initial 12 MiB + extendable 80 MiB.
pub const HEAP_TOTAL: usize = HEAP_SIZE + HEAP_EXTEND_MAX; // 92 MiB

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
/// The first [`HEAP_SIZE`] bytes serve as the initial heap (replaces the old
/// `BOOT_HEAP_BUFFER`).  The remaining [`HEAP_EXTEND_MAX`] bytes are used
/// for dynamic heap expansion (replaces the old `HEAP_EXTEND_BUFFER`).
///
/// Placed in `.data` to ensure it is page‑mapped at boot by OVMF.
/// 36 MiB is within OVMF's safe handling limits.
#[repr(align(4096))]
pub struct TotalHeapBuffer(#[allow(dead_code)] pub(crate) [u8; HEAP_TOTAL]);

/// # Safety
/// The heap buffer is written once (zeroed at compile time, mapped by UEFI),
/// and then used by the kernel allocator which serialises access via spinlock.
/// Only accessed after single‑core boot init is complete.
#[unsafe(link_section = ".data")]
pub static mut TOTAL_HEAP_BUFFER: TotalHeapBuffer = TotalHeapBuffer([0; HEAP_TOTAL]);

/// Track how many bytes of the extend region (offset `HEAP_SIZE` inside
/// `TOTAL_HEAP_BUFFER`) have already been passed to `extend_global_heap`.
static HEAP_EXTEND_USED: Mutex<usize> = Mutex::new(0);

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

/// Extend the kernel heap by `additional` bytes.
///
/// The entire [`TOTAL_HEAP_BUFFER`] (including the extend region starting
/// at offset [`HEAP_SIZE`]) is placed in `.data` and already mapped by
/// the UEFI PE loader with zeroed physical pages.  Therefore we only need
/// to call `petroleum::extend_global_heap` — no additional frame
/// allocation or page-table manipulation is required.
///
/// Returns `Ok(())` if the extension succeeded, or `Err(())` if the
/// extension would exceed `HEAP_EXTEND_MAX`.
///
/// # Safety
///
/// Must only be called after the allocator is initialized and the
/// `TOTAL_HEAP_BUFFER` region is mapped.
pub unsafe fn extend_kernel_heap(additional: usize) -> Result<(), ()> {
    // Round up to page size (4 KiB).
    let pages = (additional + 4095) / 4096;
    let bytes = pages * 4096;

    // Check we haven't exceeded the extend region.
    let mut used = HEAP_EXTEND_USED.lock();
    if *used + bytes > HEAP_EXTEND_MAX {
        petroleum::serial::serial_log(format_args!(
            "extend_kernel_heap: would exceed HEAP_EXTEND_MAX (used={}, need={}, max={})\n",
            *used, bytes, HEAP_EXTEND_MAX,
        ));
        return Err(());
    }

    // The extend region is already mapped (it's part of .data), so just
    // tell the allocator to make it available.
    unsafe {
        petroleum::extend_global_heap(bytes);
    }

    *used += bytes;

    petroleum::serial::serial_log(format_args!(
        "extend_kernel_heap: extended by {} bytes (total extend used={})\n",
        bytes, *used,
    ));
    Ok(())
}

/// Return the number of bytes currently available in the global heap
/// (free space).
pub fn heap_free() -> usize {
    petroleum::heap_stats().free
}
