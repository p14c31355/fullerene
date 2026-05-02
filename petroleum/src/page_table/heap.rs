use core::sync::atomic::{AtomicBool, Ordering};
use crate::common::memory::set_heap_range;
use x86_64::PhysAddr;

pub static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(feature = "std"), not(test)))]
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

#[cfg(all(not(feature = "std"), test))]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

pub fn init_global_heap(ptr: *mut u8, size: usize) {
    #[cfg(all(not(feature = "std"), not(test)))]
    if !HEAP_INITIALIZED.load(Ordering::SeqCst) {
        unsafe {
            ALLOCATOR.lock().init(ptr, size);
        }
        set_heap_range(ptr as usize, size);
        HEAP_INITIALIZED.store(true, Ordering::SeqCst);
    }
}

/// Allocate heap memory from EFI memory map
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> PhysAddr {
    const FRAME_SIZE: u64 = 4096;
    let _heap_frames = (heap_size + FRAME_SIZE as usize - 1) / FRAME_SIZE as usize;

    let aligned_start = if start_addr.as_u64() % FRAME_SIZE == 0 {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    aligned_start
}