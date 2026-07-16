#![no_std]
//! # Nitrogen — Hardware Mechanism Layer
//!
//! Nitrogen is a standalone, `no_std` crate providing **pure hardware mechanism**
//! abstractions for x86-64 systems. It has zero dependency on the kernel or
//! petroleum boot crate. All device-driver-level code (Port I/O, PCI, APIC,
//! PIC, VirtIO, etc.) lives here; higher-level policy (memory management,
//! scheduling, graphics compositing) belongs in other crates.
//!
//! ## Design principle
//!
//! - **Hardware mechanism only** — raw register access, capability scanning,
//!   interrupt-controller programming, DMA setup. No memory allocator policy,
//!   no page-table logic, no process scheduling.
//! - **Fully isolated** — depends only on `x86_64`, `spin`, and `core`/`alloc`.
//!   No dependency on `petroleum`, `fullerene-kernel`, or any other workspace crate.
//! - **Callback-friendly** — where memory allocation or MMIO mapping is required
//!   (e.g. VirtIO queue setup), the caller provides pre‑allocated physical pages
//!   and virtual addresses. Nitrogen never owns the allocator.

extern crate alloc;

// Module declarations.
// Drivers listed in `.driverignore` are excluded at compile time via
// `nitrogen_no_*` cfg flags emitted by build.rs.
// Infrastructure modules are always included (no cfg gate).

pub mod acpi;
pub mod apic;
pub mod apic_controller;
pub mod debug;
pub mod driver_api;
pub mod driver_context;
pub mod error;
pub mod metrics;
pub mod mmio;
pub mod pci;
pub mod pci_error;
pub mod pci_health;
pub mod port;

// ── Excludable drivers (gated by .driverignore) ──────────────
#[cfg(not(nitrogen_no_audio))]
pub mod audio;
#[cfg(not(nitrogen_no_framebuffer))]
pub mod framebuffer;
#[cfg(not(nitrogen_no_hda))]
pub mod hda;
#[cfg(not(nitrogen_no_ioapic))]
pub mod ioapic;
#[cfg(not(nitrogen_no_iommu))]
pub mod iommu;
#[cfg(not(nitrogen_no_iwlwifi))]
pub mod iwlwifi;
#[cfg(not(nitrogen_no_pic))]
pub mod pic;
#[cfg(not(nitrogen_no_ps2))]
pub mod ps2;
#[cfg(not(nitrogen_no_storage))]
pub mod storage;
pub mod timing;
#[cfg(not(nitrogen_no_usb))]
pub mod usb;
#[cfg(not(nitrogen_no_virtio))]
pub mod virtio;
#[cfg(not(nitrogen_no_wifi))]
pub mod wifi;

pub use driver_context::{DriverContext, DriverContextError, PageFlags};
pub use error::DriverError;

#[cfg(test)]
mod tests {
    use crate::driver_context::{DriverContext, DriverContextError, PageFlags};
    struct FakeDriverContext;

    impl FakeDriverContext {
        fn new() -> Self {
            Self
        }
    }

    impl DriverContext for FakeDriverContext {
        fn phys_to_virt(&self, phys: u64) -> usize {
            (phys + 0xFFFF800000000000) as usize
        }

        fn allocate_frame(&self) -> Result<u64, DriverContextError> {
            Err(DriverContextError::OutOfMemory)
        }

        fn allocate_contiguous_frames(&self, _count: usize) -> Result<u64, DriverContextError> {
            Err(DriverContextError::OutOfMemory)
        }

        fn map_mmio_region(
            &self,
            _phys: usize,
            _virt: usize,
            _size: usize,
        ) -> Result<(), DriverContextError> {
            Err(DriverContextError::MmioMappingFailed)
        }

        fn map_page(
            &self,
            _virt: usize,
            _phys: usize,
            _flags: PageFlags,
        ) -> Result<(), DriverContextError> {
            Err(DriverContextError::MmioMappingFailed)
        }

        fn free_frame(&self, _phys: u64) {}

        fn free_contiguous_frames(&self, _phys: u64, _count: usize) {}

        fn dma_map(
            &self,
            _device_id: u16,
            phys: u64,
            _size: usize,
        ) -> Result<u64, DriverContextError> {
            Ok(phys)
        }

        fn dma_unmap(&self, _iova: u64, _size: usize) {}
    }

    #[test]
    fn test_driver_context_error_display() {
        assert_eq!(
            alloc::format!("{}", DriverContextError::OutOfMemory),
            "out of memory"
        );
        assert_eq!(
            alloc::format!("{}", DriverContextError::MmioMappingFailed),
            "MMIO mapping failed"
        );
        assert_eq!(
            alloc::format!("{}", DriverContextError::InvalidArgument),
            "invalid argument"
        );
    }

    #[test]
    fn test_driver_context_error_clone() {
        let a = DriverContextError::OutOfMemory;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_page_flags_defaults() {
        let mmio = PageFlags::MMIO;
        assert!(mmio.writable);
        assert!(!mmio.write_combining);
        assert!(!mmio.executable);

        let fb = PageFlags::FRAMEBUFFER_WC;
        assert!(fb.writable);
        assert!(fb.write_combining);
        assert!(!fb.executable);
    }

    #[test]
    fn test_fake_driver_context_trait_is_object_safe() {
        let ctx = FakeDriverContext::new();
        let d: &dyn DriverContext = &ctx;
        assert_eq!(d.phys_to_virt(0x1000), 0xFFFF800000001000);
        assert!(d.allocate_frame().is_err());
        assert!(d.dma_map(0, 0x2000, 4096).is_ok());
    }
}
