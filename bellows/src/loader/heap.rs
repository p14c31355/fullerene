// bellows/src/loader/heap.rs

use linked_list_allocator::LockedHeap;
use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus};

use super::debug::*;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 32 * 1024; // 32 KiB

/// Global allocator (linked-list allocator)
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Tries to allocate pages with multiple strategies and memory types.
fn try_allocate_pages(
    bs: &EfiBootServices,
    pages: usize,
    preferred_type: EfiMemoryType,
) -> Result<usize, BellowsError> {
    let mut phys_addr: usize = 0;
    // Try LoaderData first, then Conventional (skip if invalid)
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory];

    for mem_type in types_to_try {
        debug_print_str("Heap: About to call allocate_pages (type AnyPages, mem ");
        match mem_type {
            EfiMemoryType::EfiLoaderData => debug_print_str("LoaderData)...\n"),
            EfiMemoryType::EfiConventionalMemory => debug_print_str("Conventional)...\n"),
            _ => debug_print_str("Other)...\n"),
        };

        let mut phys_addr_local: usize = 0;
        debug_print_str("Heap: Calling allocate_pages with pages=");
        debug_print_hex(pages);
        debug_print_str(", mem_type=");
        debug_print_hex(mem_type as usize);
        debug_print_str("\n");
        debug_print_str("Heap: Entering allocate_pages call...\n");
        // Use AllocateAnyPages (0) for any mem
        let alloc_type = 0usize; // AllocateAnyPages
        let status = (bs.allocate_pages)(
            alloc_type,
            mem_type,
            pages, // Start with 1 for testing
            &mut phys_addr_local,
        );
        debug_print_str("Heap: Exited allocate_pages call.\n"); // Marker after call
        phys_addr = phys_addr_local;
        debug_print_str("Heap: allocate_pages returned, phys_addr=");
        debug_print_hex(phys_addr);
        debug_print_str(", raw_status=0x");
        debug_print_hex(status); // Print raw status as hex
        debug_print_str("\n");

        // Immediate validation: check if phys_addr is page-aligned (avoid invalid reads)
        if phys_addr != 0 && !phys_addr.is_multiple_of(4096) {
            debug_print_str("Heap: WARNING: phys_addr not page-aligned!\n");
            let _ = (bs.free_pages)(phys_addr, pages); // Ignore status on free
            phys_addr = 0;
            continue;
        }

        let status_efi = EfiStatus::from(status);
        debug_print_str("Heap: Status: ");
        debug_print_str(match status_efi {
            EfiStatus::Success => "Success",
            EfiStatus::OutOfResources => "OutOfResources",
            EfiStatus::InvalidParameter => "InvalidParameter",
            _ => "Other",
        });
        debug_print_str("\n");

        if status_efi == EfiStatus::InvalidParameter {
            debug_print_str("Heap: -> Skipping invalid type.\n");
            continue; // Ignore Conventional memory type
        }

        if status_efi == EfiStatus::Success && phys_addr != 0 {
            debug_print_str("Heap: Allocated at address, aligned OK.\n");
            return Ok(phys_addr);
        }
        phys_addr = 0;
    }

    Err(BellowsError::AllocationFailed(
        "All allocation attempts failed.",
    ))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_print_str("Heap: Allocating pages for heap...\n");
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    debug_print_str("Heap: Requesting ");
    debug_print_hex(heap_pages);
    debug_print_str(" pages for heap.\n");
    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?; // 固定

    if heap_phys == 0 {
        debug_print_str("Heap: Allocated heap address is null!\n");
        return Err(BellowsError::AllocationFailed(
            "Allocated heap address is null.",
        ));
    }

    // Calculate actual allocated size (we may have gotten fewer pages than requested)
    // For now, assume we got the full amount since we don't track partial allocations
    // In a more robust implementation, we'd modify try_allocate_pages to return the actual size
    let actual_heap_size = heap_pages * 4096;

    debug_print_str("Heap: Initializing allocator...\n");
    // Safety:
    // We have successfully allocated a valid, non-zero memory region.
    // The `init` function correctly initializes the allocator with this region.
    debug_print_str("Heap: About to init ALLOCATOR...\n");
    unsafe {
        ALLOCATOR
            .lock()
            .init(heap_phys as *mut u8, actual_heap_size);
    }
    debug_print_str("Heap: ALLOCATOR init done.\n");
    debug_print_str("Heap: Returning Ok(()).\n");
    Ok(())
}
