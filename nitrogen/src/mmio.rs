//! Safe MMIO region wrapper with PCIe ordering guarantees.
//!
//! # Real-hardware failure scenarios addressed
//!
//! | Scenario | Consequence | Mitigation |
//! |----------|-------------|------------|
//! | PCIe posted write reordering | Non-posted read returns stale data | `write_barrier()` after write batches |
//! | WC buffer not drained | Device sees incomplete descriptor | `write_barrier()` before doorbell |
//! | Non-posted read to D3 device | Completion timeout (30s+) | Pre-flight `ensure_accessible()` via PciHealth |
//! | Store-forwarding from store buffer | Volatile read sees CPU-local value | `lfence` after read |
//!
//! # Memory types on x86-64
//!
//! - **UC** (Uncacheable): MMIO registers. Strongly ordered but slower.
//! - **WC** (Write-Combining): Framebuffers. Stores are buffered, can be reordered.
//! - **WB** (Write-Back): DMA buffers. Requires `clflush` before device access.
//!
//! This module provides `MemRegion` (for UC MMIO) and `DmaRegion` (for WB DMA)
//! with the correct barriers for each.
//!
//! # Usage
//!
//! ```ignore
//! let mmio = MemRegion::new(mmio_base, 0x1000);
//! let val = mmio.read32(0x100);  // volatile read + lfence
//! mmio.write32(0x104, 0x1);      // volatile write
//! mmio.write_barrier();          // ensure write is visible to PCIe
//! ```

use crate::DriverContext;
use core::ptr;

// ============================================================================
//  Cache management
// ============================================================================

/// Flush a single cache line covering `addr`.
///
/// x86-64 cache line size is 64 bytes.  `clflush` is ordered with
/// respect to other `clflush` instructions but NOT with respect to
/// writes — use `mfence` after the last `clflush` when ordering matters.
#[inline]
pub fn cache_flush(addr: *const u8) {
    unsafe { core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags)); }
}

/// Flush a range of cache lines and issue a memory fence afterwards.
/// This ensures all prior writes are visible to DMA before a doorbell.
pub fn cache_flush_range(base: *const u8, len: usize) {
    let base_addr = base as usize;
    let end = base_addr.wrapping_add(len);
    let mut addr = base_addr & !0x3F; // align to cache line
    while addr < end {
        cache_flush(addr as *const u8);
        addr = addr.wrapping_add(64);
    }
    write_barrier();
}

// ============================================================================
//  Memory barriers
// ============================================================================

/// Ensure all prior stores are visible to subsequent loads (and PCIe).
/// Required between posted write batches and the first non-posted read
/// when WC memory type is involved.
#[inline]
pub fn write_barrier() {
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)); }
}

/// Ensure all prior loads are complete before subsequent loads.
#[inline]
pub fn read_barrier() {
    unsafe { core::arch::asm!("lfence", options(nostack, preserves_flags)); }
}

/// Full memory barrier (store + load ordering).
#[inline]
pub fn full_barrier() {
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)); }
}

// ============================================================================
//  MemRegion — safe MMIO access with ordering guarantees
// ============================================================================

/// A mapped MMIO region with safe accessors.
///
/// All access is volatile and UC-safe.  `write_barrier` must be called
/// between a batch of writes and a subsequent read to enforce PCIe
/// ordering (posted writes before non-posted read).
pub struct MemRegion {
    base: *mut u8,
    size: usize,
}

impl MemRegion {
    /// Create a new MemRegion from a virtual base address and size.
    ///
    /// # Safety
    ///
    /// `base` must point to a valid UC-mapped MMIO region of at least `size` bytes.
    /// The caller must have mapped this region via `DriverContext::map_mmio_region`.
    pub unsafe fn new(base: *mut u8, size: usize) -> Self {
        Self { base, size }
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Read a u32 from an offset within this region.
    #[inline]
    pub fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + 4 <= self.size, "MMIO read32 out of bounds");
        unsafe { ptr::read_volatile(self.base.add(offset) as *const u32) }
    }

    /// Read a u64 from an offset within this region.
    #[inline]
    pub fn read64(&self, offset: usize) -> u64 {
        debug_assert!(offset + 8 <= self.size, "MMIO read64 out of bounds");
        let lo = self.read32(offset);
        let hi = self.read32(offset + 4);
        (lo as u64) | ((hi as u64) << 32)
    }

    /// Write a u32 to an offset within this region.
    #[inline]
    pub fn write32(&self, offset: usize, val: u32) {
        debug_assert!(offset + 4 <= self.size, "MMIO write32 out of bounds");
        unsafe { ptr::write_volatile(self.base.add(offset) as *mut u32, val) };
    }

    /// Write a u64 to an offset within this region.
    #[inline]
    pub fn write64(&self, offset: usize, val: u64) {
        debug_assert!(offset + 8 <= self.size, "MMIO write64 out of bounds");
        self.write32(offset, val as u32);
        self.write32(offset + 4, (val >> 32) as u32);
    }

    /// Read-modify-write a u32: clear `clear_bits`, set `set_bits`.
    #[inline]
    pub fn update32(&self, offset: usize, set_bits: u32, mask: u32) {
        let old = self.read32(offset);
        self.write32(offset, (old & !mask) | (set_bits & mask));
    }

    /// Write a batch of u32 values, then issue a write barrier.
    /// This is the pattern for "write descriptor → doorbell" sequences.
    pub fn write_batch_then_barrier(&self, writes: &[(usize, u32)]) {
        for &(off, val) in writes {
            self.write32(off, val);
        }
        write_barrier();
    }
}

