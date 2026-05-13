//! Page table types with safety guarantees.
//!
//! This module provides:
//! - `CanonicalVirtAddr`: Type-safe virtual address that is always canonical
//! - `PageIndex`: Trait for page table index calculations
//! - `PageTableEntry`: Raw entry type with flag operations
//! - `PageTable`: The 512-entry table type

use core::fmt;

// ── Re-exports for backward compatibility ─────────────────────────────

/// Re-export of `MemoryDescriptorValidator` from `memory_map`.
pub use crate::page_table::memory_map::MemoryDescriptorValidator;

// x86_64 types used by PageTableHelper
use x86_64::structures::paging::PageTableFlags;

// ── PageTableHelper Trait ──────────────────────────────────────────────

/// Trait providing high-level page table operations.
///
/// This trait is implemented by page table managers (e.g., `KernelMapper`,
/// `ProcessPageTable`, `UnifiedMemoryManager`) to provide a unified interface
/// for mapping, unmapping, translating, and managing page tables.
pub trait PageTableHelper {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: x86_64::structures::paging::PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    ) -> crate::common::logging::SystemResult<()>;
    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<x86_64::structures::paging::PhysFrame>;
    fn translate_address(&self, virtual_addr: usize)
        -> crate::common::logging::SystemResult<usize>;
    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> crate::common::logging::SystemResult<()>;
    fn get_page_flags(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<x86_64::structures::paging::PageTableFlags>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()>;
    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn destroy_page_table(
        &mut self,
        table_addr: usize,
        frame_allocator: &mut crate::page_table::constants::BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()>;
    fn clone_page_table(
        &mut self,
        source_table: usize,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

// ── Constants ──────────────────────────────────────────────────────────

/// Size of a 4 KiB page.
pub const SIZE_4K: u64 = 4096;
/// Size of a 2 MiB page.
pub const SIZE_2M: u64 = 512 * SIZE_4K;
/// Size of a 1 GiB page.
pub const SIZE_1G: u64 = 512 * SIZE_2M;

/// Number of entries per page table level.
pub const ENTRIES_PER_TABLE: usize = 512;

/// Bit width of each page table level index.
pub const LEVEL_SHIFT: u8 = 9;

// ── Page Size Marker Types ─────────────────────────────────────────────

/// Marker type for 4 KiB pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size4KiB {}
/// Marker type for 2 MiB pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size2MiB {}
/// Marker type for 1 GiB pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size1GiB {}

/// Trait for page size types, providing the size in bytes.
pub trait PageSize: sealed::Sealed {
    /// The size in bytes.
    const SIZE: u64;
}

impl PageSize for Size4KiB {
    const SIZE: u64 = SIZE_4K;
}
impl PageSize for Size2MiB {
    const SIZE: u64 = SIZE_2M;
}
impl PageSize for Size1GiB {
    const SIZE: u64 = SIZE_1G;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Size4KiB {}
    impl Sealed for super::Size2MiB {}
    impl Sealed for super::Size1GiB {}
}

// ── CanonicalVirtAddr ──────────────────────────────────────────────────

/// A type-safe virtual address guaranteed to be canonical on x86_64.
///
/// A canonical address has bits 47..63 all equal to bit 47 (sign extension).
/// Non-canonical addresses will cause a #GP if used.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalVirtAddr(u64);

impl CanonicalVirtAddr {
    /// The null address.
    pub const ZERO: Self = Self(0);

    /// Try to create a `CanonicalVirtAddr` from a raw `u64`.
    ///
    /// Returns `None` if the address is not canonical.
    #[inline]
    pub const fn new(addr: u64) -> Option<Self> {
        // Check sign extension: bits 47..63 must all equal bit 47
        let sign = (addr as i64) >> 47;
        if sign == 0 || sign == -1 {
            Some(Self(addr))
        } else {
            None
        }
    }

    /// Create a `CanonicalVirtAddr` without checking.
    ///
    /// # Safety
    /// The caller must ensure the address is canonical.
    #[inline]
    pub const unsafe fn new_unchecked(addr: u64) -> Self {
        Self(addr)
    }

    /// Get the raw `u64` value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Get the address as a raw pointer.
    #[inline]
    pub const fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    /// Get the address as a mutable raw pointer.
    #[inline]
    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    /// Align down to the given alignment (must be a power of 2).
    #[inline]
    pub const fn align_down(self, align: u64) -> Self {
        Self(self.0 & !(align - 1))
    }

    /// Align up to the given alignment (must be a power of 2).
    #[inline]
    pub const fn align_up(self, align: u64) -> Self {
        Self((self.0 + align - 1) & !(align - 1))
    }

    /// Check if the address is aligned to the given alignment.
    #[inline]
    pub const fn is_aligned(self, align: u64) -> bool {
        self.0 & (align - 1) == 0
    }

    /// Try to add an offset, returning `None` if the result is non-canonical or overflows.
    #[inline]
    pub const fn try_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(addr) => Self::new(addr),
            None => None,
        }
    }

    /// Try to subtract an offset, returning `None` if the result is non-canonical or underflows.
    #[inline]
    pub const fn try_sub(self, offset: u64) -> Option<Self> {
        match self.0.checked_sub(offset) {
            Some(addr) => Self::new(addr),
            None => None,
        }
    }

    /// Compute the offset from this address to another (self - other).
    #[inline]
    pub const fn offset_from(self, other: Self) -> i64 {
        self.0.wrapping_sub(other.0) as i64
    }
}

impl fmt::Debug for CanonicalVirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CanonicalVirtAddr(0x{:016x})", self.0)
    }
}

