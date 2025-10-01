use linked_list_allocator::LockedHeap;

const HEAP_SIZE: usize = 100 * 1024; // 100 KiB for now

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(HEAP.as_mut_ptr(), HEAP_SIZE);
    }
}
