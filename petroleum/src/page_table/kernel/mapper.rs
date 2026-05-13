//! High-level declarative page mapper with Builder pattern.
//!
//! This module provides a concise, safe API for mapping regions of memory:
//!
//! ```ignore
//! mapper
//!     .map_region(virt, phys, Size::MiB(128))
//!     .with_flags(Flags::KERNEL_DATA)
//!     .huge_if_possible()
//!     .apply()?;
//! ```

use crate::page_table::types::*;
use crate::page_table::raw::walker::{walk_or_create, FrameAlloc, WalkError};
use crate::page_table::PageTableEntry;
use crate::page_table::allocator::traits::FrameAllocator;

/// Mapping errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// The virtual address is not canonical.
    InvalidAddress,
    /// The physical address is not properly aligned.
    InvalidAlignment,
    /// Frame allocation failed.
    OutOfMemory,
    /// A huge page conflict was encountered.
    HugePageConflict { level: u8 },
    /// The size is zero.
    ZeroSize,
}

impl core::fmt::Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MapError::InvalidAddress => write!(f, "invalid virtual address"),
            MapError::InvalidAlignment => write!(f, "invalid alignment"),
            MapError::OutOfMemory => write!(f, "out of memory"),
            MapError::HugePageConflict { level } => {
                write!(f, "huge page conflict at level {}", level)
            }
            MapError::ZeroSize => write!(f, "zero size"),
        }
    }
}

impl From<WalkError> for MapError {
    fn from(e: WalkError) -> Self {
        match e {
            WalkError::OutOfMemory => MapError::OutOfMemory,
            WalkError::HugePageConflict { level } => MapError::HugePageConflict { level },
            WalkError::InvalidEntry { .. } => MapError::InvalidAddress,
        }
    }
}

/// The declarative mapper.
///
/// Wraps a page table root and frame allocator, providing a builder API
/// for mapping memory regions.
pub struct Mapper<'a, A: FrameAllocator> {
    root: &'a mut PageTable,
    allocator: &'a mut A,
}

impl<'a, A: FrameAllocator> Mapper<'a, A> {
    /// Create a new mapper.
    pub fn new(root: &'a mut PageTable, allocator: &'a mut A) -> Self {
        Self { root, allocator }
    }

    /// Start building a region mapping.
    ///
    /// Returns a `RegionBuilder` for fluent configuration.
    pub fn map_region(
        &mut self,
        virt: CanonicalVirtAddr,
        phys: u64,
        size: u64,
    ) -> RegionBuilder<'_, 'a, A> {
        RegionBuilder {
            mapper: self,
            virt,
            phys,
            size,
            flags: Flags::PRESENT | Flags::WRITABLE,
            prefer_huge: false,
        }
    }

    /// Map a single 4 KiB page.
    pub fn map_4k(
        &mut self,
        virt: CanonicalVirtAddr,
        frame: PhysFrame,
        flags: u64,
    ) -> Result<(), MapError> {
        let adapter = &mut WalkerAdapter(self.allocator);
        let entry = walk_or_create(self.root, virt, adapter, 1)?;
        *entry = PageTableEntry::new_with_frame(frame, flags);
        Ok(())
    }

    /// Map a single 2 MiB huge page.
    ///
    /// # Panics
    /// Panics in debug mode if `virt` or `phys` are not 2 MiB-aligned.
    pub fn map_2m(
        &mut self,
        virt: CanonicalVirtAddr,
        phys: u64,
        flags: u64,
    ) -> Result<(), MapError> {
        debug_assert!(virt.is_aligned(SIZE_2M), "virt not 2M-aligned");
        debug_assert!(phys % SIZE_2M == 0, "phys not 2M-aligned");

        let adapter = &mut WalkerAdapter(self.allocator);
        let entry = walk_or_create(self.root, virt, adapter, 2)?;
        *entry = PageTableEntry::new(phys | flags | Flags::HUGE_PAGE);
        Ok(())
    }

    /// Map a single 1 GiB huge page.
    ///
    /// # Panics
    /// Panics in debug mode if `virt` or `phys` are not 1 GiB-aligned.
    pub fn map_1g(
        &mut self,
        virt: CanonicalVirtAddr,
        phys: u64,
        flags: u64,
    ) -> Result<(), MapError> {
        debug_assert!(virt.is_aligned(SIZE_1G), "virt not 1G-aligned");
        debug_assert!(phys % SIZE_1G == 0, "phys not 1G-aligned");

        let adapter = &mut WalkerAdapter(self.allocator);
        let entry = walk_or_create(self.root, virt, adapter, 3)?;
        *entry = PageTableEntry::new(phys | flags | Flags::HUGE_PAGE);
        Ok(())
    }
}

/// Adapter to convert FrameAllocator into the walker's FrameAlloc.
struct WalkerAdapter<'a, A: FrameAllocator>(&'a mut A);