impl fmt::LowerHex for CanonicalVirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl fmt::UpperHex for CanonicalVirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016X}", self.0)
    }
}

// ── PageIndex Trait ────────────────────────────────────────────────────

/// Trait providing page table index calculations for a canonical virtual address.
///
/// This centralizes all index computation in one place, eliminating duplication
/// across p1_index, p2_index, p3_index, p4_index functions.
pub trait PageIndex {
    /// PML4 (level 4) index.
    fn p4_index(self) -> usize;
    /// PDPT (level 3) index.
    fn p3_index(self) -> usize;
    /// PD (level 2) index.
    fn p2_index(self) -> usize;
    /// PT (level 1) index.
    fn p1_index(self) -> usize;
    /// Get the index for a specific level (1-4).
    fn index(self, level: u8) -> usize;
    /// Page offset within a 4 KiB page.
    fn page_offset_4k(self) -> u16;
    /// Page offset within a 2 MiB page.
    fn page_offset_2m(self) -> u32;
    /// Page offset within a 1 GiB page.
    fn page_offset_1g(self) -> u32;
}

impl PageIndex for CanonicalVirtAddr {
    #[inline]
    fn p4_index(self) -> usize {
        ((self.0 >> 39) & 0x1FF) as usize
    }

    #[inline]
    fn p3_index(self) -> usize {
        ((self.0 >> 30) & 0x1FF) as usize
    }

    #[inline]
    fn p2_index(self) -> usize {
        ((self.0 >> 21) & 0x1FF) as usize
    }

    #[inline]
    fn p1_index(self) -> usize {
        ((self.0 >> 12) & 0x1FF) as usize
    }

    #[inline]
    fn index(self, level: u8) -> usize {
        assert!((1..=4).contains(&level), "page table level must be 1-4");
        ((self.0 >> (12 + (level - 1) * 9)) & 0x1FF) as usize
    }

    #[inline]
    fn page_offset_4k(self) -> u16 {
        (self.0 & 0xFFF) as u16
    }

    #[inline]
    fn page_offset_2m(self) -> u32 {
        (self.0 & 0x1F_FFFF) as u32
    }

    #[inline]
    fn page_offset_1g(self) -> u32 {
        (self.0 & 0x3FFF_FFFF) as u32
    }
}

