//! DeviceContext — aggregates PCI bus information with concrete driver
//! state for storage, audio, display, and input controllers.
//!
//! Replaces the previous pattern where `DeviceManager` held metadata but
//! actual driver objects (AHCI, NVMe, HDA, VirtioGpu) were scattered
//! across `crate::ahci`, `crate::nvme`, `crate::virtio_gpu`, and
//! `FramebufferContext.gpu`.
//!
//! # Layout
//!
//! ```text
//! DeviceContext
//!  ├ pci: PciContext          ← bus topology
//!  ├ ahci: Option<Ahci>       ← SATA
//!  ├ nvme: Option<Nvme>       ← NVMe
//!  ├ hda: Option<Hda>         ← HD Audio controller
//!  └ gpu: Option<VirtioGpu>   ← VirtIO GPU
//! ```

use alloc::boxed::Box;
use nitrogen::hda::HdaController;
use nitrogen::virtio::gpu::VirtioGpu;
use super::pci::PciContext;

/// Concrete driver state for all discovered hardware.
pub struct DeviceContext {
    /// PCI bus topology (vendor/device IDs, BARs, etc.)
    pub pci: PciContext,

    /// AHCI SATA controller (if present)
    pub ahci: Option<crate::ahci::AhciController>,

    /// NVMe controller (if present)
    pub nvme: Option<crate::nvme::NvmeController>,

    /// Intel HD Audio / High Definition Audio controller
    pub hda: Option<HdaController>,

    /// VirtIO GPU (used under QEMU / KVM)
    pub gpu: Option<Box<VirtioGpu>>,
}

// DeviceContext lives behind a Mutex; interior Send+Sync covered by sub-fields.
unsafe impl Send for DeviceContext {}
unsafe impl Sync for DeviceContext {}

impl DeviceContext {
    /// Create an empty device context (no probes performed).
    pub fn new(pci: PciContext) -> Self {
        Self {
            pci,
            ahci: None,
            nvme: None,
            hda: None,
            gpu: None,
        }
    }

    /// True when a usable framebuffer-backed GPU is available.
    pub fn has_display(&self) -> bool {
        self.gpu.is_some()
    }

    /// True when any storage controller is present.
    pub fn has_storage(&self) -> bool {
        self.ahci.is_some() || self.nvme.is_some()
    }

    /// True when an HD Audio controller is present.
    pub fn has_audio(&self) -> bool {
        self.hda.is_some()
    }
}

// ── Global singleton ──────────────────────────────────────────
use spin::Mutex;

static DEVICE: Mutex<Option<DeviceContext>> = Mutex::new(None);

/// Initialise the global DeviceContext.
///
/// Must be called *after* PCI scanning so that device discovery can
/// find actual hardware.
pub fn init_device() {
    // Build a fresh PciContext and scan it.
    let mut pci = PciContext::new();
    let _ = pci.scan();

    let mut dc = DeviceContext::new(pci);

    // Probe HDA
    if let Some(hda_dev) = dc.pci.find_hda().cloned() {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        if let Some(bar) = hda_dev.get_bar_info(0) {
            let bar0 = bar.address;
            if bar0 != 0 {
                let mmio = (bar0 + off) as *mut u8;
                dc.hda = Some(HdaController::new(mmio, bar0));
            }
        }
    }

    // Probe AHCI (placeholder — actual init lives in crate::ahci)
    // dc.ahci = crate::ahci::AhciController::probe(&dc.pci);

    // Probe NVMe (placeholder)
    // dc.nvme = crate::nvme::NvmeController::probe(&dc.pci);

    // Probe VirtIO GPU (placeholder — actual init lives in crate::virtio_gpu)
    // dc.gpu = crate::virtio_gpu::probe_virtio_gpu(&dc.pci);

    *DEVICE.lock() = Some(dc);
}

/// Get the global device context mutex.
pub fn get_device() -> &'static Mutex<Option<DeviceContext>> {
    &DEVICE
}

/// Execute a read-only closure over the DeviceContext.
pub fn with_device<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&DeviceContext) -> R,
{
    DEVICE.lock().as_ref().map(f)
}

/// Execute a mutable closure over the DeviceContext.
pub fn with_device_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut DeviceContext) -> R,
{
    DEVICE.lock().as_mut().map(f)
}