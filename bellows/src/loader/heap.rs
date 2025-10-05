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

/// Tries to allocate pages with a given memory type, with fallback.
fn try_allocate_pages(bs: &EfiBootServices, pages: usize, preferred_type: EfiMemoryType) -> Result<usize, BellowsError> {
    let mut phys_addr: usize = 0;
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory]; // Fallback

    for mem_type in types_to_try {
        let status = (bs.allocate_pages)(
            0usize, // AllocateAnyPages
            mem_type,
            pages,
            &mut phys_addr,
        );
        let status_efi = EfiStatus::from(status);
        debug_print_str("Heap: Tried allocate_pages with type ");
        // Simple debug: print type name
        match mem_type {
            EfiMemoryType::EfiLoaderData => debug_print_str("LoaderData"),
            EfiMemoryType::EfiConventionalMemory => debug_print_str("Conventional"),
            _ => debug_print_str("Other"),
        }
        debug_print_str(". Status: ");
        debug_print_str(match status_efi {
            EfiStatus::Success => "Success",
            EfiStatus::InvalidParameter => "InvalidParameter",
            EfiStatus::OutOfResources => "OutOfResources",
            EfiStatus::NotFound => "NotFound",
            _ => "Other",
        });
        debug_print_str("\n");

        if status_efi == EfiStatus::Success && phys_addr != 0 {
            debug_print_str("Heap: Allocated successfully.\n");
            return Ok(phys_addr);
        }
        // Reset address for next attempt
        phys_addr = 0;
    }

    Err(BellowsError::AllocationFailed("All allocation attempts failed."))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_print_str("Heap: Allocating pages for heap...\n");
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    debug_print_str("Heap: Requesting ");
    // Simple debug: print number of pages (assuming small number)
    match heap_pages {
        16 => debug_print_str("16"),
        _ => debug_print_str("other"),
    }
    debug_print_str(" pages.\n");

    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?;

    if heap_phys == 0 {
        debug_print_str("Heap: Allocated heap address is null!\n");
        return Err(BellowsError::AllocationFailed("Allocated heap address is null."));
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
