//! Unified Memory Management Implementation
//!
//! This module provides a comprehensive memory management system that implements
//! the MemoryManager, ProcessMemoryManager, PageTableHelper, and FrameAllocator traits.

use spin::Mutex;

use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::initializer::{FrameAllocator, Initializable, MemoryManager};
use petroleum::mem_debug;
use x86_64::structures::paging::{
    PageTable, PageTableFlags as PageFlags, page_table::PageTableEntry,
};
use x86_64::{PhysAddr, VirtAddr};

use petroleum::page_table::allocator::traits::FrameAllocatorExt;
use petroleum::page_table::process::ProcessPageTable;
use petroleum::page_table::types::PageTableHelper;
pub mod convenience;
pub mod kernel_space;
pub mod manager;
pub mod process_memory;

pub use manager::UnifiedMemoryManager;
pub use process_memory::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTableWalkError {
    NotPresent { level: usize },
    HugePage { level: usize },
}

/// Visit the P4-to-P1 entries for one address in a page-table hierarchy.
///
/// The caller supplies the physical root because loader code may inspect a
/// process table before switching CR3, while exception code uses the current
/// CR3. The callback is invoked in order from P4 through P1.
///
/// # Safety
///
/// `root` must identify a valid page-table hierarchy accessible through the
/// configured physical-memory offset. The callback must not retain the entry
/// reference after it returns.
pub unsafe fn walk_page_table_entries(
    root: PhysAddr,
    address: u64,
    mut visit: impl FnMut(usize, &mut PageTableEntry),
) -> Result<(), PageTableWalkError> {
    let offset = VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let virtual_address = VirtAddr::new(address);
    let indexes = [
        virtual_address.p4_index(),
        virtual_address.p3_index(),
        virtual_address.p2_index(),
        virtual_address.p1_index(),
    ];
    let mut table = (offset + root.as_u64()).as_mut_ptr::<PageTable>();

    for (level, index) in indexes.into_iter().enumerate() {
        let entry = unsafe { &mut (&mut *table)[index] };
        let flags = entry.flags();
        if !flags.contains(PageFlags::PRESENT) {
            return Err(PageTableWalkError::NotPresent { level });
        }
        if level < 3 && flags.contains(PageFlags::HUGE_PAGE) {
            return Err(PageTableWalkError::HugePage { level });
        }
        visit(level, entry);
        if level < 3 {
            table = (offset + entry.addr().as_u64()).as_mut_ptr::<PageTable>();
        }
    }
    Ok(())
}

/// Configure the PAT MSR with the OS-defined memory type table.
///
/// Corresponds to Linux `pat_bp_init()`.  Sets all eight PAT entries:
///
/// ```text
/// Slot  PAT PCD PWT   Type
///  0     0   0   0     WB   (default RAM)
///  1     0   0   1     WC   (framebuffer write-combining)
///  2     0   1   0     UC-
///  3     0   1   1     UC
///  4     1   0   0     WB
///  5     1   0   1     WP
///  6     1   1   0     UC-
///  7     1   1   1     WT
/// ```
///
/// This is the same full PAT table Linux uses on modern CPUs.
pub fn configure_framebuffer_pat() -> bool {
    let pat_supported = core::arch::x86_64::__cpuid(1).edx & (1 << 16) != 0;
    if !pat_supported {
        return false;
    }
    unsafe {
        petroleum::page_table::pat::init_pat();
    }
    true
}

// Memory management error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
    MappingFailed,
}

