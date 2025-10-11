//! Virtual memory management for Fullerene OS
//!
//! This module provides virtual memory management including:
//! - Page table management per process
//! - User space/kernel space separation
//! - Page fault handling
//! - Memory allocation and deallocation

use crate::heap::FRAME_ALLOCATOR;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB};
use x86_64::structures::paging::{Mapper, Page, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};

/// Page table for each process
pub struct ProcessPageTable {
    /// Physical address of the level 4 page table
    pub pml4_frame: PhysFrame,
    /// Virtual memory mapper
    pub mapper: OffsetPageTable<'static>,
}

/// Global frame allocator for physical memory
/// Use heap::FRAME_ALLOCATOR instead for consistency

/// Create a new page table for a process
pub fn create_process_page_table(physical_memory_offset: VirtAddr) -> Option<ProcessPageTable> {
    // Allocate a new level 4 page table frame
    let pml4_frame = FRAME_ALLOCATOR
        .get()
        .unwrap()
        .lock()
        .allocate_frame()
        .expect("Frame allocator not initialized");

    // Initialize the page table with kernel mappings
    let pml4: &mut PageTable = unsafe {
        &mut *(physical_memory_offset + pml4_frame.start_address().as_u64()).as_mut_ptr()
    };

    // Clear the page table
    pml4.zero();

    // Copy kernel page table mappings (higher half of virtual address space)
    let (current_frame, _) = x86_64::registers::control::Cr3::read();
    let current_pml4_virt = physical_memory_offset + current_frame.start_address().as_u64();
    let current_pml4 = unsafe { &*(current_pml4_virt.as_mut_ptr() as *const PageTable) };

    // Copy kernel mappings (entries 256-511 correspond to virtual addresses >= 0xFFFF800000000000)
    // We need to recursively copy all kernel page table entries, not just the top level
    unsafe {
        // Copy all entries directly using raw pointer operations
        // PageTableEntry is essentially a u64 wrapper, so this is safe
        ptr::copy_nonoverlapping(
            (current_pml4 as *const PageTable as *const PageTableEntry as *const u64).offset(256),
            (pml4 as *mut PageTable as *mut PageTableEntry as *mut u64).offset(256),
            256,
        );
    }

    let mapper = unsafe { OffsetPageTable::new(pml4, physical_memory_offset) };

    Some(ProcessPageTable { pml4_frame, mapper })
}

/// Map user-space virtual address to physical frame
pub fn map_user_page(
    page_table: &mut ProcessPageTable,
    virtual_addr: VirtAddr,
    physical_addr: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), MapError> {
    let page = Page::<Size4KiB>::containing_address(virtual_addr);
    let frame = PhysFrame::<Size4KiB>::containing_address(physical_addr);

    unsafe {
        page_table
            .mapper
            .map_to(
                page,
                frame,
                flags,
                &mut *FRAME_ALLOCATOR.get().unwrap().lock(),
            )
            .map_err(|_| MapError::MappingFailed)?
            .flush();
    }

    Ok(())
}

/// Unmap user-space virtual address
pub fn unmap_user_page(
    page_table: &mut ProcessPageTable,
    virtual_addr: VirtAddr,
) -> Result<(), MapError> {
    let page = Page::<Size4KiB>::containing_address(virtual_addr);

    let (_frame, flush) = page_table
        .mapper
        .unmap(page)
        .map_err(|_| MapError::UnmappingFailed)?;
    flush.flush();
    Ok(())
}

/// Allocate user-space memory for a process
pub fn allocate_user_memory(
    page_table: &mut ProcessPageTable,
    size_bytes: usize,
) -> Result<VirtAddr, AllocError> {
    let num_pages = (size_bytes + 4095) / 4096; // Round up to page size
    let frame_allocator = &mut *FRAME_ALLOCATOR.get().unwrap().lock();

    // Find a free virtual address range in user space (addresses below kernel space)
    // Kernel space typically starts at 0xFFFF_8000_0000_0000 in x86_64
    let base_addr = VirtAddr::new(0x200000); // 2MB base for user programs

    // For now, allocate sequentially (simple bump allocator)
    static NEXT_USER_ADDR: AtomicU64 = AtomicU64::new(0x200000);

    let start_addr = NEXT_USER_ADDR.fetch_add((num_pages * 4096) as u64, Ordering::Relaxed);

    let flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    for i in 0..num_pages {
        let page_addr = VirtAddr::new(start_addr + (i * 4096) as u64);
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(AllocError::OutOfMemory)?;

        map_user_page(page_table, page_addr, frame.start_address(), flags)?;
    }

    Ok(VirtAddr::new(start_addr))
}

/// Free user-space memory
pub fn free_user_memory(
    page_table: &mut ProcessPageTable,
    addr: VirtAddr,
    size_bytes: usize,
) -> Result<(), FreeError> {
    let num_pages = (size_bytes + 4095) / 4096;

    for i in 0..num_pages {
        let page_addr = addr + ((i * 4096) as u64);
        unmap_user_page(page_table, page_addr)?;
    }

    Ok(())
}

/// Switch to a process's page table (set CR3)
pub unsafe fn switch_to_page_table(page_table: &ProcessPageTable) {
    use x86_64::registers::control::Cr3;

    let (frame, _) = Cr3::read();
    let new_frame = page_table.pml4_frame;

    if frame != new_frame {
        Cr3::write(new_frame, Cr3::read().1);
    }
}

/// Check if address is in user space
pub fn is_user_address(addr: VirtAddr) -> bool {
    let addr_u64 = addr.as_u64();
    // User space: 0x0000_0000_0000_0000 to 0x0000_7FFF_FFFF_FFFF
    // Kernel space: 0xFFFF_8000_0000_0000 to 0xFFFF_FFFF_FFFF_FFFF
    addr_u64 < 0x800000000000
}

/// Check if address is in kernel space
pub fn is_kernel_address(addr: VirtAddr) -> bool {
    !is_user_address(addr)
}

/// Page fault error enumeration
#[derive(Debug, Clone, Copy)]
pub enum MapError {
    MappingFailed,
    UnmappingFailed,
    FrameAllocationFailed,
}

/// Memory allocation error
#[derive(Debug, Clone, Copy)]
pub enum AllocError {
    OutOfMemory,
    MappingFailed,
}

impl From<MapError> for AllocError {
    fn from(error: MapError) -> Self {
        match error {
            MapError::MappingFailed => AllocError::MappingFailed,
            MapError::UnmappingFailed => AllocError::MappingFailed,
            MapError::FrameAllocationFailed => AllocError::OutOfMemory,
        }
    }
}

/// Memory free error
#[derive(Debug, Clone, Copy)]
pub enum FreeError {
    UnmappingFailed,
}

impl From<MapError> for FreeError {
    fn from(error: MapError) -> Self {
        match error {
            MapError::MappingFailed => FreeError::UnmappingFailed,
            MapError::UnmappingFailed => FreeError::UnmappingFailed,
            MapError::FrameAllocationFailed => FreeError::UnmappingFailed,
        }
    }
}

/// Initialize virtual memory system
pub fn init() {
    // Initialize global frame allocator if not done
    if FRAME_ALLOCATOR.get().is_none() {
        // This would need the memory map - assume it's initialized elsewhere
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_checks() {
        let user_addr = VirtAddr::new(0x100000);
        let kernel_addr = VirtAddr::new(0xFFFF800000000000);

        assert!(is_user_address(user_addr));
        assert!(!is_user_address(kernel_addr));
        assert!(!is_kernel_address(user_addr));
        assert!(is_kernel_address(kernel_addr));
    }
}
