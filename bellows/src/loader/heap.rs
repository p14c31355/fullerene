// bellows/src/loader/heap.rs

use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus};
use petroleum::debug_log;
use petroleum::debug_log_no_alloc;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Tries to allocate pages with multiple strategies and memory types.
fn try_allocate_pages(
    bs: &EfiBootServices,
    pages: usize,
    preferred_type: EfiMemoryType,
) -> Result<usize, BellowsError> {
    // Try LoaderData first, then Conventional (skip if invalid)
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory];

    for mem_type in types_to_try {
        let type_str = match mem_type {
            EfiMemoryType::EfiLoaderData => "LoaderData",
            EfiMemoryType::EfiConventionalMemory => "Conventional",
            _ => "Other",
        };
        debug_log_no_alloc!("Heap: About to call allocate_pages mem_type=", mem_type as usize);

        let mut phys_addr_local: usize = 0;
        debug_log_no_alloc!("Heap: Calling allocate_pages pages=", pages);
        debug_log_no_alloc!("Heap: Calling allocate_pages mem_type=", mem_type as usize);
        debug_log_no_alloc!("Heap: Entering allocate_pages call...");
        // Use AllocateAnyPages (0) for any mem
        let alloc_type = 0usize; // AllocateAnyPages
        let status = (bs.allocate_pages)(
            alloc_type,
            mem_type,
            pages, // Start with 1 for testing
            &mut phys_addr_local,
        );
        debug_log_no_alloc!("Heap: Exited allocate_pages call phys_addr_local=", phys_addr_local);
        debug_log_no_alloc!("Heap: Exited allocate_pages call raw_status=", status);

        // Immediate validation: check if phys_addr_local is page-aligned (avoid invalid reads)
        if phys_addr_local != 0 && !phys_addr_local.is_multiple_of(4096) {
            debug_log_no_alloc!("Heap: WARNING: phys_addr_local not page-aligned!");
            let _ = (bs.free_pages)(phys_addr_local, pages); // Ignore status on free
            continue;
        }

        let status_efi = EfiStatus::from(status);
        let status_str = match status_efi {
            EfiStatus::Success => "Success",
            EfiStatus::OutOfResources => "OutOfResources",
            EfiStatus::InvalidParameter => "InvalidParameter",
            _ => "Other",
        };
        debug_log_no_alloc!("Heap: Status: ", status_efi as usize);

        if status_efi == EfiStatus::InvalidParameter {
            debug_log_no_alloc!("Heap: -> Skipping invalid type.");
            continue; // Ignore Conventional memory type
        }

        if status_efi == EfiStatus::Success && phys_addr_local != 0 {
            debug_log_no_alloc!("Heap: Allocated at address, aligned OK.");
            return Ok(phys_addr_local);
        }
    }

    Err(BellowsError::AllocationFailed(
        "All allocation attempts failed.",
    ))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_log_no_alloc!("Heap: Allocating pages for heap...");
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    debug_log_no_alloc!("Heap: Requesting pages=", heap_pages);
    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?; // 固定

    if heap_phys == 0 {
        debug_log_no_alloc!("Heap: Allocated heap address is null!");
        return Err(BellowsError::AllocationFailed(
            "Allocated heap address is null.",
        ));
    }

    // Calculate actual allocated size (we may have gotten fewer pages than requested)
    // For now, assume we got the full amount since we don't track partial allocations
    // In a more robust implementation, we'd modify try_allocate_pages to return the actual size
    let actual_heap_size = heap_pages * 4096;

    debug_log_no_alloc!("Heap: Initializing global allocator using petroleum...");
    petroleum::init_global_heap(heap_phys as *mut u8, actual_heap_size);
    debug_log_no_alloc!("Heap: Petroleum global heap init done. Returning Ok(()).");
    Ok(())
}
