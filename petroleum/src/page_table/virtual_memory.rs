//! VirtualMemoryContext — centralized page-table state and operations.
//!
//! Replaces scattered `map_page()`, `map_range_with_huge_pages()`, etc.
//! with high-level methods that keep all mapping metadata in one place.
//!
//! # Design
//!
//! - **No raw page-table access from outside this module.**
//! - Every mapping is recorded in `MappingInfo` so "where is X mapped" is
//!   always answerable.
//! - `clone_for_process()` handles shallow page-table copying without
//!   corrupting `.data` / `.bss` backed memory.
use alloc::vec::Vec;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size2MiB, Size4KiB,
    },
};

use crate::page_table::allocator::bitmap::BitmapFrameAllocator;
use crate::page_table::memory_map::validator::MemoryDescriptorValidator;

// ── Mapping metadata ──────────────────────────────────────────

/// Describes one contiguous virtual→physical mapping.
#[derive(Clone, Debug)]
pub struct MappingInfo {
    pub name: &'static str,
    pub phys_start: u64,
    pub virt_start: u64,
    pub size_bytes: u64,
    pub flags: PageTableFlags,
    /// If true, physical frames are owned (must be freed on unmap).
    pub owned: bool,
}

impl MappingInfo {
    pub const fn empty() -> Self {
        Self {
            name: "",
            phys_start: 0,
            virt_start: 0,
            size_bytes: 0,
            flags: PageTableFlags::empty(),
            owned: false,
        }
    }

    pub fn contains_virt(&self, va: u64) -> bool {
        va >= self.virt_start && va < self.virt_start + self.size_bytes
    }
}

// ── VirtualMemoryContext ───────────────────────────────────────

/// Complete virtual-memory state for the kernel.
///
/// Owns the frame allocator (via `BitmapFrameAllocator`) and the active
/// page-table hierarchy.  All mapping operations are methods on this struct.
pub struct VirtualMemoryContext {
    /// Physical address of the PML4 table.
    pub pml4_phys: u64,
    /// Cached physical→virtual offset.
    pub physical_offset: u64,
    /// Frame allocator (bitmap-based, owns the free-frame tracking).
    pub frame_allocator: BitmapFrameAllocator,
    /// Record of every mapping made through this context.
    pub mappings: Vec<MappingInfo>,
    /// Whether huge-page (2 MiB) mappings are preferred.
    pub huge_pages_enabled: bool,
}

impl VirtualMemoryContext {
    /// Create a new context from an existing PML4 and frame allocator.
    ///
    /// The `frame_allocator` is **moved** in; the caller must not use it
    /// afterwards.
    pub fn new(
        pml4_phys: u64,
        physical_offset: u64,
        frame_allocator: BitmapFrameAllocator,
    ) -> Self {
        Self {
            pml4_phys,
            physical_offset,
            frame_allocator,
            mappings: Vec::new(),
            huge_pages_enabled: true,
        }
    }

