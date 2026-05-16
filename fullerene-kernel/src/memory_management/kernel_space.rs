//! Kernel space memory initialization.
//!
//! Uses the declarative mapper for concise, safe initial mappings.

use crate::memory_management::KERNEL_OFFSET;
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

    mapper
        .map_region(virt, phys, size)
        .with_flags(Flags::DEVICE_MMIO)
        .huge_if_possible()
        .apply()
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
    // TODO: implement proper free VA search
    None
}
