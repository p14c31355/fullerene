//! USBContext — top-level USB subsystem.
//!
//! Owns all controllers, port managers, device drivers, and storage
//! devices.  The kernel calls [`USBContext::enable`] once at boot,
//! then [`USBContext::poll`] periodically for hotplug.
//!
//! # Usage
//!
//! ```ignore
//! let mut usb = USBContext::new(&kernel_ctx);
//! usb.enable()?;         // PCI scan → init → poll → auto-mount
//! for disk in usb.disks() {
//!     println!("{}", disk.name());
//! }
//! ```

use crate::DriverContext;
use alloc::boxed::Box;
use alloc::vec::Vec;

use super::disk::{Disk, StorageManager};
use super::ehci_context::EhciContext;
use super::host_controller::HostController;
use super::xhci_context::XhciContext;

// ============================================================================
//  ControllerManager — PCI scan, init, polling
// ============================================================================

/// Manages all USB host controllers found on the PCI bus.
struct ControllerManager {
    ehci: Vec<Box<EhciContext>>,
    xhci: Vec<Box<XhciContext>>,
}

impl ControllerManager {
    fn new() -> Self {
        Self {
            ehci: Vec::new(),
            xhci: Vec::new(),
        }
    }

    /// Scan the PCI bus and initialise every USB controller found.
    fn init_controllers(&mut self, ctx: &'static dyn DriverContext) {
        use crate::pci::PciScanner;
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();
        for dev in scanner.get_devices() {
            if dev.class_code != 0x0C || dev.subclass != 0x03 {
                continue;
            }
            let mmio_base = match dev.read_bar(0) {
                Some(addr) => addr,
                None => continue,
            };
            let mmio_virt = ctx.phys_to_virt(mmio_base) as *mut u8;
            if mmio_virt.is_null() {
                continue;
            }
            dev.enable_memory_access();
            dev.ensure_d0();

            let prog_if = crate::pci::PciConfigSpace::read_config_byte(
                dev.bus, dev.device, dev.function, 0x09,
            );
            match prog_if {
                0x20 => {
                    if let Some(mut hc) = EhciContext::new(mmio_virt, ctx) {
                        if hc.initialize().is_ok() {
                            self.ehci.push(Box::new(hc));
                        }
                    }
                }
                0x30 => {
                    if let Some(mut hc) = XhciContext::new(mmio_virt, ctx) {
                        if hc.init().is_ok() {
                            self.xhci.push(Box::new(hc));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Poll all controllers; returns newly discovered devices.
    fn poll(&mut self) -> ControllerEvent {
        let mut ehci_devices: Vec<(usize, usize)> = Vec::new();
        let mut xhci_devices: Vec<(usize, usize)> = Vec::new();

        for (idx, ehci) in self.ehci.iter_mut().enumerate() {
            let _ = ehci.start();
            let old = ehci.devices().len();
            let new = ehci.poll_ports();
            if new > 0 {
                for d in old..ehci.devices().len() {
                    ehci_devices.push((idx, d));
                }
            }
        }

        for (idx, xhci) in self.xhci.iter_mut().enumerate() {
            xhci.clear_hse_and_recover();
            let old = xhci.devices().len();
            let new = xhci.poll_ports();
            if new > 0 {
                for d in old..xhci.devices().len() {
                    xhci_devices.push((idx, d));
                }
            }
        }

        ControllerEvent {
            ehci_devices,
            xhci_devices,
        }
    }

    fn debug_dump(&self) {
        log::info!("=== USB DEBUG ===");
        for (i, ehci) in self.ehci.iter().enumerate() {
            log::info!("EHCI[{}]: {} ports", i, ehci.n_ports());
            for p in 0..(ehci.n_ports().min(4)) {
                let ps = ehci.read_portsc(p);
                log::info!(
                    "  PORTSC[{}]=0x{:08X} CCS={} PE={}",
                    p, ps, ps & 1, (ps >> 2) & 1
                );
            }
        }
        for (i, xhci) in self.xhci.iter().enumerate() {
            log::info!(
                "xHCI[{}] ppc={} n_ports={} max_slots={} ports_done={:#x} legacy={}",
                i,
                xhci.ppc_enabled(),
                xhci.n_ports(),
                xhci.max_slots(),
                xhci.ports_done_mask(),
                xhci.legacy_handoff_done()
            );
            for p in 0..xhci.n_ports() {
                let ps = xhci.read_portsc(p);
                if ps == 0xFFFF {
                    continue;
                }
                log::info!(
                    "xHCI PORTSC[{}]={:#x} CCS={} PED={} PLS={} PP={} PR={} speed={}",
                    p, ps, ps & 1, (ps >> 1) & 1,
                    (ps >> 5) & 0xF, (ps >> 9) & 1, (ps >> 4) & 1, (ps >> 10) & 0xF
                );
            }
        }
        log::info!("=== USB END ===");
    }
}

/// Events from a single poll cycle.
struct ControllerEvent {
    ehci_devices: Vec<(usize, usize)>,
    xhci_devices: Vec<(usize, usize)>,
}

// ============================================================================
//  USBContext — public top-level API
// ============================================================================

/// Top-level USB subsystem handle.
///
/// Call [`enable`](Self::enable) once at boot; call [`poll`](Self::poll)
/// from a background timer to handle hotplug.
pub struct USBContext {
    controllers: ControllerManager,
    storage: StorageManager,
    driver_ctx: &'static dyn DriverContext,
}

impl USBContext {
    /// Create an empty USB context.
    pub fn new(driver_ctx: &'static dyn DriverContext) -> Self {
        Self {
            controllers: ControllerManager::new(),
            storage: StorageManager::new(),
            driver_ctx,
        }
    }

    /// Enable USB: scan PCI, initialise controllers, poll once, and
    /// auto-mount any mass-storage devices found.
    pub fn enable(&mut self) -> Result<(), &'static str> {
        self.controllers.init_controllers(self.driver_ctx);
        self.controllers.debug_dump();
        self.poll();
        Ok(())
    }

    /// Poll all controllers for hotplug events and mount new devices.
    pub fn poll(&mut self) {
        let ev = self.controllers.poll();

        for (_idx, ctrl_idx) in &ev.ehci_devices {
            self.mount_ehci_device(*ctrl_idx, *ctrl_idx);
        }
        for (_idx, ctrl_idx) in &ev.xhci_devices {
            self.mount_xhci_device(*ctrl_idx, *ctrl_idx);
        }
    }

    /// References to all mounted storage disks.
    pub fn disks(&self) -> &[Disk] {
        self.storage.disks()
    }

    /// Perform a BOT read via the identified controller.
    pub fn bot_read(
        &mut self,
        ctrl_type: &str,
        ctrl_idx: usize,
        dev_addr: u8,
        ep_out: u8,
        ep_in: u8,
        lba: u32,
        count: u16,
        block_size: u32,
        buf: &mut [u8],
        tag: &mut u32,
    ) -> Result<(), &'static str> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => &mut *self.controllers.xhci[ctrl_idx],
            _ => &mut *self.controllers.ehci[ctrl_idx],
        };
        super::usb_bus::bot_read_sectors(host, dev_addr, ep_out, ep_in, lba, count, block_size, buf, tag)
    }

    /// Perform a BOT write via the identified controller.
    pub fn bot_write(
        &mut self,
        ctrl_type: &str,
        ctrl_idx: usize,
        dev_addr: u8,
        ep_out: u8,
        ep_in: u8,
        lba: u32,
        count: u16,
        block_size: u32,
        buf: &[u8],
        tag: &mut u32,
    ) -> Result<(), &'static str> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => &mut *self.controllers.xhci[ctrl_idx],
            _ => &mut *self.controllers.ehci[ctrl_idx],
        };
        super::usb_bus::bot_write_sectors(host, dev_addr, ep_out, ep_in, lba, count, block_size, buf, tag)
    }

    // ── Internal mount helpers ──────────────────────────────

    fn mount_ehci_device(&mut self, ctrl_idx: usize, dev_idx: usize) {
        // Borrow scope to avoid conflicting with self.storage later.
        let (dev_addr, bulk_out, bulk_in) = {
            let ehci: &mut EhciContext = &mut *self.controllers.ehci[ctrl_idx];
            ehci.reset_pools();

            let dev = {
                let mut ctrl_fn = |addr, ep, setup: &super::UsbSetupPacket, buf: &mut [u8]| {
                    ehci.control_transfer(addr, ep, setup, buf)
                };
                super::hub::enumerate_device(&mut ctrl_fn)
            };
            let dev = match dev {
                Ok(d) if d.is_mass_storage() => d,
                _ => return,
            };

            if let Some(slot) = ehci.devices_mut().get_mut(dev_idx) {
                *slot = dev.clone();
            }

            let mut bulk_out = 0u8;
            let mut bulk_in = 0u8;
            for ep in &dev.endpoints {
                if ep.xfer_type() != super::UsbXferType::Bulk {
                    continue;
                }
                match ep.direction() {
                    super::UsbDirection::Out => bulk_out = ep.b_endpoint_address,
                    super::UsbDirection::In => bulk_in = ep.b_endpoint_address,
                }
            }
            if bulk_out == 0 || bulk_in == 0 {
                return;
            }
            (dev.address, bulk_out, bulk_in)
        };

        self.storage.try_mount("EHCI", dev_addr, bulk_out, bulk_in, ctrl_idx);
    }

    fn mount_xhci_device(&mut self, ctrl_idx: usize, dev_idx: usize) {
        let (dev_addr, ep_out, ep_in) = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];

            let slot_id = match xhci.enable_slot() {
                Ok(id) => id,
                Err(_) => return,
            };
            if xhci.address_device(slot_id).is_err() {
                let _ = xhci.disable_slot(slot_id);
                return;
            }

            let dev_addr = slot_id as u8;
            let result = super::usb_bus::enumerate_mass_storage(
                xhci as &mut dyn HostController,
                dev_addr,
                dev_idx,
            );
            let (ep_out, ep_in, _blk) = match result {
                Ok(v) => v,
                Err(_) => return,
            };

            if xhci.configure_endpoint_bulk(slot_id, ep_out, 512).is_err() {
                return;
            }
            if xhci.configure_endpoint_bulk(slot_id, ep_in, 512).is_err() {
                return;
            }
            (dev_addr, ep_out, ep_in)
        };

        self.storage.try_mount("xHCI", dev_addr, ep_out, ep_in, ctrl_idx);
    }
}