// Also implement for raw u64 for backward compatibility
impl PageIndex for u64 {
    #[inline]
    fn p4_index(self) -> usize {
        ((self >> 39) & 0x1FF) as usize
    }
    #[inline]
    fn p3_index(self) -> usize {
        ((self >> 30) & 0x1FF) as usize
    }
    #[inline]
    fn p2_index(self) -> usize {
        ((self >> 21) & 0x1FF) as usize
    }
    #[inline]
    fn p1_index(self) -> usize {
        ((self >> 12) & 0x1FF) as usize
    }
    #[inline]
    fn index(self, level: u8) -> usize {
        assert!((1..=4).contains(&level), "page table level must be 1-4");
        ((self >> (12 + (level - 1) * 9)) & 0x1FF) as usize
    }
    #[inline]
    fn page_offset_4k(self) -> u16 {
        (self & 0xFFF) as u16
    }
    #[inline]
    fn page_offset_2m(self) -> u32 {
        (self & 0x1F_FFFF) as u32
    }
    #[inline]
    fn page_offset_1g(self) -> u32 {
        (self & 0x3FFF_FFFF) as u32
    }
}

// ── PhysFrame ──────────────────────────────────────────────────────────

/// A physical frame, identified by its starting physical address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysFrame {
    pub start_address: u64,
}

impl PhysFrame {
    /// Create a `PhysFrame` for a 4 KiB frame at the given physical address.
    ///
    /// # Safety
    /// The address must be 4 KiB-aligned and refer to a valid physical frame.
    #[inline]
    pub unsafe fn from_start_address_unchecked(addr: u64) -> Self {
        debug_assert!(addr % SIZE_4K == 0, "PhysFrame address must be 4K-aligned");
        Self { start_address: addr }
    }

    /// Create a `PhysFrame` for a 4 KiB frame, checking alignment.
    #[inline]
    pub fn from_start_address(addr: u64) -> Option<Self> {
        if addr % SIZE_4K == 0 {
            Some(Self { start_address: addr })
        } else {
            None
        }
    }

    /// Get the starting physical address of this frame.
    #[inline]
    pub fn start_address(self) -> u64 {
        self.start_address
    }

    /// Get the frame as a mutable pointer.
    #[inline]
    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.start_address as *mut T
    }
}

impl fmt::Debug for PhysFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysFrame(0x{:010x})", self.start_address)
    }
}

// ── PageTableEntry ─────────────────────────────────────────────────────

/// A raw 64-bit page table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry {
    entry: u64,
}

/// Flags for page table entries.
pub mod Flags {
    /// Present bit — entry is valid.
    pub const PRESENT: u64 = 1 << 0;
    /// Writable bit.
    pub const WRITABLE: u64 = 1 << 1;
    /// User-accessible bit.
    pub const USER_ACCESSIBLE: u64 = 1 << 2;
    /// Write-through caching.
    pub const WRITE_THROUGH: u64 = 1 << 3;
    /// Disable cache.
    pub const NO_CACHE: u64 = 1 << 4;
    /// Accessed bit (set by CPU).
    pub const ACCESSED: u64 = 1 << 5;
    /// Dirty bit (set by CPU, only at leaf level).
    pub const DIRTY: u64 = 1 << 6;
    /// Huge page bit (PDPT or PD level).
    pub const HUGE_PAGE: u64 = 1 << 7;
    /// Global bit (not flushed on CR3 write).
    pub const GLOBAL: u64 = 1 << 8;
    /// No-execute bit.
    pub const NO_EXECUTE: u64 = 1 << 63;

    /// Common flag combinations
    pub const KERNEL_DATA: u64 = PRESENT | WRITABLE | NO_EXECUTE;
    pub const KERNEL_CODE: u64 = PRESENT | NO_EXECUTE;
    pub const USER_DATA: u64 = PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE;
    pub const USER_CODE: u64 = PRESENT | USER_ACCESSIBLE;
    pub const DEVICE_MMIO: u64 = PRESENT | WRITABLE | NO_EXECUTE | NO_CACHE | WRITE_THROUGH;
}

impl PageTableEntry {
    /// Create a new entry from a raw value.
    #[inline]
    pub const fn new(entry: u64) -> Self {
        Self { entry }
    }

