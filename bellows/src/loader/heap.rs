// bellows/src/loader/heap.rs

use crate::uefi::{EfiBootServices, EfiMemoryType, Result};
use linked_list_allocator::LockedHeap;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_heap(bs: &EfiBootServices) -> Result<()> {
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    let mut heap_phys: usize = 0;
    let status = (unsafe {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            heap_pages,
            &mut heap_phys,
        )
    });
    if status != 0 {
        return Err("Failed to allocate heap memory.");
    }
    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }
    Ok(())
}
