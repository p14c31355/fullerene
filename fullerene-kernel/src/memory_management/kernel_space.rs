use alloc::collections::BTreeMap;
use spin::Mutex;
use petroleum::common::logging::SystemResult;

/// Kernel virtual address space allocated regions tracker
pub static KERNEL_VIRTUAL_ALLOCATED_REGIONS: Mutex<BTreeMap<usize, usize>> =
    Mutex::new(BTreeMap::new());

/// Helper for aligning sizes to page boundaries
pub fn align_page(size: usize) -> usize {
    const PAGE_SIZE: usize = 4096;
    (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Find a free virtual address range in the kernel space
pub fn find_free_virtual_address(size: usize) -> SystemResult<usize> {
    let size_aligned = align_page(size);
    let mut regions = KERNEL_VIRTUAL_ALLOCATED_REGIONS.lock();

    // Kernel space starts at 0xFFFF_8000_0000_0000
    let mut current_addr = 0xFFFF_8000_0000_0000;

    // Find a free gap large enough for the allocation
    for (&start, &size) in regions.iter() {
        let end = start + size;
        if current_addr + size_aligned <= start {
            // Found a gap
            break;
        }
        current_addr = end;
    }

    // Align to page boundary
    current_addr = align_page(current_addr);

    // Record the allocation
    regions.insert(current_addr, size_aligned);

    Ok(current_addr)
}