use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::boxed::Box;

const HEAP_SIZE: usize = 128 * 1024;

#[repr(align(16))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0; HEAP_SIZE]);

struct BumpAllocator {
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    next: AtomicUsize::new(0),
};

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = unsafe { core::ptr::addr_of_mut!(HEAP_STORAGE.0) as usize };
        let end = base.saturating_add(HEAP_SIZE);
        let align_mask = layout.align().saturating_sub(1);

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = match base
                .saturating_add(current)
                .checked_add(align_mask)
                .map(|address| address & !align_mask)
            {
                Some(address) if address < end => address,
                _ => return core::ptr::null_mut(),
            };
            let offset = aligned - base;
            let new_next = match offset.checked_add(layout.size()) {
                Some(value) if aligned.saturating_add(layout.size()) <= end => value,
                _ => return core::ptr::null_mut(),
            };
            if self
                .next
                .compare_exchange(current, new_next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    super::uart::puts("aarch64 allocator exhausted\n");
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

pub fn smoke() {
    let value = Box::new(0x_f00d_u64);
    assert_eq!(*value, 0x_f00d);
}
