// bellows/src/loader/heap.rs

use linked_list_allocator::LockedHeap;
use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus};
use petroleum::debug_log;
use petroleum::serial::debug_print_hex;

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
        let type_str = match mem_type {
            EfiMemoryType::EfiLoaderData => "LoaderData",
            EfiMemoryType::EfiConventionalMemory => "Conventional",
            _ => "Other",
        };
        debug_log!("Heap: About to call allocate_pages (type AnyPages, mem {}))", type_str);

        let mut phys_addr_local: usize = 0;
        debug_log!("Heap: Calling allocate_pages with pages={:x}, mem_type={:x}", pages, mem_type as usize);
        debug_log!("Heap: Entering allocate_pages call...");
        // Use AllocateAnyPages (0) for any mem
        let alloc_type = 0usize; // AllocateAnyPages
        let status = (bs.allocate_pages)(
            alloc_type,
            mem_type,
            pages, // Start with 1 for testing
            &mut phys_addr_local,
        );
        debug_log!("Heap: Exited allocate_pages call. phys_addr_local={:x}, raw_status=0x{:x}", phys_addr_local, status);

        // Immediate validation: check if phys_addr_local is page-aligned (avoid invalid reads)
        if phys_addr_local != 0 && !phys_addr_local.is_multiple_of(4096) {
            debug_log!("Heap: WARNING: phys_addr_local not page-aligned!");
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
        debug_log!("Heap: Status: {}", status_str);

        if status_efi == EfiStatus::InvalidParameter {
            debug_log!("Heap: -> Skipping invalid type.");
            continue; // Ignore Conventional memory type
        }

        if status_efi == EfiStatus::Success && phys_addr_local != 0 {
            debug_log!("Heap: Allocated at address, aligned OK.");
            return Ok(phys_addr_local);
        }
    }

    Err(BellowsError::AllocationFailed(
        "All allocation attempts failed.",
    ))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_log!("Heap: Allocating pages for heap...");
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    debug_log!("Heap: Requesting {:x} pages for heap.", heap_pages);
    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?; // 固定

    if heap_phys == 0 {
        debug_log!("Heap: Allocated heap address is null!");
        return Err(BellowsError::AllocationFailed(
            "Allocated heap address is null.",
        ));
    }

    // Calculate actual allocated size (we may have gotten fewer pages than requested)
    // For now, assume we got the full amount since we don't track partial allocations
    // In a more robust implementation, we'd modify try_allocate_pages to return the actual size
    let actual_heap_size = heap_pages * 4096;

    debug_log!("Heap: Initializing allocator...");
    // Safety:
    // We have successfully allocated a valid, non-zero memory region.
    // The `init` function correctly initializes the allocator with this region.
    debug_log!("Heap: About to init ALLOCATOR...");
    unsafe {
        ALLOCATOR
            .lock()
            .init(heap_phys as *mut u8, actual_heap_size);
    }
    debug_log!("Heap: ALLOCATOR init done. Returning Ok(()).");
    Ok(())
}
