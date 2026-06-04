//! Kernel space memory initialization.
//!
//! Uses the declarative mapper for concise, safe initial mappings.

use petroleum::page_table::KERNEL_OFFSET;
use petroleum::page_table::allocator::bitmap::BitmapFrameAllocator;
use petroleum::page_table::allocator::traits::FrameAllocatorExt;
use petroleum::page_table::kernel::mapper::{MapError, Mapper};
use petroleum::page_table::types::*;

/// Set up the kernel's initial page tables.
///
/// Maps:
/// - Higher-half kernel (identity-mapped at KERNEL_OFFSET)
/// - MMIO regions
/// - Framebuffer (if present)
///
/// Uses huge pages where alignment permits.
pub fn setup_kernel_space(
    root: &mut PageTable,
    allocator: &mut BitmapFrameAllocator,
) -> Result<(), MapError> {
    // Get total memory before creating mapper (which borrows allocator)
    let max_phys = allocator.total_memory();
    let mut mapper = Mapper::new(root, allocator);

    if max_phys > 0 {
        mapper
            .map_region(
                CanonicalVirtAddr::new(KERNEL_OFFSET.as_u64())
                    .expect("KERNEL_OFFSET is not canonical"),
                0,
                max_phys,
            )
            .with_flags(Flags::KERNEL_DATA)
            .huge_if_possible()
            .apply()?;
    }

    Ok(())
}

/// Map a specific MMIO region.
pub fn map_mmio(
    root: &mut PageTable,
    allocator: &mut BitmapFrameAllocator,
    phys: u64,
    size: u64,
) -> Result<(), MapError> {
    let mut mapper = Mapper::new(root, allocator);

    let virt = CanonicalVirtAddr::new(KERNEL_OFFSET.as_u64() + phys)
        .expect("MMIO virtual address is not canonical");

    petroleum::serial::serial_log(format_args!(
        "[map_mmio] virt={:#x}, phys={:#x}, size={:#x}\n",
        virt.as_u64(),
        phys,
        size
    ));

    let res = mapper
        .map_region(virt, phys, size)
        .with_flags(Flags::DEVICE_MMIO)
        .huge_if_possible()
        .apply();

    if let Err(e) = &res {
        petroleum::serial::serial_log(format_args!("[map_mmio] Failed: {:?}\n", e));
    }

    res
}

/// Map the framebuffer.
pub fn map_framebuffer(
    root: &mut PageTable,
    allocator: &mut BitmapFrameAllocator,
    phys: u64,
    size: u64,
) -> Result<(), MapError> {
    let mut mapper = Mapper::new(root, allocator);

    let virt = CanonicalVirtAddr::new(KERNEL_OFFSET.as_u64() + phys)
        .expect("framebuffer virtual address is not canonical");

    mapper
        .map_region(virt, phys, size)
        .with_flags(Flags::KERNEL_DATA | Flags::WRITE_THROUGH)
        .huge_if_possible()
        .apply()
}

/// Find a free virtual address region (backward-compat stub).
pub fn find_free_virtual_address(size: u64) -> Option<usize> {
    use petroleum::page_table::constants::HIGHER_HALF_OFFSET;
    use petroleum::page_table::raw::utils::is_mapped;
    use petroleum::page_table::types::{CanonicalVirtAddr, PageTable};

    // Search region: kernel dynamic allocations start above the direct-mapped region
    // Start search at a safe offset in the higher half
    const SEARCH_START: u64 = 0xFFFF_8800_0000_0000;
    const SEARCH_END: u64 = 0xFFFF_9000_0000_0000;
    const PAGE_SIZE: u64 = 4096;

    let pages_needed = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    let aligned_size = pages_needed * PAGE_SIZE;

    // Get the current page table root
    let cr3 = unsafe {
        let (frame, _) = x86_64::registers::control::Cr3::read();
        frame.start_address().as_u64()
    };

    // Convert physical address to virtual for accessing page table
    let root_virt = HIGHER_HALF_OFFSET.as_u64() + cr3;
    let root = unsafe { &*(root_virt as *const PageTable) };

    let mut candidate = SEARCH_START;

    while candidate + aligned_size <= SEARCH_END {
        // Check if this range is free
        let mut all_free = true;
        for page_offset in 0..pages_needed {
            let check_addr = candidate + (page_offset * PAGE_SIZE);
            if let Some(virt) = CanonicalVirtAddr::new(check_addr) {
                if is_mapped(root, virt) {
                    all_free = false;
                    // Skip to next page after this mapped one
                    candidate = check_addr + PAGE_SIZE;
                    break;
                }
            } else {
                all_free = false;
                break;
            }
        }

        if all_free {
            return Some(candidate as usize);
        }

        // Move to next aligned boundary
        candidate = (candidate + PAGE_SIZE) & !(PAGE_SIZE - 1);
    }

    None
}
