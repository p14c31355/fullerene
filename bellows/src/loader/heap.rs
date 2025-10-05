// bellows/src/loader/heap.rs

use core::arch::asm;
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
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory];

    for mem_type in types_to_try {
        debug_print_str("Heap: About to call allocate_pages (type AnyPages, mem ");
        match mem_type {
            EfiMemoryType::EfiLoaderData => debug_print_str("LoaderData)...\n"),
            EfiMemoryType::EfiConventionalMemory => debug_print_str("Conventional)...\n"),
            _ => debug_print_str("Other)...\n"),
        };
        
        let mut phys_addr_local: usize = 0;
        let status: usize;
        unsafe {
            asm!(
                "sub rsp, 40h", 
                "call rax",     
                "add rsp, 40h",
                in("rdi") 0usize,
                in("rsi") mem_type as usize,
                in("rdx") pages,
                inlateout("rcx") phys_addr_local => phys_addr_local,
                in("rax") bs.allocate_pages,
                lateout("rax") status,
                clobber_abi("system"),
            );
        }
        phys_addr = phys_addr_local;
        debug_print_str("Heap: allocate_pages returned.\n");

        // 即時検証: phys_addrがページ境界かチェック（Invalid read回避）
        if phys_addr != 0 && phys_addr % 4096 != 0 {
            debug_print_str("Heap: WARNING: phys_addr not page-aligned!\n");
            (bs.free_pages)(phys_addr, pages);  // 即時解放
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

        if status_efi == EfiStatus::Success && phys_addr != 0 {
            debug_print_str("Heap: Allocated at address, aligned OK.\n");
            return Ok(phys_addr);
        }
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
    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?;  // 固定
    // アライメント検証強化
    if heap_phys % 4096 != 0 {
        debug_print_str("Heap: Misaligned alloc! Freeing...\n");
        unsafe { (bs.free_pages)(heap_phys, heap_pages); }
        return Err(BellowsError::AllocationFailed("Misaligned heap allocation."));
    }

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
