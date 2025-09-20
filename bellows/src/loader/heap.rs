// bellows/src/loader/heap.rs

use crate::uefi::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus, Result};
use linked_list_allocator::LockedHeap;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_heap(bs: &EfiBootServices) -> Result<()> {
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    let mut heap_phys: usize = 0;
    let status = {
        (bs.allocate_pages)(
            0usize,
            EfiMemoryType::EfiLoaderData,
            heap_pages,
            &mut heap_phys,
        )
    };
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed("Failed to allocate heap memory."));
    }

    if heap_phys == 0 {
        return Err(BellowsError::AllocationFailed("Allocated heap address is null."));
    }

    // Safety:
    // We have successfully allocated a valid, non-zero memory region
    // of size HEAP_SIZE. The `init` function correctly initializes the
    // allocator with this region.
    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }
    Ok(())
}
