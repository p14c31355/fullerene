//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

use petroleum::page_table::BootInfoFrameAllocator;

pub const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4MB heap
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB

use petroleum::initializer::FrameAllocator;
use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::PageTableHelper;
use petroleum::page_table::memory_map::MemoryMapDescriptor;
use spin::Mutex;

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Global memory map storage
pub static MEMORY_MAP: Mutex<Option<&'static [MemoryMapDescriptor]>> = Mutex::new(None);

/// Buffer for memory map descriptors to avoid heap allocation during init
pub const MAX_DESCRIPTORS: usize = 2048;

/// Static buffer for the global allocator to avoid early boot page faults
#[repr(align(4096))]
pub struct HeapBuffer(pub(crate) [u8; HEAP_SIZE]);

#[unsafe(link_section = ".data")]
pub static mut BOOT_HEAP_BUFFER: HeapBuffer = HeapBuffer([0; HEAP_SIZE]);

/// Maximum additional heap that can be requested via `extend_kernel_heap`.
/// This is a statically-reserved virtual address region contiguous with
/// `BOOT_HEAP_BUFFER`, so that `linked_list_allocator::Heap::extend()` can
/// operate on it without holes.
const HEAP_EXTEND_MAX: usize = 32 * 1024 * 1024; // 32 MiB

/// Pre‑reserved virtual address region for dynamic heap expansion.
/// Placed in `.data` (not `.bss`) to ensure it is page‑mapped at boot.
#[repr(align(4096))]
pub struct HeapExtendBuffer(pub [u8; HEAP_EXTEND_MAX]);

#[unsafe(link_section = ".data")]
pub static mut HEAP_EXTEND_BUFFER: HeapExtendBuffer = HeapExtendBuffer([0; HEAP_EXTEND_MAX]);

/// Track how many bytes of `HEAP_EXTEND_BUFFER` have already been
/// passed to `extend_global_heap`.
static HEAP_EXTEND_USED: Mutex<usize> = Mutex::new(0);

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
/// Uses the static `HEAP_EXTEND_BUFFER` region; maps physical frames into
/// that region before calling `extend_global_heap`.
///
/// Returns `Ok(())` if the extension succeeded, or `Err(())` if the
/// extension would exceed `HEAP_EXTEND_MAX` or frame allocation failed.
///
/// # Safety
///
/// Must only be called after the memory manager is initialized and while
/// the `HEAP_EXTEND_BUFFER` region is identity‑mapped.
pub unsafe fn extend_kernel_heap(additional: usize) -> Result<(), ()> {
    // Round up to page size (4 KiB) — `linked_list_allocator::extend` can
    // handle arbitrary sizes but we map in page granularity.
    let pages = (additional + 4095) / 4096;
    let bytes = pages * 4096;

    // Check we haven't exceeded the static buffer.
    let mut used = HEAP_EXTEND_USED.lock();
    if *used + bytes > HEAP_EXTEND_MAX {
        petroleum::serial::serial_log(format_args!(
            "extend_kernel_heap: would exceed HEAP_EXTEND_MAX (used={}, need={}, max={})\n",
            *used, bytes, HEAP_EXTEND_MAX,
        ));
        return Err(());
    }

    // Get memory manager to map physical frames.
    let mut mgr_guard = crate::memory_management::get_memory_manager().lock();
    let mgr = mgr_guard.as_mut().ok_or(())?;

    // Map each page.
    for i in 0..pages {
        let phys = match mgr.allocate_frame() {
            Ok(p) => p,
            Err(_) => {
                // Best-effort rollback: deallocate any frames already mapped.
                rollback_frames(mgr, *used, i);
                return Err(());
            }
        };
        let virt = unsafe {
            (core::ptr::addr_of!(HEAP_EXTEND_BUFFER.0) as *const u8).add(*used + i * 4096)
        } as usize;

        if mgr
            .safe_map_page(
                virt,
                phys,
                x86_64::structures::paging::PageTableFlags::PRESENT
                    | x86_64::structures::paging::PageTableFlags::WRITABLE,
            )
            .is_err()
        {
            // Rollback on mapping failure.
            let _ = mgr.free_frame(phys);
            rollback_frames(mgr, *used, i);
            return Err(());
        }
    }

    // Extend the global heap.
    let extend_ptr = unsafe {
        (core::ptr::addr_of_mut!(HEAP_EXTEND_BUFFER.0) as *mut u8).add(*used)
    };
    // The region from `BOOT_HEAP_BUFFER` end to `extend_ptr` should be
    // contiguous. `linked_list_allocator::extend` extends the heap at
    // `top()`, so if this is the first call, `top()` points to the end
    // of the initial 4 MiB, which must equal `extend_ptr`.
    unsafe {
        petroleum::extend_global_heap(bytes);
    }

    *used += bytes;
    drop(mgr_guard);

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

// ── internal helpers ──────────────────────────────────────────

use crate::memory_management::manager::UnifiedMemoryManager;

/// Rollback: unmap + free previously mapped extend frames.
fn rollback_frames(mgr: &mut UnifiedMemoryManager, used: usize, count: usize) {
    for j in 0..count {
        let virt = unsafe {
            (core::ptr::addr_of!(HEAP_EXTEND_BUFFER.0) as *const u8).add(used + j * 4096) as usize
        };
        if let Ok(phys) = mgr.translate_address(virt) {
            let _ = mgr.safe_unmap_page(virt);
            let _ = mgr.free_frame(phys);
        }
    }
}