    /// Create a new entry pointing to a physical frame with given flags.
    #[inline]
    pub fn new_with_frame(frame: PhysFrame, flags: u64) -> Self {
        Self {
            entry: frame.start_address | flags,
        }
    }

    /// Get the raw entry value.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.entry
    }

    /// Check if the entry is unused (zero).
    #[inline]
    pub const fn is_unused(self) -> bool {
        self.entry == 0
    }

    /// Check if the entry is present.
    #[inline]
    pub const fn is_present(self) -> bool {
        self.entry & Flags::PRESENT != 0
    }

    /// Check if this is a huge page entry.
    #[inline]
    pub const fn is_huge(self) -> bool {
        self.entry & Flags::HUGE_PAGE != 0
    }

    /// Get the physical address from this entry.
    #[inline]
    pub fn addr(self) -> u64 {
        self.entry & 0x000F_FFFF_FFFF_F000
    }

    /// Get the flags from this entry.
    #[inline]
    pub fn flags(self) -> u64 {
        self.entry & !0x000F_FFFF_FFFF_F000
    }

    /// Set flags on this entry.
    #[inline]
    pub fn set_flags(&mut self, flags: u64) {
        self.entry = (self.entry & 0x000F_FFFF_FFFF_F000) | flags;
    }

    /// Clear the entry.
    #[inline]
    pub fn clear(&mut self) {
        self.entry = 0;
    }
}

impl fmt::Debug for PageTableEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PTE(0x{:016x} addr=0x{:010x} flags=0x{:x})",
            self.entry,
            self.addr(),
            self.flags()
        )
    }
}

// ── PageTable ──────────────────────────────────────────────────────────

/// A page table: 512 entries, 4 KiB aligned.
#[derive(Debug)]
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; ENTRIES_PER_TABLE],
}

impl PageTable {
    /// Create a new, zeroed page table.
    #[inline]
    pub const fn new() -> Self {
        Self {
            entries: [PageTableEntry::new(0); ENTRIES_PER_TABLE],
        }
    }

    /// Get a reference to an entry by index.
    #[inline]
    pub fn entry(&self, index: usize) -> &PageTableEntry {
        &self.entries[index]
    }

    /// Get a mutable reference to an entry by index.
    #[inline]
    pub fn entry_mut(&mut self, index: usize) -> &mut PageTableEntry {
        &mut self.entries[index]
    }

    /// Get the entries slice.
    #[inline]
    pub fn entries(&self) -> &[PageTableEntry; ENTRIES_PER_TABLE] {
        &self.entries
    }

    /// Get the mutable entries slice.
    #[inline]
    pub fn entries_mut(&mut self) -> &mut [PageTableEntry; ENTRIES_PER_TABLE] {
        &mut self.entries
    }

    /// Zero all entries.
    #[inline]
    pub fn zero(&mut self) {
        for entry in &mut self.entries {
            entry.clear();
        }
    }

