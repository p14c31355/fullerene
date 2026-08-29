//! Explicit single-address-space memory policy for ESP32.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

pub const BOOT_HEAP_SIZE: usize = 168 * 1024;
pub const BOOT_STACK_SIZE: usize = 16 * 1024;
const TASK_STACK_REGION_SIZE: usize = 32 * 1024;
const HEAP_ALIGNMENT: usize = 16;

#[repr(C, align(16))]
struct HeapStorage([u8; BOOT_HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0; BOOT_HEAP_SIZE]);
static HEAP_CURSOR: AtomicUsize = AtomicUsize::new(0);

struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() > HEAP_ALIGNMENT {
            return core::ptr::null_mut();
        }
        let align = layout.align().max(4);
        let base = core::ptr::addr_of_mut!(HEAP_STORAGE).cast::<u8>();
        let base_address = base as usize;
        let mut current = HEAP_CURSOR.load(Ordering::Acquire);
        loop {
            // Align the absolute address, not just the cursor offset. The
            // storage is aligned to the largest alignment this allocator
            // accepts, but keeping the base in this calculation preserves the
            // contract if the storage placement changes later.
            let aligned_address = match base_address
                .checked_add(current)
                .and_then(|address| address.checked_add(align - 1))
            {
                Some(address) => address & !(align - 1),
                None => return core::ptr::null_mut(),
            };
            let aligned = match aligned_address.checked_sub(base_address) {
                Some(offset) => offset,
                None => return core::ptr::null_mut(),
            };
            let next = match aligned.checked_add(layout.size()) {
                Some(value) => value,
                None => return core::ptr::null_mut(),
            };
            if next > BOOT_HEAP_SIZE {
                return core::ptr::null_mut();
            }
            match HEAP_CURSOR.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return unsafe { base.add(aligned) };
                }
                Err(observed) => current = observed,
            }
        }
    }

    unsafe fn dealloc(&self, _pointer: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static BOOT_HEAP: BumpAllocator = BumpAllocator;

static mut TASK_STACK_STORAGE: [u8; TASK_STACK_REGION_SIZE] = [0; TASK_STACK_REGION_SIZE];
static TASK_STACK_CURSOR: AtomicUsize = AtomicUsize::new(0);

pub fn allocate_stack(size: usize) -> Option<&'static mut [u8]> {
    let align = 16;
    let mut current = TASK_STACK_CURSOR.load(Ordering::Acquire);
    loop {
        let aligned = current.div_ceil(align) * align;
        let next = aligned.checked_add(size)?;
        if next > TASK_STACK_REGION_SIZE {
            return None;
        }
        match TASK_STACK_CURSOR.compare_exchange_weak(
            current,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                let base = core::ptr::addr_of_mut!(TASK_STACK_STORAGE).cast::<u8>();
                let stack = unsafe { core::slice::from_raw_parts_mut(base.add(aligned), size) };
                stack.fill(0xaa);
                return Some(stack);
            }
            Err(observed) => current = observed,
        }
    }
}

pub fn init_heap() {
    HEAP_CURSOR.store(0, Ordering::Release);
}

pub fn used() -> usize {
    HEAP_CURSOR.load(Ordering::Acquire)
}

pub fn capacity() -> usize {
    BOOT_HEAP_SIZE
}

pub fn available() -> usize {
    BOOT_HEAP_SIZE.saturating_sub(used())
}
