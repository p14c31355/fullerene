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

// Infrastructure modules shared by every architecture.
#[cfg(not(target_arch = "xtensa"))]
pub mod acpi;
#[cfg(not(target_arch = "xtensa"))]
pub mod apic;
#[cfg(not(target_arch = "xtensa"))]
pub mod apic_controller;
pub mod arch;
pub mod debug;
#[cfg(not(target_arch = "xtensa"))]
pub mod driver_api;
pub mod driver_context;
pub mod error;
#[cfg(not(target_arch = "xtensa"))]
pub mod hbd;
#[cfg(not(target_arch = "xtensa"))]
pub mod hid;
#[cfg(not(target_arch = "xtensa"))]
pub mod i2c_hid;
pub mod metrics;
#[cfg(not(target_arch = "xtensa"))]
pub mod mmio;
#[cfg(not(target_arch = "xtensa"))]
pub mod pci;
#[cfg(not(target_arch = "xtensa"))]
pub mod pci_error;
#[cfg(not(target_arch = "xtensa"))]
pub mod pci_health;
#[cfg(not(target_arch = "xtensa"))]
pub mod port;

// Desktop drivers remain available on x86_64/AArch64 while the Xtensa
// architecture uses the bounded drivers under src/arch.
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_audio)))]
pub mod audio;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_framebuffer)))]
pub mod framebuffer;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_hda)))]
pub mod hda;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_ioapic)))]
pub mod ioapic;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_iommu)))]
pub mod iommu;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_iwlwifi)))]
pub mod iwlwifi;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_pic)))]
pub mod pic;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_ps2)))]
pub mod ps2;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_storage)))]
pub mod storage;
#[cfg(not(target_arch = "xtensa"))]
pub mod timing;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_usb)))]
pub mod usb;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_virtio)))]
pub mod virtio;
#[cfg(all(not(target_arch = "xtensa"), not(nitrogen_no_wifi)))]
pub mod wifi;

pub use driver_context::{DmaAllocation, DriverContext, DriverContextError, PageFlags};
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

        fn zero_dma_buffer(&self, _phys: u64, _bytes: usize) {}

        fn allocate_frame(&self) -> Result<u64, DriverContextError> {
            Err(DriverContextError::OutOfMemory)
        }

        fn allocate_contiguous_frames(&self, _count: usize) -> Result<u64, DriverContextError> {
            Ok(0x2000)
        }

        fn map_mmio_region(
            &self,
            _phys: usize,
            _virt: usize,
            _size: usize,
        ) -> Result<(), DriverContextError> {
            Err(DriverContextError::MmioMappingFailed)
        }

        fn unmap_mmio_region(&self, _phys: usize, _virt: usize, _size: usize) {}

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
        assert_eq!(
            alloc::format!("{}", DriverContextError::DmaMappingFailed),
            "DMA mapping failed"
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
        let allocation = d.allocate_dma_buffer(0x0200, 8193).unwrap();
        assert_eq!(allocation.phys, 0x2000);
        assert_eq!(allocation.iova, 0x2000);
        assert_eq!(allocation.frames, 3);
        d.release_dma_buffer(allocation);
    }
}
