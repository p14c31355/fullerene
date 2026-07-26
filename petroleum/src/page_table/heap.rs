//! Heap allocator initialization and management
//!
//! Provides the global heap allocator for the kernel, initialized from
//! a static buffer to avoid dependencies on UEFI memory services after
//! exit_boot_services.

use crate::page_table::memory_map::descriptor::MemoryMapDescriptor;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::AtomicBool;
use spin::Mutex;
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

/// State for the statically mapped heap extension region.
struct ExtensionState {
    limit: usize,
    used: usize,
}

/// A linked-list heap which grows into a pre-mapped contiguous extension when
/// an allocation cannot be satisfied by the currently exposed free holes.
///
/// The first allocation attempt releases the heap lock before taking the
/// extension lock.  This is important: extending from inside a
/// `GlobalAlloc::alloc` call must never recursively acquire the allocator
/// lock held by the failed attempt.
pub struct GrowingHeap {
    heap: Mutex<linked_list_allocator::Heap>,
    extension: Mutex<ExtensionState>,
}

impl GrowingHeap {
    pub const fn empty() -> Self {
        Self {
            heap: Mutex::new(linked_list_allocator::Heap::empty()),
            extension: Mutex::new(ExtensionState { limit: 0, used: 0 }),
        }
    }

    pub unsafe fn init(&self, ptr: *mut u8, size: usize) {
        let mut heap = self.heap.lock();
        if !heap.bottom().is_null() {
            return;
        }
        unsafe { heap.init(ptr, size) };
    }

    pub fn configure_extension(&self, limit: usize) {
        let mut extension = self.extension.lock();
        extension.limit = limit;
        extension.used = 0;
    }

    fn try_alloc(&self, layout: Layout) -> *mut u8 {
        self.heap
            .lock()
            .allocate_first_fit(layout)
            .ok()
            .map_or(core::ptr::null_mut(), |allocation| allocation.as_ptr())
    }

    /// Expose one additional contiguous portion of the reserved heap.
    ///
    /// This function is deliberately a single transaction.  A failed
    /// allocation may cause at most one extension attempt and one retry in
    /// `GlobalAlloc::alloc`; it must never turn into an allocator-side loop.
    fn extend_for(&self, layout: Layout) -> bool {
        const PAGE: usize = 4096;
        const MIN_GROW: usize = 64 * 1024;
        let required = layout
            .size()
            .saturating_add(layout.align())
            .saturating_add(2 * core::mem::size_of::<usize>());
        let requested = required.max(MIN_GROW).div_ceil(PAGE) * PAGE;

        let mut extension = self.extension.lock();
        let available = extension.limit.saturating_sub(extension.used);
        let grow = requested.min(available);
        if grow == 0 {
            return false;
        }

        // The extension region is immediately adjacent to the original
        // static backing. Its pages are exposed by this bounded transaction;
        // the kernel PF path may map a missing page and resume the instruction
        // that touched it.
        let (old_top, old_size) = {
            let heap = self.heap.lock();
            (heap.top() as usize, heap.size())
        };
        unsafe { self.heap.lock().extend(grow) };
        let (new_top, new_size) = {
            let heap = self.heap.lock();
            (heap.top() as usize, heap.size())
        };

        // `Heap::extend` is unsafe and has no result value.  Verify that it
        // really advanced before accounting the bytes.  This prevents a
        // broken mapping/adjacency assumption from becoming an endless
        // allocate -> extend -> retry cycle.
        if new_top <= old_top || new_size <= old_size {
            return false;
        }
        let actual_grow = new_size - old_size;
        extension.used = extension.used.saturating_add(actual_grow);
        let start = crate::common::memory::HEAP_START.load(core::sync::atomic::Ordering::SeqCst);
        if start != 0 {
            crate::common::memory::set_heap_range(start, new_top.saturating_sub(start));
        }
        true
    }

    pub fn size(&self) -> usize {
        self.heap.lock().size()
    }

    pub fn used(&self) -> usize {
        self.heap.lock().used()
    }

    pub fn free(&self) -> usize {
        self.heap.lock().free()
    }

    pub fn top(&self) -> *mut u8 {
        self.heap.lock().top()
    }

    pub unsafe fn extend(&self, additional: usize) -> Result<(), ()> {
        let mut extension = self.extension.lock();
        let available = extension.limit.saturating_sub(extension.used);
        if additional == 0 || additional > available {
            return Err(());
        }
        unsafe { self.heap.lock().extend(additional) };
        extension.used += additional;
        Ok(())
    }
}

