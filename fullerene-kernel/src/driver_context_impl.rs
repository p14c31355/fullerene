//! Kernel-side implementation of `nitrogen::DriverContext`.
//!
//! Bridges nitrogen's abstract allocation/MMIO trait to the kernel's
//! concrete memory manager and page-table infrastructure.

use nitrogen::{DriverContext, DriverContextError, PageFlags};
use petroleum::initializer::FrameAllocator;
use x86_64::structures::paging::PageTableFlags;

/// The kernel's concrete implementation of [`DriverContext`].
///
/// This is a zero-sized type — the kernel's memory manager and
/// physical-memory offset are global singletons, so the trait methods
/// are stateless.
pub struct KernelDriverContext;

impl DriverContext for KernelDriverContext {
    fn phys_to_virt(&self, phys: u64) -> usize {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        (phys + off) as usize
    }

    fn allocate_frame(&self) -> Result<u64, DriverContextError> {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        let m = mgr.as_mut().ok_or(DriverContextError::OutOfMemory)?;
        let phys = m
            .allocate_frame()
            .map_err(|_| DriverContextError::OutOfMemory)?;
        Ok(phys as u64)
    }

    fn allocate_contiguous_frames(&self, count: usize) -> Result<u64, DriverContextError> {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        let m = mgr.as_mut().ok_or(DriverContextError::OutOfMemory)?;
        let phys = m
            .allocate_contiguous_frames(count)
            .map_err(|_| DriverContextError::OutOfMemory)?;
        Ok(phys as u64)
    }

    fn map_mmio_region(
        &self,
        phys: usize,
        virt: usize,
        size: usize,
    ) -> Result<(), DriverContextError> {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        let m = mgr.as_mut().ok_or(DriverContextError::MmioMappingFailed)?;
        m.map_mmio_region(phys, virt, size)
            .map_err(|_| DriverContextError::MmioMappingFailed)
    }

    fn map_page(
        &self,
        virt: usize,
        phys: usize,
        flags: PageFlags,
    ) -> Result<(), DriverContextError> {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        let m = mgr.as_mut().ok_or(DriverContextError::MmioMappingFailed)?;

        let mut pte_flags = PageTableFlags::PRESENT;
        if !flags.executable {
            pte_flags |= PageTableFlags::NO_EXECUTE;
        }
        if flags.writable {
            pte_flags |= PageTableFlags::WRITABLE;
        }
        if flags.write_combining {
            pte_flags |= PageTableFlags::WRITE_THROUGH;
        }

        m.safe_map_page(virt, phys, pte_flags)
            .map_err(|_| DriverContextError::MmioMappingFailed)
    }

    fn free_frame(&self, phys: u64) {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        if let Some(m) = mgr.as_mut() {
            let _ = m.free_frame(phys as usize);
        }
    }

    fn free_contiguous_frames(&self, phys: u64, count: usize) {
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        if let Some(m) = mgr.as_mut() {
            let _ = m.free_contiguous_frames(phys as usize, count);
        }
    }

    fn dma_map(&self, device_id: u16, phys: u64, size: usize) -> Result<u64, DriverContextError> {
        // IOMMU is a global singleton; delegate to it.
        // If no IOMMU is present, returns phys (identity mapping).
        nitrogen::iommu::dma_map_with_ctx(self, device_id, phys, size)
    }

    fn dma_unmap(&self, iova: u64, size: usize) {
        nitrogen::iommu::dma_unmap(self, iova, size);
    }
}
