//! DriverContext — callback trait for memory allocation and MMIO mapping.
//!
//! Nitrogen drivers that need DMA buffers, MMIO BAR mapping, or physical↔virtual
//! address translation receive a `&dyn DriverContext` from the kernel (or any
//! higher-level crate that owns the memory manager and page tables).
//!
//! # Rationale
//!
//! Nitrogen is a pure hardware-mechanism layer and must not depend on
//! `petroleum` or `fullerene-kernel`.  Instead of calling
//! `petroleum::common::memory::physical_to_virtual()` directly, drivers go
//! through this trait so the kernel retains ownership of the allocator and
//! address-space layout.
//!
//! # Example
//!
//! ```ignore
//! // Kernel side:
//! struct KernelDriverContext;
//! impl DriverContext for KernelDriverContext { … }
//!
//! // Driver side:
//! pub fn init(ctx: &dyn DriverContext, dev: PciDevice) -> Option<Self> {
//!     let virt = ctx.phys_to_virt(bar_phys);
//!     ctx.map_mmio(bar_phys, virt, bar_size)?;
//!     let frame = ctx.allocate_frame()?;
//!     …
//! }
//! ```
use core::fmt;

/// Error type for driver context operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverContextError {
    /// The requested memory allocation could not be satisfied.
    OutOfMemory,
    /// The MMIO region could not be mapped (e.g. address conflict).
    MmioMappingFailed,
    /// An invalid (null or misaligned) argument was supplied.
    InvalidArgument,
}

impl fmt::Display for DriverContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::MmioMappingFailed => f.write_str("MMIO mapping failed"),
            Self::InvalidArgument => f.write_str("invalid argument"),
        }
    }
}

/// Services that a driver needs from the owning kernel / runtime.
///
/// All methods are fallible — drivers must handle allocation or mapping
/// failures gracefully, typically by returning `None` from their `init()`.
pub trait DriverContext {
    /// Convert a physical address to a kernel-accessible virtual address.
    ///
    /// In a higher-half kernel this is typically `phys + offset`.
    fn phys_to_virt(&self, phys: u64) -> usize;

    /// Allocate a single physical 4 KiB frame.
    ///
    /// Returns the **physical** address of the frame.
    fn allocate_frame(&self) -> Result<u64, DriverContextError>;

    /// Allocate `count` contiguous physical 4 KiB frames.
    ///
    /// Returns the **physical** address of the first frame.
    fn allocate_contiguous_frames(
        &self,
        count: usize,
    ) -> Result<u64, DriverContextError>;

    /// Map a physical MMIO region into the kernel's virtual address space.
    ///
    /// `phys` and `virt` must be page-aligned.  `size` is in bytes.
    fn map_mmio_region(
        &self,
        phys: usize,
        virt: usize,
        size: usize,
    ) -> Result<(), DriverContextError>;

    /// Map a single page with the given flags.
    ///
    /// Used for framebuffer mapping (write-combining, etc.).
    fn map_page(
        &self,
        virt: usize,
        phys: usize,
        flags: PageFlags,
    ) -> Result<(), DriverContextError>;

    /// Free a single physical 4 KiB frame previously returned by
    /// [`allocate_frame`](Self::allocate_frame).
    ///
    /// `phys` must be the exact physical address returned by
    /// `allocate_frame`.  Behaviour is undefined if `phys` was not
    /// allocated through this trait or has already been freed.
    fn free_frame(&self, phys: u64);

    /// Free `count` contiguous physical 4 KiB frames previously returned by
    /// [`allocate_contiguous_frames`](Self::allocate_contiguous_frames).
    ///
    /// `phys` must be the exact physical address returned by
    /// `allocate_contiguous_frames`.  Behaviour is undefined if the region
    /// was not allocated through this trait or has already been freed.
    fn free_contiguous_frames(&self, phys: u64, count: usize);
}

/// Simplified page-table flags for driver mapping requests.
///
/// Drivers don't need to know the exact x86 page-table bit layout;
/// they specify semantics through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    /// Page is writable.
    pub writable: bool,
    /// Page uses write-combining caching (WC) instead of write-back.
    pub write_combining: bool,
    /// Page is executable.
    pub executable: bool,
}

impl PageFlags {
    /// Standard uncacheable MMIO.
    pub const MMIO: Self = Self {
        writable: true,
        write_combining: false,
        executable: false,
    };

    /// Write-combining framebuffer.
    pub const FRAMEBUFFER_WC: Self = Self {
        writable: true,
        write_combining: true,
        executable: false,
    };
}