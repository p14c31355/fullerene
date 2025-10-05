use core::{mem::MaybeUninit, ptr::NonNull};
use linked_list_allocator::LockedHeap;
use x86_64::{PhysAddr, VirtAddr};
use alloc::alloc::{alloc, Layout};

pub const HEAP_SIZE: usize = 100 * 1024; // 100 KiB

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init(heap_start: VirtAddr, heap_size: usize) {
    unsafe {
        let heap_start = heap_start.as_mut_ptr::<u8>();
        ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

// Allocate heap from memory map (simplified: assume identity mapping for now)
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    // Simplified: assume identity mapping (implement proper page table later)
    VirtAddr::new(phys_start.as_u64())
}
