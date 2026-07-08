use petroleum::initializer::FrameAllocator;
use x86_64::structures::paging::PageTableFlags;

#[derive(Debug)]
pub enum MemError {
    OutOfMemory,
    MmioMappingFailed,
}

pub(crate) fn phys_to_virt(phys: u64) -> usize {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    (phys + off) as usize
}

pub(crate) fn allocate_frame() -> Result<u64, MemError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(MemError::OutOfMemory)?;
    m.allocate_frame().map_err(|_| MemError::OutOfMemory).map(|p| p as u64)
}

pub(crate) fn allocate_contiguous(count: usize) -> Result<u64, MemError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(MemError::OutOfMemory)?;
    m.allocate_contiguous_frames(count).map_err(|_| MemError::OutOfMemory).map(|p| p as u64)
}

pub(crate) fn map_mmio(phys: usize, virt: usize, size: usize) -> Result<(), MemError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(MemError::MmioMappingFailed)?;
    m.map_mmio_region(phys, virt, size).map_err(|_| MemError::MmioMappingFailed)
}

pub(crate) fn map_page(virt: usize, phys: usize, writable: bool, executable: bool) -> Result<(), MemError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(MemError::MmioMappingFailed)?;

    let mut pte_flags = PageTableFlags::PRESENT;
    if !executable { pte_flags |= PageTableFlags::NO_EXECUTE; }
    if writable { pte_flags |= PageTableFlags::WRITABLE; }

    m.safe_map_page(virt, phys, pte_flags).map_err(|_| MemError::MmioMappingFailed)
}

pub(crate) fn free_frame(phys: u64) {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    if let Some(m) = mgr.as_mut() { let _ = m.free_frame(phys as usize); }
}

pub(crate) fn free_contiguous(phys: u64, count: usize) {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    if let Some(m) = mgr.as_mut() { let _ = m.free_contiguous_frames(phys as usize, count); }
}

pub(crate) fn iommu_dma_map(device_id: u16, phys: u64, size: usize) -> Result<u64, ()> {
    let ctx = crate::driver_context_impl::KernelDriverContext;
    nitrogen::iommu::dma_map_with_ctx(&ctx, device_id, phys, size).map_err(|_| ())
}

pub(crate) fn iommu_dma_unmap(iova: u64, size: usize) {
    let ctx = crate::driver_context_impl::KernelDriverContext;
    nitrogen::iommu::dma_unmap(&ctx, iova, size);
}