    /// Count non-unused entries.
    #[inline]
    pub fn used_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_unused()).count()
    }

    /// Check if the table is completely empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| e.is_unused())
    }

    /// Get a mutable reference to this table for read-only walking.
    ///
    /// # Safety
    /// The caller must not modify any entries through this reference.
    /// This is used by the page table walker which only reads entries.
    #[inline]
    pub unsafe fn as_mut_for_walking(&self) -> &mut PageTable {
        // SAFETY: The caller guarantees no mutation occurs through this reference.
        // This is required by the walker API which needs &mut for interior mutability
        // of page table entries during mapping operations.
        let ptr = self as *const PageTable;
        let cell = &*(ptr as *const core::cell::UnsafeCell<PageTable>);
        &mut *cell.get()
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl core::ops::Index<usize> for PageTable {
    type Output = PageTableEntry;
    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl core::ops::IndexMut<usize> for PageTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

// ── Address alignment helpers ──────────────────────────────────────────

/// Align a value down to the given alignment (must be power of 2).
#[inline]
pub const fn align_down(val: u64, align: u64) -> u64 {
    val & !(align - 1)
}

/// Align a value up to the given alignment (must be power of 2).
#[inline]
pub const fn align_up(val: u64, align: u64) -> u64 {
    (val + align - 1) & !(align - 1)
}

/// Check if a value is aligned to the given alignment.
#[inline]
pub const fn is_aligned(val: u64, align: u64) -> bool {
    val & (align - 1) == 0
}

/// Determine the largest page size that can map `size` bytes starting at `virt`
/// with `phys` alignment, without splitting unnecessarily.
///
/// Returns the largest page size where both `virt` and `phys` are aligned
/// and the size covers at least that page size.
#[inline]
pub const fn best_page_size(virt: u64, phys: u64, size: u64) -> u64 {
    if size >= SIZE_1G && is_aligned(virt, SIZE_1G) && is_aligned(phys, SIZE_1G) {
        SIZE_1G
    } else if size >= SIZE_2M && is_aligned(virt, SIZE_2M) && is_aligned(phys, SIZE_2M) {
        SIZE_2M
    } else {
        SIZE_4K
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_zero() {
        assert!(CanonicalVirtAddr::new(0).is_some());
    }

    #[test]
    fn canonical_low_half() {
        // 0x0000_7FFF_FFFF_FFFF is the highest low-half canonical address
        assert!(CanonicalVirtAddr::new(0x0000_7FFF_FFFF_FFFF).is_some());
    }

    #[test]
    fn canonical_high_half() {
        // 0xFFFF_8000_0000_0000 is the lowest high-half canonical address
        assert!(CanonicalVirtAddr::new(0xFFFF_8000_0000_0000).is_some());
    }

    #[test]
    fn non_canonical() {
        // 0x0000_8000_0000_0000 is non-canonical (hole)
        assert!(CanonicalVirtAddr::new(0x0000_8000_0000_0000).is_none());
        // 0xFFFF_7FFF_FFFF_FFFF is also non-canonical
        assert!(CanonicalVirtAddr::new(0xFFFF_7FFF_FFFF_FFFF).is_none());
    }

    #[test]
    fn page_indices() {
        // Address: 0x0000_0000_0000_0000
        let addr = CanonicalVirtAddr::new(0).unwrap();
        assert_eq!(addr.p4_index(), 0);
        assert_eq!(addr.p3_index(), 0);
        assert_eq!(addr.p2_index(), 0);
        assert_eq!(addr.p1_index(), 0);

        // Address: 0x0000_0080_0000_0000 (bit 39 set → p4_index = 1)
        let addr = CanonicalVirtAddr::new(0x0000_0080_0000_0000).unwrap();
        assert_eq!(addr.p4_index(), 1);
    }

    #[test]
    fn align_helpers() {
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_down(7, 4), 4);
        assert_eq!(align_up(4096, 4096), 4096);
        assert!(is_aligned(4096, 4096));
        assert!(!is_aligned(4097, 4096));
    }

    #[test]
    fn best_page_size_4k() {
        assert_eq!(best_page_size(0, 0, 4096), SIZE_4K);
        assert_eq!(best_page_size(1, 0, 4096), SIZE_4K); // unaligned virt
        assert_eq!(best_page_size(0, 1, 4096), SIZE_4K); // unaligned phys
    }

    #[test]
    fn best_page_size_2m() {
        assert_eq!(best_page_size(0, 0, SIZE_2M), SIZE_2M);
        assert_eq!(best_page_size(0, 0, SIZE_2M + 1), SIZE_2M);
    }

    #[test]
    fn best_page_size_1g() {
        assert_eq!(best_page_size(0, 0, SIZE_1G), SIZE_1G);
    }

    #[test]
    fn entry_flags() {
        let entry = PageTableEntry::new(Flags::PRESENT | Flags::WRITABLE);
        assert!(entry.is_present());
        assert!(!entry.is_huge());
        assert!(!entry.is_unused());

        let unused = PageTableEntry::new(0);
        assert!(unused.is_unused());
        assert!(!unused.is_present());
    }
}