// bellows/src/loader/heap.rs

use linked_list_allocator::LockedHeap;
use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus};
use x86_64::instructions::port::Port; // Import Port for direct I/O

/// Writes a single byte to the COM1 serial port (0x3F8).
/// This is a very basic, early debug function that doesn't rely on any complex initialization.
fn debug_print_byte(byte: u8) {
    let mut port = Port::new(0x3F8);
    unsafe {
        // Wait until the transmit buffer is empty
        while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
        port.write(byte);
    }
}

/// Writes a string to the COM1 serial port.
fn debug_print_str(s: &str) {
    for byte in s.bytes() {
        debug_print_byte(byte);
    }
}

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 64 * 1024; // 64 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_print_str("Heap: Allocating pages for heap...\n");
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
        debug_print_str("Heap: Failed to allocate heap memory.\n");
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate heap memory.",
        ));
    }

    debug_print_str("Heap: Allocated heap memory.\n");
    if heap_phys == 0 {
        debug_print_str("Heap: Allocated heap address is null!\n");
        return Err(BellowsError::AllocationFailed(
            "Allocated heap address is null.",
        ));
    }

    debug_print_str("Heap: Initializing allocator...\n");
    // Safety:
    // We have successfully allocated a valid, non-zero memory region
    // of size HEAP_SIZE. The `init` function correctly initializes the
    // allocator with this region.
    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, HEAP_SIZE);
    }
    debug_print_str("Heap: Allocator initialized successfully.\n");
    Ok(())
}
