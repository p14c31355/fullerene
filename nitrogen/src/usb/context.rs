//! USBContext — top-level USB subsystem.
//!
//! Owns all controllers, port managers, device drivers, and storage
//! devices.  The service may be registered during boot and activated on
//! demand with [`USBContext::enable`], then polled for hotplug.
//!
//! # Usage
//!
//! ```ignore
//! let mut usb = USBContext::new(&kernel_ctx);
//! usb.enable()?;         // PCI scan → init → poll → storage discovery
//! for disk in usb.disks() {
//!     println!("{}", disk.name());
//! }
//! ```

use crate::DriverContext;
use alloc::boxed::Box;
use alloc::vec::Vec;

use super::disk::{Disk, StorageManager};
use super::ehci::context::EhciContext;
use super::host_controller::HostController;
use super::xhci::context::XhciContext;

// ============================================================================
//  ControllerManager — PCI scan, init, polling
// ============================================================================

/// Manages all USB host controllers found on the PCI bus.
#[derive(Default)]
struct ControllerManager {
    ehci: Vec<Box<EhciContext>>,
    xhci: Vec<Box<XhciContext>>,
}

impl ControllerManager {
    fn route_intel_ports_to_xhci(devices: &[crate::pci::PciDevice]) -> bool {
        use crate::pci::PciConfigSpace;

        let has_intel_ehci = devices.iter().any(|dev| {
            dev.vendor_id == 0x8086
                && dev.class_code == 0x0C
                && dev.subclass == 0x03
                && dev.prog_if == 0x20
        });
        if !has_intel_ehci {
            return false;
        }

        let mut routed = false;
        for dev in devices.iter().filter(|dev| {
            dev.vendor_id == 0x8086
                && dev.class_code == 0x0C
                && dev.subclass == 0x03
                && dev.prog_if == 0x30
        }) {
            if !dev.ensure_d0() {
                log::warn!("USB: Intel xHCI failed to enter D0 before port routing");
                continue;
            }
            const XUSB2PR: u8 = 0xD0;
            const USB2PRM: u8 = 0xD4;
            const USB3_PSSEN: u8 = 0xD8;
            const USB3PRM: u8 = 0xDC;
            let read = |offset| {
                PciConfigSpace::read_config_dword(dev.bus, dev.device, dev.function, offset)
            };

            // Linux enables SuperSpeed terminations before moving the USB2
            // data wires, preventing SuperSpeed devices from reconnecting at
            // high speed during the switchover.
            let usb3 = read(USB3PRM);
            PciConfigSpace::write_config_dword_raw(
                dev.bus,
                dev.device,
                dev.function,
                USB3_PSSEN,
                usb3,
            );
            let usb2 = read(USB2PRM);
            PciConfigSpace::write_config_dword_raw(
                dev.bus,
                dev.device,
                dev.function,
                XUSB2PR,
                usb2,
            );
            let usb2_active = read(XUSB2PR);
            let usb3_active = read(USB3_PSSEN);
            routed |= usb2 != 0 && usb2_active == usb2;
            log::info!(
                "USB: Intel routing USB2={:#x}/{:#x} USB3={:#x}/{:#x}",
                usb2_active,
                usb2,
                usb3_active,
                usb3,
            );
        }
        routed
    }