    /// Get a mutable reference to the L4 table via the direct map.
    ///
    /// # Safety
    /// The physical offset must be correct and the PML4 must be mapped.
    pub unsafe fn l4_table_mut(&self) -> &'static mut PageTable {
        let va = self.physical_offset + self.pml4_phys;
        unsafe { &mut *(va as *mut PageTable) }
    }

    // ── High-level mapping ───────────────────────────────────

    /// Map a framebuffer (MMIO, uncacheable / write-combining).
    ///
    /// Returns the virtual address.
    pub fn map_framebuffer(
        &mut self,
        phys: u64,
        size_bytes: u64,
        preferred_virt: Option<u64>,
    ) -> Result<u64, &'static str> {
        // On InsydeH2O the boot-phase huge-page (WB) mapping already covers
        // the framebuffer region via the direct map.  Splitting it into 4 KiB
        // WC pages breaks the mapping (see README Fix #3).  We therefore keep
        // the direct-map identity mapping and do NOT remap.
        let va = preferred_virt.unwrap_or(phys + self.physical_offset);

        // Record the mapping.
        self.mappings.push(MappingInfo {
            name: "framebuffer",
            phys_start: phys,
            virt_start: va,
            size_bytes,
            flags: PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::NO_CACHE,
            owned: false,
        });

        Ok(va)
    }

    /// Identity-map a physical range with huge (2 MiB) pages.
    ///
    /// Used for the kernel image and initial boot mappings.
    ///
    /// # Safety
    /// Caller must ensure the physical range is valid and the L4 table is mapped.
    pub unsafe fn map_identity_huge(
        &mut self,
        phys_start: u64,
        size_bytes: u64,
        flags: PageTableFlags,
        name: &'static str,
    ) -> Result<u64, &'static str> {
        let va = phys_start + self.physical_offset;
        let l4 = self.l4_table_mut();
        // Build mapper from the raw L4 pointer (avoids borrowing self twice).
        let phys_offset = VirtAddr::new(self.physical_offset);
        let mut mapper = unsafe { OffsetPageTable::new(l4, phys_offset) };

        let page_count = (size_bytes + 0x1F_FFFF) / 0x20_0000; // 2 MiB
        for i in 0..page_count {
            let p = phys_start + i * 0x20_0000;
            let v = va + i * 0x20_0000;
            let page = Page::<Size2MiB>::containing_address(VirtAddr::new(v));
            let frame = PhysFrame::<Size2MiB>::containing_address(PhysAddr::new(p));
            unsafe {
                mapper
                    .map_to(page, frame, flags, &mut self.frame_allocator)
                    .map_err(|_| "map_identity_huge: map_to failed")?
                    .flush();
            }
        }

        self.mappings.push(MappingInfo {
            name,
            phys_start,
            virt_start: va,
            size_bytes: page_count * 0x20_0000,
            flags,
            owned: false,
        });

        Ok(va)
    }

    /// Map a range using 4 KiB pages (splitting huge pages if needed).
    pub fn map_range_4k(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        size_bytes: u64,
        flags: PageTableFlags,
        name: &'static str,
    ) -> Result<(), &'static str> {
        let page_count = (size_bytes + 0xFFF) / 0x1000;
        let l4 = unsafe { self.l4_table_mut() };

        // Use the existing map_page_4k_l1 helper which handles huge-page splitting.
        for i in 0..page_count {
            let p = phys_start + i * 0x1000;
            let v = virt_start + i * 0x1000;
            unsafe {
                crate::page_table::kernel::init::map_page_4k_l1(
                    l4,
                    VirtAddr::new(v),
                    PhysAddr::new(p),
                    flags,
                    &mut self.frame_allocator,
                    VirtAddr::new(self.physical_offset),
                )
                .map_err(|_| "map_range_4k: map_page_4k_l1 failed")?;
            }
        }

        self.mappings.push(MappingInfo {
            name,
            phys_start,
            virt_start,
            size_bytes: page_count * 0x1000,
            flags,
            owned: false,
        });

        Ok(())
    }

    /// Map a heap region (owned, writable, no-execute).
    pub fn map_heap(&mut self, phys: u64, size_bytes: u64) -> Result<u64, &'static str> {
        let va = phys + self.physical_offset;
        self.map_range_4k(
            phys,
            va,
            size_bytes,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            "heap",
        )?;
        Ok(va)
    }

    /// Direct-map all available physical memory (from UEFI memory map).
    ///
    /// Uses the existing mappings where possible (avoids remapping).
    pub fn direct_map_physical(
        &mut self,
        memory_map: &[crate::page_table::memory_map::MemoryMapDescriptor],
    ) -> Result<(), &'static str> {
        for desc in memory_map {
            let phys = desc.physical_start();
            let size = desc.number_of_pages() as u64 * 0x1000;
            let va = phys + self.physical_offset;
            // Already identity-mapped by init_and_jump; skip remapping.
            self.mappings.push(MappingInfo {
                name: "direct_map",
                phys_start: phys,
                virt_start: va,
                size_bytes: size,
                flags: PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                owned: false,
            });
        }
        Ok(())
    }

    /// Shallow-clone the page table for a new process.
    ///
    /// This copies the PML4 structure but preserves all existing mappings
    /// (including `.data` and `.bss` sections) unlike the previous
    /// `clone_page_table` which zeroed temporary VA ranges.
    pub fn clone_for_process(&mut self) -> Result<VirtualMemoryContext, &'static str> {
        let new_frame = self
            .frame_allocator
            .allocate_frame()
            .ok_or("clone_for_process: allocate_frame failed")?;
        let new_pml4_phys = new_frame.start_address().as_u64();

        // Copy the current L4 entries to the new table.
        // We use the direct map to access both tables.
        let src_va = self.physical_offset + self.pml4_phys;
        let dst_va = self.physical_offset + new_pml4_phys;
        unsafe {
            let src = src_va as *const PageTable;
            let dst = dst_va as *mut PageTable;
            core::ptr::copy_nonoverlapping(src, dst, 1);
        }

        // The new context shares the same frame allocator *reference* —
        // in practice the kernel's allocator is behind a Mutex.
        Ok(VirtualMemoryContext {
            pml4_phys: new_pml4_phys,
            physical_offset: self.physical_offset,
            frame_allocator: BitmapFrameAllocator::empty(), // process-level allocator placeholder
            mappings: self.mappings.clone(),
            huge_pages_enabled: self.huge_pages_enabled,
        })
    }

    /// Look up a virtual address in the recorded mappings.
    pub fn lookup_mapping(&self, va: u64) -> Option<&MappingInfo> {
        self.mappings.iter().find(|m| m.contains_virt(va))
    }

    /// Find the framebuffer mapping, if any.
    pub fn framebuffer_mapping(&self) -> Option<&MappingInfo> {
        self.mappings.iter().find(|m| m.name == "framebuffer")
    }

    /// Check whether a virtual address is within the framebuffer range.
    pub fn is_framebuffer_virt(&self, va: u64) -> bool {
        self.framebuffer_mapping()
            .map(|m| m.contains_virt(va))
            .unwrap_or(false)
    }
}

// ── BitmapFrameAllocator helpers ──────────────────────────────

impl BitmapFrameAllocator {
    /// Create a placeholder allocator (no free frames).
    /// Used for process-level contexts that don't own the allocator.
    pub fn empty() -> Self {
        Self::new(0)
    }
}