petroleum::error_chain!(AllocError, petroleum::common::logging::SystemError,
    AllocError::OutOfMemory => petroleum::common::logging::SystemError::MemOutOfMemory,
    AllocError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeError {
    UnmappingFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    MappingFailed,
    UnmappingFailed,
    FrameAllocationFailed,
}

petroleum::error_chain!(MapError, petroleum::common::logging::SystemError,
    MapError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
    MapError::UnmappingFailed => petroleum::common::logging::SystemError::UnmappingFailed,
    MapError::FrameAllocationFailed => petroleum::common::logging::SystemError::FrameAllocationFailed,
);

petroleum::error_chain!(FreeError, petroleum::common::logging::SystemError,
    FreeError::UnmappingFailed => petroleum::common::logging::SystemError::UnmappingFailed,
);

// Global memory manager instance
static MEMORY_MANAGER: Mutex<Option<UnifiedMemoryManager>> = Mutex::new(None);

/// Switch to a specific page table
pub fn switch_to_page_table(page_table: &ProcessPageTable) -> SystemResult<()> {
    let pml4_frame = page_table.pml4_frame().ok_or(SystemError::InternalError)?;
    petroleum::safe_cr3_write!(pml4_frame);
    Ok(())
}

/// Create a new process page table
pub fn create_process_page_table() -> SystemResult<ProcessPageTable> {
    mem_debug!("Mem: create_process_page_table start\n");

    // Allocate a new PML4 frame for the process page table
    let pml4_phys = {
        let mut manager_guard = get_memory_manager().lock();
        let manager = manager_guard.as_mut().ok_or(SystemError::InternalError)?;
        manager
            .allocate_frame()
            .map_err(|_| SystemError::FrameAllocationFailed)?
    };

    let pml4_frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
        x86_64::PhysAddr::new(pml4_phys as u64),
    );

    // Zero the allocated page table frame using Direct Mapping
    let pml4_virt = petroleum::common::memory::physical_to_virtual(pml4_phys);

    unsafe {
        let table_ptr = pml4_virt as *mut u64;
        core::slice::from_raw_parts_mut(table_ptr, 512).fill(0);
    }

    // Copy kernel mappings to the new page table (PML4[256..512])
    let current_cr3 = x86_64::registers::control::Cr3::read();
    let kernel_table_phys = current_cr3.0.start_address().as_u64() as usize;
    let kernel_table_virt = petroleum::common::memory::physical_to_virtual(kernel_table_phys);

    unsafe {
        let kernel_entries_src = (kernel_table_virt + 256 * 8) as *const u64;
        let new_entries_dst = (pml4_virt + 256 * 8) as *mut u64;
        core::ptr::copy_nonoverlapping(kernel_entries_src, new_entries_dst, 256);
    }

    // Initialize the new page table manager with the allocated frame.
    // We set up the OffsetPageTable mapper pointing to the new PML4 (not the
    // current CR3), so PageTableHelper::map_page modifies the process's page
    // table, not the kernel's.
    let mut page_table_manager = ProcessPageTable::new_with_frame(pml4_frame);
    let phys_offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    {
        use x86_64::structures::paging::{OffsetPageTable, PageTable};
        let l4_virt = phys_offset + pml4_frame.start_address().as_u64();
        let mapper =
            unsafe { OffsetPageTable::new(&mut *(l4_virt.as_mut_ptr::<PageTable>()), phys_offset) };
        page_table_manager.mapper = Some(mapper);
        page_table_manager.initialized = true;
    }
    // Ensure init is called for the Initializable contract
    Initializable::init(&mut page_table_manager)?;

    mem_debug!("Mem: create_process_page_table done\n");
    Ok(page_table_manager)
}

/// Deallocate a process page table and free its frames
pub fn deallocate_process_page_table(pml4_frame: x86_64::structures::paging::PhysFrame) {
    if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
        let frame_addr = pml4_frame.start_address().as_u64() as usize;
        let _ = manager.free_frame(frame_addr);
        mem_debug!("Mem: Deallocated process page table\n");
    }
}

/// Reclaim empty user page-table levels below a process PML4.
///
/// The x86 mapper removes leaf entries but intentionally leaves intermediate
/// tables allocated. Process-owned mappings can therefore release their empty
/// P1/P2/P3 tables after the leaf pages have been unmapped.
pub(crate) unsafe fn reclaim_empty_user_page_tables(
    root: PhysAddr,
    address: u64,
    allocator: &mut impl FrameAllocatorExt,
) {
    let offset = VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let virtual_address = VirtAddr::new(address);
    let indexes = [
        virtual_address.p4_index(),
        virtual_address.p3_index(),
        virtual_address.p2_index(),
        virtual_address.p1_index(),
    ];
    let mut table = (offset + root.as_u64()).as_mut_ptr::<PageTable>();
    let mut path = [None; 3];

    for (level, index) in indexes.into_iter().enumerate().take(3) {
        let entry = unsafe { &mut (&mut *table)[index] };
        let flags = entry.flags();
        if !flags.contains(PageFlags::PRESENT) || flags.contains(PageFlags::HUGE_PAGE) {
            return;
        }
        let child = entry.addr();
        path[level] = Some((table, index, child));
        table = (offset + child.as_u64()).as_mut_ptr::<PageTable>();
    }

    for level in (0..3).rev() {
        let Some((parent, index, child)) = path[level] else {
            break;
        };
        let child_table = unsafe { &*(offset + child.as_u64()).as_ptr::<PageTable>() };
        if child_table.iter().any(|entry| !entry.is_unused()) {
            break;
        }
        unsafe { (&mut *parent)[index].set_unused() };
        allocator.deallocate_frame(petroleum::page_table::types::PhysFrame {
            start_address: child.as_u64(),
        });
    }
}