impl<'a, A: FrameAllocator> FrameAlloc for WalkerAdapter<'a, A> {
    fn alloc_zeroed(&mut self) -> Option<u64> {
        self.0.allocate().ok().map(|f| f.start_address())
    }
}

/// Builder for mapping a region of memory.
///
/// Created by `Mapper::map_region()`. Configure with builder methods,
/// then call `apply()` to execute the mapping.
pub struct RegionBuilder<'m, 'a, A: FrameAllocator> {
    mapper: &'m mut Mapper<'a, A>,
    virt: CanonicalVirtAddr,
    phys: u64,
    size: u64,
    flags: u64,
    prefer_huge: bool,
}

impl<'m, 'a, A: FrameAllocator> RegionBuilder<'m, 'a, A> {
    /// Set the page table entry flags.
    pub fn with_flags(mut self, flags: u64) -> Self {
        self.flags = flags;
        self
    }

    /// Prefer huge pages when alignment permits.
    ///
    /// The mapper will automatically use 1 GiB or 2 MiB pages where
    /// both virtual and physical addresses are sufficiently aligned.
    pub fn huge_if_possible(mut self) -> Self {
        self.prefer_huge = true;
        self
    }

    /// Execute the mapping.
    pub fn apply(self) -> Result<(), MapError> {
        if self.size == 0 {
            return Err(MapError::ZeroSize);
        }

        let mut virt = self.virt.as_u64();
        let mut phys = self.phys;
        let mut remaining = self.size;

        if self.prefer_huge {
            // Use largest possible page sizes
            while remaining > 0 {
                let page_size = best_page_size(virt, phys, remaining);

                match page_size {
                    SIZE_1G => {
                        self.mapper
                            .map_1g(CanonicalVirtAddr::new(virt).unwrap(), phys, self.flags)?;
                        virt += SIZE_1G;
                        phys += SIZE_1G;
                        remaining -= SIZE_1G;
                    }
                    SIZE_2M => {
                        self.mapper
                            .map_2m(CanonicalVirtAddr::new(virt).unwrap(), phys, self.flags)?;
                        virt += SIZE_2M;
                        phys += SIZE_2M;
                        remaining -= SIZE_2M;
                    }
                    _ => {
                        let frame = PhysFrame::from_start_address(phys)
                            .ok_or(MapError::InvalidAlignment)?;
                        self.mapper.map_4k(CanonicalVirtAddr::new(virt).unwrap(), frame, self.flags)?;
                        virt += SIZE_4K;
                        phys += SIZE_4K;
                        remaining -= SIZE_4K;
                    }
                }
            }
        } else {
            // Always use 4 KiB pages
            while remaining > 0 {
                let frame = PhysFrame::from_start_address(phys)
                    .ok_or(MapError::InvalidAlignment)?;
                self.mapper.map_4k(CanonicalVirtAddr::new(virt).unwrap(), frame, self.flags)?;
                virt += SIZE_4K;
                phys += SIZE_4K;
                remaining = remaining.saturating_sub(SIZE_4K);
            }
        }

        Ok(())
    }
}

/// Unmap a single page at the given virtual address.
///
/// Returns the physical frame that was mapped, if any.
pub fn unmap_page<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    allocator: &mut A,
) -> Result<Option<PhysFrame>, WalkError> {
    use crate::page_table::raw::walker::walk;

    let entry = walk(root, virt, 1)?;

    if !entry.is_present() {
        return Ok(None);
    }

    let frame = PhysFrame::from_start_address(entry.addr()).unwrap();
    entry.clear();
    allocator.deallocate(frame);

    Ok(Some(frame))
}

/// Unmap a range of pages.
pub fn unmap_range<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    size: u64,
    allocator: &mut A,
) -> Result<u64, WalkError> {
    let mut unmapped: u64 = 0;
    let mut addr = virt.as_u64();

    for _ in 0..(size / SIZE_4K) {
        if let Some(_frame) = unmap_page(root, unsafe { CanonicalVirtAddr::new_unchecked(addr) }, allocator)? {
            unmapped += 1;
        }
        addr += SIZE_4K;
    }

    Ok(unmapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_table::allocator::bitmap::BitmapFrameAllocator;

    #[test]
    fn map_single_4k() {
        let mut root = PageTable::new();
        let _storage = &mut [0u8; 65536];
        let mut alloc = BitmapFrameAllocator::new(1024);

        // Allocate frame before creating mapper (which borrows alloc)
        let frame = alloc.allocate().unwrap();
        let virt = CanonicalVirtAddr::new(0x1000).unwrap();

        let mut mapper = Mapper::new(&mut root, &mut alloc);
        mapper.map_4k(virt, frame, Flags::PRESENT | Flags::WRITABLE)
            .unwrap();

        // Verify the entry
        let p4_idx = virt.p4_index();
        assert!(root[p4_idx].is_present());
    }
}