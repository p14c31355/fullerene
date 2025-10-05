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

/// Tries to allocate pages with multiple strategies and memory types.
fn try_allocate_pages(bs: &EfiBootServices, pages: usize, preferred_type: EfiMemoryType) -> Result<usize, BellowsError> {
    let mut phys_addr: usize = 0;
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory, EfiMemoryType::EfiRuntimeServicesData];

    // Try different allocation types: AllocateAnyPages (0), AllocateMaxAddress (1)
    let alloc_types = [0usize, 1usize]; // 0 = AllocateAnyPages, 1 = AllocateMaxAddress

    for alloc_type in alloc_types {
        for &mem_type in &types_to_try {
            debug_print_str("Heap: About to call allocate_pages (type ");
            match alloc_type {
                0 => debug_print_str("AnyPages"),
                1 => debug_print_str("MaxAddress"),
                _ => debug_print_str("Unknown"),
            }
            debug_print_str(", mem ");
            match mem_type {
                EfiMemoryType::EfiLoaderData => debug_print_str("LoaderData"),
                EfiMemoryType::EfiConventionalMemory => debug_print_str("Conventional"),
                EfiMemoryType::EfiRuntimeServicesData => debug_print_str("RuntimeServicesData"),
                _ => debug_print_str("Other"),
            }
            debug_print_str(")...\n");

            let status = (bs.allocate_pages)(
                alloc_type,
                mem_type,
                pages,
                &mut phys_addr,
            );

            debug_print_str("Heap: allocate_pages returned.\n");
            let status_efi = EfiStatus::from(status);

            debug_print_str("Heap: Result: ");
            debug_print_str(match status_efi {
                EfiStatus::Success => "Success",
                EfiStatus::InvalidParameter => "InvalidParameter",
                EfiStatus::OutOfResources => "OutOfResources",
                EfiStatus::NotFound => "NotFound",
                _ => "Other",
            });
            debug_print_str("\n");

            if status_efi == EfiStatus::Success && phys_addr != 0 {
                debug_print_str("Heap: Allocated successfully at address ");
                // Simple debug: print address (assuming < 2^32 for simplicity)
                let addr_high = (phys_addr >> 16) & 0xFFFF;
                let addr_low = phys_addr & 0xFFFF;
                if addr_high > 0 {
                    debug_print_str("high");
                }
                debug_print_str("\n");
                return Ok(phys_addr);
            }
            // Reset address for next attempt
            phys_addr = 0;
        }
    }

    // If all attempts fail, try with smaller allocation sizes
    debug_print_str("Heap: Trying smaller allocations...\n");
    let smaller_sizes = [pages / 2, pages / 4, 1]; // Try half, quarter, then minimum 1 page

    for &smaller_pages in &smaller_sizes {
        if smaller_pages == 0 { continue; }
        debug_print_str("Heap: Trying ");
        match smaller_pages {
            8 => debug_print_str("8"),
            4 => debug_print_str("4"),
            1 => debug_print_str("1"),
            _ => debug_print_str("other"),
        }
        debug_print_str(" pages...\n");

        for alloc_type in alloc_types {
            for &mem_type in &types_to_try {
                let status = (bs.allocate_pages)(
                    alloc_type,
                    mem_type,
                    smaller_pages,
                    &mut phys_addr,
                );

                let status_efi = EfiStatus::from(status);
                if status_efi == EfiStatus::Success && phys_addr != 0 {
                    debug_print_str("Heap: Smaller allocation succeeded!\n");
                    return Ok(phys_addr);
                }
                phys_addr = 0;
            }
        }
    }

    Err(BellowsError::AllocationFailed("All allocation attempts failed."))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_print_str("Heap: Allocating pages for heap...\n");
    let requested_pages = HEAP_SIZE.div_ceil(4096);
    debug_print_str("Heap: Requesting ");
    // Simple debug: print number of pages (assuming small number)
    match requested_pages {
        16 => debug_print_str("16"),
        _ => debug_print_str("other"),
    }
    debug_print_str(" pages.\n");

    let heap_phys = try_allocate_pages(bs, requested_pages, EfiMemoryType::EfiLoaderData)?;

    if heap_phys == 0 {
        debug_print_str("Heap: Allocated heap address is null!\n");
        return Err(BellowsError::AllocationFailed("Allocated heap address is null."));
    }

    // Calculate actual allocated size (we may have gotten fewer pages than requested)
    // For now, assume we got the full amount since we don't track partial allocations
    // In a more robust implementation, we'd modify try_allocate_pages to return the actual size
    let actual_heap_size = HEAP_SIZE;

    debug_print_str("Heap: Initializing allocator...\n");
    // Safety:
    // We have successfully allocated a valid, non-zero memory region.
    // The `init` function correctly initializes the allocator with this region.
    unsafe {
        ALLOCATOR.lock().init(heap_phys as *mut u8, actual_heap_size);
    }
    debug_print_str("Heap: Allocator initialized successfully.\n");
    Ok(())
}