/// Unmap and free process-owned pages in a user address range.
pub fn unmap_process_pages(
    page_table: &mut ProcessPageTable,
    start: u64,
    length: u64,
    free_leaf_frames: bool,
) {
    let Some(end) = start.checked_add(length) else {
        return;
    };
    petroleum::page_table::constants::with_frame_allocator(|allocator| {
        let mut address = start & !(4096 - 1);
        while address < end {
            if let Ok(frame) = page_table.unmap_page(address as usize) {
                if free_leaf_frames {
                    allocator.deallocate_frame(petroleum::page_table::types::PhysFrame {
                        start_address: frame.start_address().as_u64(),
                    });
                }
            }
            if let Some(root) = page_table.pml4_frame() {
                unsafe {
                    reclaim_empty_user_page_tables(root.start_address(), address, allocator);
                }
            }
            address = match address.checked_add(4096) {
                Some(next) => next,
                None => break,
            };
        }
    });
}

/// Initialize the global memory manager
pub fn init_memory_manager(
    memory_map: &[impl petroleum::page_table::types::MemoryDescriptorValidator],
) -> SystemResult<()> {
    mem_debug!("Mem: init_memory_manager entered\n");

    let mut manager = MEMORY_MANAGER.lock();
    let mut memory_manager = UnifiedMemoryManager::new();

    if let Err(e) = memory_manager.init(memory_map) {
        mem_debug!("Mem: UnifiedMemoryManager::init failed!\n");
        return Err(e);
    }

    *manager = Some(memory_manager);
    mem_debug!("Mem: Global memory manager initialized\n");
    Ok(())
}

/// Get a reference to the global memory manager
pub fn get_memory_manager() -> &'static Mutex<Option<UnifiedMemoryManager>> {
    &MEMORY_MANAGER
}

/// Map one page from the statically reserved kernel-heap extension.
///
/// This is intentionally a non-blocking helper for the page-fault handler. A
/// heap extension can fault while `Heap::extend()` is writing its new
/// free-list node; mapping the page and returning from the exception lets the
/// CPU continue that instruction. If the manager lock is already held, refuse
/// recovery rather than risking an exception-handler deadlock.
pub fn try_map_kernel_heap_extension_page(address: usize) -> bool {
    if !crate::heap::is_reserved_extension_address(address) {
        return false;
    }

    let page = address & !0xfff;
    let offset = petroleum::common::memory::get_physical_memory_offset();
    let Some(physical) = page.checked_sub(offset) else {
        return false;
    };
    let Some(mut manager) = get_memory_manager().try_lock() else {
        return false;
    };
    let Some(manager) = manager.as_mut() else {
        return false;
    };

    manager
        .safe_map_page(
            page,
            physical,
            x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE,
        )
        .is_ok()
}

/// Return the physical address of the kernel page table used by the idle
/// context. Kernel processes have their own cloned page tables, so switching
/// back to the idle process must explicitly restore this CR3 value.
pub fn kernel_page_table_phys() -> x86_64::PhysAddr {
    MEMORY_MANAGER
        .lock()
        .as_ref()
        .map(|manager| x86_64::PhysAddr::new(manager.kernel_pml4_phys as u64))
        .unwrap_or_else(|| x86_64::PhysAddr::new(0))
}

/// Map a user page for kernel access
pub fn map_user_page(
    virtual_addr: usize,
    physical_addr: usize,
    flags: PageFlags,
) -> SystemResult<()> {
    if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
        manager
            .page_table_manager
            .map_page(virtual_addr, physical_addr, flags, unsafe {
                petroleum::page_table::constants::get_frame_allocator_mut()
            })
    } else {
        Err(SystemError::InternalError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_memory_manager_creation() {
        let manager = UnifiedMemoryManager::new();
        assert_eq!(manager.name(), "UnifiedMemoryManager");
        assert_eq!(manager.priority(), 1000);
        assert!(!manager.is_initialized());
    }
}
