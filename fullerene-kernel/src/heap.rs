use core::mem::MaybeUninit;
use linked_list_allocator::LockedHeap;

const HEAP_SIZE: usize = 100 * 1024; // 100 KiB for now

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

static HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

pub fn init() {
    unsafe {
        let heap_start = HEAP.as_ptr() as *mut u8;
        ALLOCATOR.lock().init(heap_start, HEAP_SIZE);
    }
}
