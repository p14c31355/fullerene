//! Framebuffer / MMIO mapping abstraction.
//!
//! Graphics subsystems must NOT directly manipulate page tables or cache
//! attributes.  They declare *what* physical memory they need and let the
//! memory manager decide *how* to map it.
//!
//! # Design
//!
//! ```text
//! Graphics / Driver                Memory Manager
//! ─────────────────                ──────────────
//! map(phys, size, Uncached)   →    choose virtual range
//!                                  split huge pages (if needed)
//!                                  set PAT / PCD / PWT
//!                                  return VirtAddr
//! ```
//!
//! The returned virtual address is opaque to the caller — the caller
//! only cares that reads/writes are safe and cache behaviour is correct.

/// Cache mode for a mapped region.
///
/// Corresponds to x86 PAT / PCD / PWT combinations:
///
/// | Mode           | PAT | PCD | PWT | x86 type |
/// |----------------|-----|-----|-----|----------|
/// | `Uncached`     | UC  |   1 |   – | UC       |
/// | `WriteCombining` | WC |   – |   – | WC       |
/// | `WriteBack`    | WB  |   0 |   0 | WB       |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Strong uncacheable — reads always go to device, writes are not
    /// buffered.  Appropriate for MMIO control registers.
    Uncached,
    /// Write-combining — writes are buffered and combined, reads are
    /// weakly ordered.  Appropriate for framebuffer scan-out memory.
    WriteCombining,
    /// Write-back — standard caching.  Appropriate for normal RAM.
    WriteBack,
}

/// Trait for mapping framebuffer / MMIO memory into virtual address space.
///
/// # Contract
///
/// - `map_framebuffer` allocates a virtual address range, maps the given
///   physical pages with the requested [`CacheMode`], and returns the
///   **virtual** start address on success.
/// - `unmap_framebuffer` reverses the mapping and frees the virtual range.
/// - Implementations decide whether to use 4 KiB pages, 2 MiB huge pages,
///   or whether huge-page splitting is needed.
/// - The caller must NOT assume any particular virtual address layout.
pub trait FramebufferMapper {
    /// Map a physical region with the given cache mode.
    ///
    /// Returns `Some(virt_addr)` on success, or `None` if the mapping
    /// could not be created (OOM, address conflict, etc.).
    fn map_framebuffer(&mut self, phys_addr: u64, size: usize, cache: CacheMode) -> Option<u64>;

    /// Unmap a region previously returned by [`map_framebuffer`].
    ///
    /// `size` must match the value originally passed to `map_framebuffer`.
    fn unmap_framebuffer(&mut self, virt_addr: u64, size: usize);
}