unsafe impl GlobalAlloc for GrowingHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Keep this bounded by construction.  The CPU can resume after a
        // page-fault-based growth, but a GlobalAlloc implementation must not
        // emulate that by retrying forever inside the allocator.
        let ptr = self.try_alloc(layout);
        if !ptr.is_null() {
            return ptr;
        }
        if self.extend_for(layout) {
            self.try_alloc(layout)
        } else {
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = NonNull::new(ptr) {
            unsafe { self.heap.lock().deallocate(ptr, layout) };
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size == 0 {
            unsafe { self.dealloc(ptr, layout) };
            return core::ptr::null_mut();
        }
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return core::ptr::null_mut();
        };
        let new_ptr = unsafe { self.alloc(new_layout) };
        if new_ptr.is_null() {
            return core::ptr::null_mut();
        }
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}

/// Global heap allocator instance
#[cfg(all(not(feature = "std"), not(test)))]
#[global_allocator]
pub static ALLOCATOR: GrowingHeap = GrowingHeap::empty();

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
pub unsafe fn init_global_heap(ptr: *mut u8, size: usize) {
    #[cfg(all(not(feature = "std"), not(test)))]
    unsafe {
        // Check if already initialized by testing if allocator is empty
        // (LockedHeap::empty() creates an allocator with size 0)
        if !ALLOCATOR.heap.lock().bottom().is_null() {
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
        ALLOCATOR.init(ptr, size);

        // NOTE: Do NOT call set_heap_range here because this is called before the world switch.
        // The heap range will be set in init_common after the world switch.

        // Mark as initialized
        HEAP_INITIALIZED.store(true, core::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(any(feature = "std", test))]
    let _ = (ptr, size);
}

/// Configure the maximum size of the statically mapped heap extension.
pub fn configure_heap_extension(limit: usize) {
    #[cfg(all(not(feature = "std"), not(test)))]
    ALLOCATOR.configure_extension(limit);
    #[cfg(any(feature = "std", test))]
    let _ = limit;
}

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
pub unsafe fn extend_global_heap(additional: usize) {
    let _ = unsafe { try_extend_global_heap(additional) };
}

/// Extend the global heap explicitly, subject to the configured limit.
pub unsafe fn try_extend_global_heap(additional: usize) -> Result<(), ()> {
    #[cfg(all(not(feature = "std"), not(test)))]
    unsafe {
        let old_top = ALLOCATOR.top() as usize;
        ALLOCATOR.extend(additional)?;
        // Update the tracked heap range so page-fault detection still works.
        let new_top = ALLOCATOR.top() as usize;
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
        Ok(())
    }
    #[cfg(any(feature = "std", test))]
    {
        let _ = additional;
        Ok(())
    }
}

/// Return the current top address of the global heap.
///
/// Returns the pointer just past the end of the usable heap.
pub fn heap_top() -> *mut u8 {
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        ALLOCATOR.top()
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
        HeapStats {
            total: ALLOCATOR.size(),
            used: ALLOCATOR.used(),
            free: ALLOCATOR.free(),
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

#[cfg(test)]
mod tests {
    use super::GrowingHeap;
    use core::alloc::{GlobalAlloc, Layout};
    use core::mem::MaybeUninit;

    #[repr(align(4096))]
    struct TestHeap([MaybeUninit<u8>; 16 * 1024]);

    #[test]
    fn fragmented_heap_extends_and_retries_allocation() {
        let mut storage = TestHeap([MaybeUninit::uninit(); 16 * 1024]);
        let heap = GrowingHeap::empty();
        unsafe { heap.init(storage.0.as_mut_ptr().cast(), 4096) };
        heap.configure_extension(8 * 1024);

        let small = Layout::from_size_align(1500, 8).unwrap();
        let first = unsafe { GlobalAlloc::alloc(&heap, small) };
        let second = unsafe { GlobalAlloc::alloc(&heap, small) };
        assert!(!first.is_null() && !second.is_null());
        unsafe { GlobalAlloc::dealloc(&heap, first, small) };

        let large = Layout::from_size_align(2200, 8).unwrap();
        let allocation = unsafe { GlobalAlloc::alloc(&heap, large) };
        assert!(!allocation.is_null());
        assert!(heap.size() > 4096);
        unsafe {
            GlobalAlloc::dealloc(&heap, allocation, large);
            GlobalAlloc::dealloc(&heap, second, small);
        }
    }

    #[test]
    fn exhausted_extension_returns_null_without_retrying() {
        let mut storage = TestHeap([MaybeUninit::uninit(); 16 * 1024]);
        let heap = GrowingHeap::empty();
        unsafe { heap.init(storage.0.as_mut_ptr().cast(), 4096) };
        heap.configure_extension(0);

        let layout = Layout::from_size_align(8192, 8).unwrap();
        let allocation = unsafe { GlobalAlloc::alloc(&heap, layout) };
        assert!(allocation.is_null());
        assert_eq!(heap.size(), 4096);
    }
}
