//! # Early Boot Page Table Mapper
//!
//! Identity-mapped page table construction for the bootloader phase.
//! This mapper is used BEFORE the world switch — after switching to the kernel's
//! higher-half page tables, use `page_table::kernel` instead.
//!
//! ## Rationale
//!
//! During early boot:
//! - We are running under **identity mapping** (virtual == physical)
//! - Page table entries are accessed through their physical addresses
//! - The frame allocator is unstable (still being populated from the UEFI memory map)
//!
//! The runtime kernel has completely different paging assumptions (higher-half,
//! managed allocator, full `OffsetPageTable` with `Mapper` trait). Mixing the two
//! causes stale references and ABI mismatches.
//!
//! ## Usage
//!
//! ```ignore
//! use petroleum::early::mapper::EarlyMapper;
//! use petroleum::early::allocator::EarlyFrameAllocator;
//!
//! let alloc = EarlyFrameAllocator::init_with_memory_map(&memory_map);
//! let mapper = EarlyMapper::new(alloc);
//! mapper.map_4k(virt_addr, phys_addr, flags);
//! ```

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    },
};

use crate::early::allocator::EarlyFrameAllocator;

// ── Re-export boot-only mapping functions from page_table::kernel::init ──
// These functions are used during the boot → kernel transition (world switch)
// and should NOT be called by runtime kernel code.
pub use crate::page_table::kernel::init::{InitAndJumpArgs, init_and_jump, map_page_4k_l1};

/// Early-boot page table mapper for identity-mapped environments.
///
/// After construction, this mapper operates on the **active** page table
/// (read from CR3). All new tables are allocated from the provided
/// `EarlyFrameAllocator`.
pub struct EarlyMapper {
    /// Offset used to access page table structures.
    /// During early boot this is typically `VirtAddr::new(0)` (identity mapping).
    phys_offset: VirtAddr,
}

impl EarlyMapper {
    /// Create a new early mapper with the given physical offset.
    ///
    /// `phys_offset` should be `VirtAddr::new(0)` for pure identity mapping.
    /// If you need to access page tables through a higher-half alias, pass
    /// that offset instead.
    pub fn new(phys_offset: VirtAddr) -> Self {
        Self { phys_offset }
    }

    /// Get the active L4 page table (read from CR3), accessed through `phys_offset`.
    ///
    /// # Safety
    /// CR3 must point to a valid page table that is accessible through `phys_offset`.
    pub unsafe fn active_l4_table(&self) -> &'static mut PageTable {
        let (l4_frame, _) = x86_64::registers::control::Cr3::read();
        let l4_phys = l4_frame.start_address().as_u64();
        let l4_virt = l4_phys + self.phys_offset.as_u64();
        &mut *(l4_virt as *mut PageTable)
    }

    /// Create an `OffsetPageTable` mapper for the active L4 table.
    ///
    /// # Safety
    /// Same as `active_l4_table`.
    pub unsafe fn offset_mapper(&self) -> OffsetPageTable<'static> {
        OffsetPageTable::new(self.active_l4_table(), self.phys_offset)
    }

    /// Map a single 4 KiB page at the given virtual address to the given physical address.
    ///
    /// `frame_allocator` is used to allocate new page-table pages if needed.
    ///
    /// # Safety
    /// The caller must ensure the virtual address is not already mapped, or that
    /// remapping is intentional.
    pub unsafe fn map_4k(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageTableFlags,
        frame_allocator: &mut EarlyFrameAllocator,
    ) -> Result<(), &'static str> {
        let mut mapper = self.offset_mapper();
        let page = Page::<Size4KiB>::containing_address(virt);
        let frame = PhysFrame::containing_address(phys);
        // SAFETY: The caller ensures safety.
        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .map_err(|_| "map_4k failed")?
                .flush();
        }
        Ok(())
    }

    /// Map a range of 4 KiB pages.
    ///
    /// # Safety
    /// Same as `map_4k`.
    pub unsafe fn map_range_4k(
        &self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        page_count: u64,
        flags: PageTableFlags,
        frame_allocator: &mut EarlyFrameAllocator,
    ) -> Result<(), &'static str> {
        for i in 0..page_count {
            let virt = VirtAddr::new(virt_start.as_u64() + i * 4096);
            let phys = PhysAddr::new(phys_start.as_u64() + i * 4096);
            self.map_4k(virt, phys, flags, frame_allocator)?;
        }
        Ok(())
    }

    /// Map a range of 2 MiB huge pages.
    ///
    /// Both `virt_start` and `phys_start` must be 2 MiB-aligned.
    ///
    /// # Safety
    /// Same as `map_4k`.
    pub unsafe fn map_range_2mb(
        &self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        page_count: u64,
        flags: PageTableFlags,
        frame_allocator: &mut EarlyFrameAllocator,
    ) -> Result<(), &'static str> {
        let flags_2mb = flags | PageTableFlags::HUGE_PAGE;
        for i in 0..page_count {
            let virt = VirtAddr::new(virt_start.as_u64() + i * 2 * 1024 * 1024);
            let phys = PhysAddr::new(phys_start.as_u64() + i * 2 * 1024 * 1024);
            let mut mapper = self.offset_mapper();
            let page = Page::<x86_64::structures::paging::Size2MiB>::containing_address(virt);
            let frame =
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size2MiB>::containing_address(phys);
            // SAFETY: Caller ensures safety.
            unsafe {
                mapper
                    .map_to(page, frame, flags_2mb, frame_allocator)
                    .map_err(|_| "map_2mb failed")?
                    .flush();
            }
        }
        Ok(())
    }

    /// Flush the TLB for the entire system.
    pub fn flush_tlb_all(&self) {
        x86_64::instructions::tlb::flush_all();
    }
}

// NOTE: `flush_tlb_and_verify` is defined in `page_table::raw::utils`.
// Do NOT redefine it here.