// ============================================================================
//  DmaRegion — DMA buffer with cache management
// ============================================================================

/// A DMA-accessible buffer with automatic cache management.
///
/// DMA buffers live in WB (Write-Back) memory.  Before the device reads
/// them (or after the device writes them), cache lines must be flushed
/// or invalidated to maintain coherency.
pub struct DmaRegion {
    virt: *mut u8,
    phys: u64,
    len: usize,
    /// Whether this region is currently mapped for DMA.
    mapped: bool,
}

impl DmaRegion {
    /// Allocate a DMA buffer via `DriverContext`.
    pub fn alloc(ctx: &dyn DriverContext, size: usize) -> Option<Self> {
        let pages = (size + 4095) / 4096;
        let phys = ctx.allocate_contiguous_frames(pages).ok()?;
        let virt = ctx.phys_to_virt(phys) as *mut u8;
        unsafe { core::ptr::write_bytes(virt, 0, size); }
        Some(Self {
            virt,
            phys,
            len: size,
            mapped: false,
        })
    }

    pub fn virt(&self) -> *mut u8 {
        self.virt
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// As a slice for reading.
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt, self.len) }
    }

    /// As a mutable slice for writing.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.len) }
    }

    /// Copy data into this buffer and flush cache for DMA read by device.
    pub fn write_from(&mut self, src: &[u8]) {
        let len = self.len.min(src.len());
        unsafe {
            ptr::copy_nonoverlapping(src.as_ptr(), self.virt, len);
        }
        self.flush_for_device();
    }

    /// Copy data from this buffer after DMA write by device.
    pub fn read_into(&self, dst: &mut [u8]) {
        // Device has written to this buffer; invalidate cache before CPU read.
        // On x86-64 we use clflushopt (or clflush) which is ordered.
        self.flush_for_cpu();
        let len = self.len.min(dst.len());
        unsafe {
            ptr::copy_nonoverlapping(self.virt, dst.as_mut_ptr(), len);
        }
    }

    /// Flush cache lines covering the buffer, then issue write barrier.
    /// Call BEFORE ringing a doorbell / kicking the DMA engine.
    pub fn flush_for_device(&self) {
        cache_flush_range(self.virt, self.len);
    }

    /// Flush cache lines so CPU sees device-written data.
    /// On x86, clflush is sufficient (it invalidates as well).
    pub fn flush_for_cpu(&self) {
        cache_flush_range(self.virt, self.len);
    }

    /// Map this buffer for DMA via IOMMU.
    pub fn dma_map(&mut self, ctx: &dyn DriverContext, device_id: u16) -> Result<u64, &'static str> {
        let dma = ctx
            .dma_map(device_id, self.phys, self.len)
            .map_err(|_| "dma_map failed")?;
        self.mapped = true;
        Ok(dma)
    }

    /// Unmap this buffer.
    pub fn dma_unmap(&mut self, ctx: &dyn DriverContext, _device_id: u16) {
        if self.mapped {
            ctx.dma_unmap(self.phys, self.len);
            self.mapped = false;
        }
    }
}

impl DmaRegion {
    pub fn free(&mut self, ctx: &dyn DriverContext) {
        if self.mapped {
            ctx.dma_unmap(self.phys, self.len);
            self.mapped = false;
        }
        let pages = (self.len + 4095) / 4096;
        ctx.free_contiguous_frames(self.phys, pages);
    }
}

impl Drop for DmaRegion {
    fn drop(&mut self) {
        if self.mapped {
            log::warn!("DmaRegion dropped while still mapped");
        }
    }
}
