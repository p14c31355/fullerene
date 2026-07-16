use alloc::vec::Vec;
use log;
use spin::Mutex;

use nitrogen::DriverContext;
use nitrogen::driver_api::DriverRegistry;
use nitrogen::pci::PciDevice;

/// Metadata about an attached driver (without the driver instance itself).
#[derive(Debug, Clone, Copy)]
pub struct AttachedDriverInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// Orchestrates the driver lifecycle: probe → priority → attach → registration.
///
/// Usage:
/// ```ignore
/// let mgr = DriverManager::new();
/// mgr.discover_and_attach(&registry, ctx, &devices);
/// ```
pub struct DriverManager {
    attached: Mutex<Vec<AttachedDriverInfo>>,
}

impl DriverManager {
    pub const fn new() -> Self {
        Self {
            attached: Mutex::new(Vec::new()),
        }
    }

    /// Run the full probe → attach pipeline for every registered driver
    /// against the given PCI devices.
    ///
    /// 1. For each device, find matching drivers via `DriverRegistry`
    /// 2. Call `probe()` — candidates are tried in priority order
    /// 3. Call `attach()` — finalises initialisation (falls back to next
    ///    candidate if a higher‑priority driver fails to attach)
    /// 4. Store metadata for lifecycle management
    pub fn discover_and_attach(
        &self,
        registry: &DriverRegistry,
        ctx: &dyn DriverContext,
        devices: &[PciDevice],
    ) {
        for dev in devices {
            if dev.class_code == 0x06 {
                continue;
            }
            let mut candidates = registry.probe_candidates(ctx, dev);
            let mut attached = false;
            for candidate in &mut candidates {
                match candidate.attach() {
                    Ok(()) => {
                        log::info!(
                            "DriverManager: attached driver for {:04x}:{:04x} (class {:#04x}) at {:02x}:{:02x}.{}",
                            dev.vendor_id,
                            dev.device_id,
                            dev.class_code,
                            dev.bus,
                            dev.device,
                            dev.function,
                        );
                        self.attached.lock().push(AttachedDriverInfo {
                            vendor_id: dev.vendor_id,
                            device_id: dev.device_id,
                            class_code: dev.class_code,
                            subclass: dev.subclass,
                            bus: dev.bus,
                            device: dev.device,
                            function: dev.function,
                        });
                        attached = true;
                        break;
                    }
                    Err(e) => {
                        log::warn!(
                            "DriverManager: attach failed for {:04x}:{:04x} — {}",
                            dev.vendor_id,
                            dev.device_id,
                            e,
                        );
                    }
                }
            }
            if candidates.is_empty() && !attached {
                log::debug!(
                    "DriverManager: no driver for {:04x}:{:04x} (class {:#04x}) at {:02x}:{:02x}.{}",
                    dev.vendor_id,
                    dev.device_id,
                    dev.class_code,
                    dev.bus,
                    dev.device,
                    dev.function,
                );
            }
        }
    }

    pub fn attached_count(&self) -> usize {
        self.attached.lock().len()
    }

    pub fn with_attached<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&[AttachedDriverInfo]) -> R,
    {
        let guard = self.attached.lock();
        f(&guard)
    }
}