    /// Scan the PCI bus and initialise every USB controller found.
    fn init_controllers(&mut self, ctx: &'static dyn DriverContext) {
        use crate::pci::{PciConfigSpace, PciScanner};

        log::info!("USB: scanning PCI for USB host controllers");
        let mut scanner = PciScanner::new();
        if let Err(e) = scanner.scan_all_buses() {
            log::info!("USB: PCI scan failed: {:?}", e);
            return;
        }
        let intel_ports_routed = Self::route_intel_ports_to_xhci(scanner.get_devices());
        let mut controllers: Vec<_> = scanner
            .get_devices()
            .iter()
            .filter(|dev| dev.class_code == 0x0C && dev.subclass == 0x03)
            .collect();
        // Initialise xHCI before its EHCI companion regardless of PCI scan order.
        controllers.sort_by_key(|dev| dev.prog_if != 0x30);
        let found_any = !controllers.is_empty();
        let mut intel_xhci_active = false;
        for dev in controllers {
            if dev.prog_if == 0x20
                && dev.vendor_id == 0x8086
                && intel_ports_routed
                && intel_xhci_active
            {
                log::info!(
                    "USB: skipping routed Intel EHCI companion at {:02x}:{:02x}.{}",
                    dev.bus,
                    dev.device,
                    dev.function
                );
                continue;
            }
            log::info!(
                "USB: found controller at {:02x}:{:02x}.{} (vendor={:#06x} device={:#06x})",
                dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id
            );

            let mmio_base = match dev.read_bar(0) {
                Some(addr) => addr,
                None => {
                    log::info!("USB: controller has no BAR0, skipping");
                    continue;
                }
            };

            // Avoid destructive BAR-size probing while firmware or a previous
            // controller instance may still be active. Mapping extra pages is
            // harmless; no transaction occurs until a register is accessed.
            let bar_size = super::HOST_CONTROLLER_BAR_SIZE;

            let mmio_virt = ctx.phys_to_virt(mmio_base) as *mut u8;
            if mmio_virt.is_null() {
                log::info!("USB: phys_to_virt returned null for BAR0={:#x}", mmio_base);
                continue;
            }

            if !dev.prepare_mmio() {
                log::warn!(
                    "USB: failed to enter D0 or enable MMIO at {:02x}:{:02x}.{}",
                    dev.bus,
                    dev.device,
                    dev.function
                );
                continue;
            }

            // ── Map the MMIO BAR before touching any registers ──────────
            // Without this, MMIO reads to an unmapped page-table entry will
            // page-fault and hang the CPU.  PCI MMIO aperture is NOT part of
            // the direct physical-memory map, so phys_to_virt alone is
            // insufficient.
            crate::debug::hint(b"us_map");
            log::info!(
                "USB: mapping MMIO BAR0 {:#x} -> virt {:#p} ({} bytes)",
                mmio_base, mmio_virt, bar_size
            );
            if ctx
                .map_mmio_region(mmio_base as usize, mmio_virt as usize, bar_size)
                .is_err()
            {
                log::info!(
                    "USB: failed to map MMIO for {:02x}:{:02x}.{}, skipping",
                    dev.bus, dev.device, dev.function
                );
                continue;
            }

            // ── Confirm device is safe to access before MMIO ─────────
            // Even with MMIO mapped in page tables, a non-posted read to an
            // unresponsive device (D3, link down, ASPM L1 wedge) can hang the
            // CPU indefinitely.  PciHealth checks vendor ID, D0, and PCIe link
            // status through PCI config space (port I/O, always safe), then
            // disables ASPM — all before we issue a single MMIO read.
            // Also disable ASPM on the upstream PCIe bridge (if any).
            let upstream = scanner.get_devices().iter().find(|bridge| {
                bridge.class_code == 0x06
                    && bridge.subclass == 0x04
                    && PciConfigSpace::read_config_byte(
                        bridge.bus, bridge.device, bridge.function, 0x19,
                    ) == dev.bus
            });
            if let Some(up) = upstream {
                up.disable_pcie_aspm();
            }

            use crate::pci_health::PciHealth;
            let mut health = upstream.map_or_else(
                || PciHealth::new(dev),
                |bridge| {
                    PciHealth::new(dev)
                        .with_upstream_bridge(bridge.bus, bridge.device, bridge.function)
                },
            );
            if health.pre_mmio_access().is_err() {
                log::info!(
                    "USB: device at {:02x}:{:02x}.{} failed health check (not in D0 or link \
                     down) — skipping",
                    dev.bus, dev.device, dev.function
                );
                continue;
            }

            crate::debug::hint(b"us_pif");
            match dev.prog_if {
                0x20 => {
                    log::info!(
                        "USB: EHCI at {:02x}:{:02x}.{} — initialising",
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                    crate::debug::hint(b"eh_new");
                    if let Some(mut hc) = unsafe { EhciContext::new(mmio_virt, ctx, health) } {
                        if hc.initialize().is_ok() {
                            log::info!("USB: EHCI init OK, {} ports", hc.n_ports());
                            self.ehci.push(Box::new(hc));
                        } else {
                            log::info!("USB: EHCI init failed for {:02x}:{:02x}.{}",
                                dev.bus, dev.device, dev.function);
                        }
                    } else {
                        log::info!("USB: EHCI new failed for {:02x}:{:02x}.{}",
                            dev.bus, dev.device, dev.function);
                    }
                }
                0x30 => {
                    log::info!(
                        "USB: xHCI at {:02x}:{:02x}.{} — initialising",
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                    if let Some(mut hc) = unsafe { XhciContext::new(mmio_virt, ctx, health) } {
                        if hc.init().is_ok() {
                            log::info!("USB: xHCI init OK, {} ports", hc.n_ports());
                            self.xhci.push(Box::new(hc));
                            intel_xhci_active |= dev.vendor_id == 0x8086;
                        } else {
                            log::info!("USB: xHCI init failed for {:02x}:{:02x}.{}",
                                dev.bus, dev.device, dev.function);
                        }
                    } else {
                        log::info!("USB: xHCI new failed for {:02x}:{:02x}.{}",
                            dev.bus, dev.device, dev.function);
                    }
                }
                _ => {
                    log::info!(
                        "USB: unknown prog_if 0x{:02x} at {:02x}:{:02x}.{}",
                        dev.prog_if,
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                }
            }
        }
        if !found_any {
            log::info!("USB: no host controllers found on PCI bus");
        }
    }

    /// Poll all controllers; returns newly discovered devices.
    fn poll(&mut self) -> ControllerEvent {
        let mut ehci_devices: Vec<(usize, usize)> = Vec::new();
        let mut xhci_devices: Vec<(usize, usize)> = Vec::new();

        for (idx, ehci) in self.ehci.iter_mut().enumerate() {
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
/// Call [`enable`](Self::enable) before the first poll, then call
/// [`poll`](Self::poll) from the service scheduler to handle hotplug.
pub struct USBContext {
    controllers: ControllerManager,
    storage: StorageManager,
    driver_ctx: &'static dyn DriverContext,
    enabled: bool,
}

impl USBContext {
    /// Create an empty USB context.
    pub fn new(driver_ctx: &'static dyn DriverContext) -> Self {
        Self {
            controllers: ControllerManager::default(),
            storage: StorageManager::new(),
            driver_ctx,
            enabled: false,
        }
    }

    /// Enable USB hardware without invoking polling or filesystem policy.
    pub fn enable(&mut self) -> Result<(), &'static str> {
        if self.enabled {
            return Ok(());
        }
        self.controllers.init_controllers(self.driver_ctx);
        self.enabled = true;
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Poll all controllers for hotplug events and register new storage.
    pub fn poll(&mut self) {
        let ev = self.controllers.poll();

        for (ctrl_idx, dev_idx) in &ev.ehci_devices {
            self.register_ehci_storage(*ctrl_idx, *dev_idx);
        }
        for (ctrl_idx, dev_idx) in &ev.xhci_devices {
            self.register_xhci_storage(*ctrl_idx, *dev_idx);
        }
    }

    /// References to all discovered storage disks.
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
        ep_out_mps: u16,
        ep_in: u8,
        ep_in_mps: u16,
        lba: u32,
        count: u16,
        block_size: u32,
        buf: &mut [u8],
        tag: &mut u32,
    ) -> Result<(), &'static str> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => {
                if ctrl_idx >= self.controllers.xhci.len() {
                    return Err("xHCI controller index out of bounds");
                }
                &mut *self.controllers.xhci[ctrl_idx]
            }
            _ => {
                if ctrl_idx >= self.controllers.ehci.len() {
                    return Err("EHCI controller index out of bounds");
                }
                &mut *self.controllers.ehci[ctrl_idx]
            }
        };
        super::usb_bus::bot_read_sectors(
            host, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, lba, count, block_size, buf, tag,
        )
    }

    /// Perform a BOT write via the identified controller.
    pub fn bot_write(
        &mut self,
        ctrl_type: &str,
        ctrl_idx: usize,
        dev_addr: u8,
        ep_out: u8,
        ep_out_mps: u16,
        ep_in: u8,
        ep_in_mps: u16,
        lba: u32,
        count: u16,
        block_size: u32,
        buf: &[u8],
        tag: &mut u32,
    ) -> Result<(), &'static str> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => {
                if ctrl_idx >= self.controllers.xhci.len() {
                    return Err("xHCI controller index out of bounds");
                }
                &mut *self.controllers.xhci[ctrl_idx]
            }
            _ => {
                if ctrl_idx >= self.controllers.ehci.len() {
                    return Err("EHCI controller index out of bounds");
                }
                &mut *self.controllers.ehci[ctrl_idx]
            }
        };
        super::usb_bus::bot_write_sectors(
            host, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, lba, count, block_size, buf, tag,
        )
    }

    // ── Internal storage discovery ─────────────────────────

    fn register_ehci_storage(&mut self, ctrl_idx: usize, dev_idx: usize) {
        // Borrow scope to avoid conflicting with self.storage later.
        let (dev_addr, bulk_out, bulk_in) = {
            let ehci: &mut EhciContext = &mut *self.controllers.ehci[ctrl_idx];
            ehci.reset_pools();

            let dev = {
                let mut addr_slot = ehci.next_address;
                let result = {
                    let mut ctrl_fn = |addr, ep, setup: &super::UsbSetupPacket, buf: &mut [u8]| {
                        ehci.control_transfer(addr, ep, setup, buf)
                    };
                    super::hub::enumerate_device(&mut ctrl_fn, &mut addr_slot)
                };
                ehci.next_address = addr_slot;
                result
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

        self.storage
            .try_register("EHCI", dev_addr, bulk_out, bulk_in, ctrl_idx);
    }

    fn register_xhci_storage(&mut self, ctrl_idx: usize, dev_idx: usize) {
        let (dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps) = {
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
            let (ep_out, ep_out_mps, ep_in, ep_in_mps, _blk) = match result {
                Ok(v) => v,
                Err(_) => {
                    let _ = xhci.disable_slot(slot_id);
                    return;
                }
            };

            // Use the device-reported wMaxPacketSize for each endpoint.
            // For SuperSpeed (USB 3.x) bulk endpoints this must be 1024;
            // for High-speed / Full-speed it is 512.  Using the wrong value
            // causes the controller to mis-segment transfers and the
            // device may silently fail to enumerate or stall on the
            // first bulk transfer.
            if xhci
                .configure_endpoint_bulk(slot_id, ep_out, ep_out_mps)
                .is_err()
            {
                let _ = xhci.disable_slot(slot_id);
                return;
            }
            if xhci
                .configure_endpoint_bulk(slot_id, ep_in, ep_in_mps)
                .is_err()
            {
                let _ = xhci.disable_slot(slot_id);
                return;
            }
            (dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps)
        };

        self.storage.try_register_with_mps(
            "xHCI", dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, ctrl_idx,
        );
    }
}
