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
//!     println!("{} blocks", disk.total_blocks);
//! }
//! ```

use crate::DriverContext;
use alloc::boxed::Box;
use alloc::vec::Vec;

use super::disk::{Disk, StorageManager};
use super::ehci::context::EhciContext;
use super::host_controller::HostController;
use super::xhci::context::XhciContext;

/// Read-only state exposed by [`USBContext`] for shell and boot diagnostics.
///
/// Keeping this as a value type avoids exposing controller ownership or MMIO
/// details to the kernel UI while still making real-hardware bring-up useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbControllerInfo {
    pub kind: &'static str,
    pub ports: u32,
    pub running: bool,
    pub devices: usize,
    pub done_ports: u32,
}

// ============================================================================
//  ControllerManager — PCI scan, init, polling
// ============================================================================

/// Manages all USB host controllers found on the PCI bus.
#[derive(Default)]
struct ControllerManager {
    ehci: Vec<Box<EhciContext>>,
    xhci: Vec<Box<XhciContext>>,
}

#[derive(Clone, Copy)]
struct IntelPortRoute {
    bus: u8,
    device: u8,
    function: u8,
    routed: bool,
}

impl ControllerManager {
    fn route_intel_ports_to_xhci(devices: &[crate::pci::PciDevice]) -> Vec<IntelPortRoute> {
        use crate::pci::PciConfigSpace;

        let has_intel_ehci = devices.iter().any(|dev| {
            dev.vendor_id == 0x8086
                && dev.class_code == 0x0C
                && dev.subclass == 0x03
                && dev.prog_if == 0x20
        });
        if !has_intel_ehci {
            return Vec::new();
        }

        let mut routes = Vec::new();
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
            // XUSB2PRM/USB3PRM describe the ports that are switchable, not
            // necessarily the complete current routing bitmap.  Firmware
            // may already have additional USB2 ports routed to xHCI, so an
            // exact read-back comparison incorrectly leaves the EHCI
            // companion active alongside xHCI.
            let usb2_ok = usb2 == 0 || (usb2_active & usb2) == usb2;
            let usb3_ok = usb3 == 0 || (usb3_active & usb3) == usb3;
            let routed = usb2_ok && usb3_ok;
            routes.push(IntelPortRoute {
                bus: dev.bus,
                device: dev.device,
                function: dev.function,
                routed,
            });
            log::info!(
                "USB: Intel routing USB2={:#x}/{:#x} USB3={:#x}/{:#x} routed={}",
                usb2_active,
                usb2,
                usb3_active,
                usb3,
                routed,
            );
        }
        routes
    }

    fn disable_intel_port_routes(routes: &[IntelPortRoute]) {
        use crate::pci::PciConfigSpace;

        for route in routes {
            // Match Linux's usb_disable_xhci_ports(): zeroing both routing
            // registers hands switchable USB2 wires back to EHCI and turns
            // off xHCI SuperSpeed terminations. Restoring an arbitrary BIOS
            // value could leave ports assigned to a controller that just
            // failed initialization.
            PciConfigSpace::write_config_dword_raw(
                route.bus,
                route.device,
                route.function,
                0xD8,
                0,
            );
            PciConfigSpace::write_config_dword_raw(
                route.bus,
                route.device,
                route.function,
                0xD0,
                0,
            );
            log::info!(
                "USB: disabled Intel xHCI port routing at {:02x}:{:02x}.{}; EHCI fallback enabled",
                route.bus,
                route.device,
                route.function,
            );
        }
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
        let intel_routes = Self::route_intel_ports_to_xhci(scanner.get_devices());
        let intel_ports_routed = intel_routes.iter().any(|route| route.routed);
        let mut controllers: Vec<_> = scanner
            .get_devices()
            .iter()
            .filter(|dev| dev.class_code == 0x0C && dev.subclass == 0x03)
            .collect();
        // Initialise xHCI before its EHCI companion regardless of PCI scan order.
        controllers.sort_by_key(|dev| dev.prog_if != 0x30);
        let found_any = !controllers.is_empty();
        let mut intel_xhci_active = false;
        let mut intel_routes_restored = false;
        for dev in controllers {
            if dev.prog_if == 0x20
                && dev.vendor_id == 0x8086
                && intel_ports_routed
                && !intel_xhci_active
                && !intel_routes_restored
            {
                // xHCI was tried first but did not become usable. Restore
                // firmware routing before probing the EHCI companion so it
                // can actually see the USB2 ports again.
                Self::disable_intel_port_routes(&intel_routes);
                intel_routes_restored = true;
            }
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
                dev.bus,
                dev.device,
                dev.function,
                dev.vendor_id,
                dev.device_id
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

            // Validate or create the MMIO mapping before touching registers.
            // The kernel preserves a verified boot direct mapping instead of
            // splitting its huge pages, which is unsafe on the target firmware.
            crate::debug::hint(b"us_map");
            log::info!(
                "USB: mapping MMIO BAR0 {:#x} -> virt {:#p} ({} bytes)",
                mmio_base,
                mmio_virt,
                bar_size
            );
            if ctx
                .map_mmio_region(mmio_base as usize, mmio_virt as usize, bar_size)
                .is_err()
            {
                log::info!(
                    "USB: failed to map MMIO for {:02x}:{:02x}.{}, skipping",
                    dev.bus,
                    dev.device,
                    dev.function
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
                        bridge.bus,
                        bridge.device,
                        bridge.function,
                        0x19,
                    ) == dev.bus
            });
            if let Some(up) = upstream {
                up.disable_pcie_aspm();
            }

            use crate::pci_health::PciHealth;
            let mut health = upstream.map_or_else(
                || PciHealth::new(dev),
                |bridge| {
                    PciHealth::new(dev).with_upstream_bridge(
                        bridge.bus,
                        bridge.device,
                        bridge.function,
                    )
                },
            );
            if health.pre_mmio_access().is_err() {
                log::info!(
                    "USB: device at {:02x}:{:02x}.{} failed health check (not in D0 or link \
                     down) — skipping",
                    dev.bus,
                    dev.device,
                    dev.function
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
                    if let Some(mut hc) =
                        unsafe { EhciContext::new(mmio_virt as usize, ctx, health) }
                    {
                        if hc.initialize().is_ok() {
                            log::info!("USB: EHCI init OK, {} ports", hc.n_ports());
                            self.ehci.push(Box::new(hc));
                        } else {
                            log::info!(
                                "USB: EHCI init failed for {:02x}:{:02x}.{}",
                                dev.bus,
                                dev.device,
                                dev.function
                            );
                        }
                    } else {
                        log::info!(
                            "USB: EHCI new failed for {:02x}:{:02x}.{}",
                            dev.bus,
                            dev.device,
                            dev.function
                        );
                    }
                }
                0x30 => {
                    log::info!(
                        "USB: xHCI at {:02x}:{:02x}.{} — initialising",
                        dev.bus,
                        dev.device,
                        dev.function
                    );
                    if let Some(mut hc) =
                        unsafe { XhciContext::new(mmio_virt as usize, ctx, health) }
                    {
                        if hc.init().is_ok() {
                            log::info!("USB: xHCI init OK, {} ports", hc.n_ports());
                            self.xhci.push(Box::new(hc));
                            intel_xhci_active |= dev.vendor_id == 0x8086;
                        } else {
                            log::info!(
                                "USB: xHCI init failed for {:02x}:{:02x}.{}",
                                dev.bus,
                                dev.device,
                                dev.function
                            );
                        }
                    } else {
                        log::info!(
                            "USB: xHCI new failed for {:02x}:{:02x}.{}",
                            dev.bus,
                            dev.device,
                            dev.function
                        );
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

    fn shutdown(&mut self) {
        // Stop xHCI first; Intel EHCI companions may share the same physical
        // ports, but each controller owns independent DMA state.
        for controller in &mut self.xhci {
            controller.shutdown();
        }
        for controller in &mut self.ehci {
            controller.shutdown();
        }
        self.xhci.clear();
        self.ehci.clear();
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
    pub fn enable(&mut self) -> Result<(), crate::DriverError> {
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

    /// Return a safe snapshot of the active host controllers.
    pub fn controller_info(&self) -> Vec<UsbControllerInfo> {
        let mut info =
            Vec::with_capacity(self.controllers.ehci.len() + self.controllers.xhci.len());
        info.extend(
            self.controllers
                .ehci
                .iter()
                .map(|controller| UsbControllerInfo {
                    kind: "EHCI",
                    ports: controller.n_ports(),
                    running: controller.is_running(),
                    devices: controller.devices().len(),
                    done_ports: controller.ports.processed_mask,
                }),
        );
        info.extend(
            self.controllers
                .xhci
                .iter()
                .map(|controller| UsbControllerInfo {
                    kind: "xHCI",
                    ports: controller.n_ports(),
                    running: controller.is_running(),
                    devices: controller.devices().len(),
                    done_ports: controller.ports_done_mask(),
                }),
        );
        info
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

    /// Stop all controllers before replacing this context during rescan.
    pub fn shutdown(&mut self) {
        self.controllers.shutdown();
        self.storage = StorageManager::new();
        self.enabled = false;
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
    ) -> Result<(), crate::DriverError> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => {
                if ctrl_idx >= self.controllers.xhci.len() {
                    return Err(crate::DriverError::InvalidArgument);
                }
                &mut *self.controllers.xhci[ctrl_idx]
            }
            _ => {
                if ctrl_idx >= self.controllers.ehci.len() {
                    return Err(crate::DriverError::InvalidArgument);
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
    ) -> Result<(), crate::DriverError> {
        let host: &mut dyn HostController = match ctrl_type {
            "xHCI" => {
                if ctrl_idx >= self.controllers.xhci.len() {
                    return Err(crate::DriverError::InvalidArgument);
                }
                &mut *self.controllers.xhci[ctrl_idx]
            }
            _ => {
                if ctrl_idx >= self.controllers.ehci.len() {
                    return Err(crate::DriverError::InvalidArgument);
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
        let (dev_addr, bulk_out, bulk_out_mps, bulk_in, bulk_in_mps, block_size, total_blocks) = {
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
                Ok(_) => {
                    log::warn!("USB: EHCI device {} is not mass storage", dev_idx);
                    return;
                }
                Err(error) => {
                    log::warn!("USB: EHCI enumeration failed: {}", error);
                    return;
                }
            };

            if let Some(slot) = ehci.devices_mut().get_mut(dev_idx) {
                *slot = dev.clone();
            }

            let mut bulk_out = 0u8;
            let mut bulk_out_mps = 0u16;
            let mut bulk_in = 0u8;
            let mut bulk_in_mps = 0u16;
            for ep in &dev.endpoints {
                if ep.xfer_type() != super::UsbXferType::Bulk {
                    continue;
                }
                match ep.direction() {
                    super::UsbDirection::Out => {
                        bulk_out = ep.b_endpoint_address;
                        bulk_out_mps = ep.w_max_packet_size;
                    }
                    super::UsbDirection::In => {
                        bulk_in = ep.b_endpoint_address;
                        bulk_in_mps = ep.w_max_packet_size;
                    }
                }
            }
            if bulk_out == 0 || bulk_out_mps == 0 || bulk_in == 0 || bulk_in_mps == 0 {
                log::warn!("USB: EHCI mass-storage device has incomplete bulk endpoints");
                return;
            }
            let mut tag = 1;
            let (block_size, total_blocks) = match super::usb_bus::bot_read_capacity(
                ehci,
                dev.address,
                bulk_out,
                bulk_out_mps,
                bulk_in,
                bulk_in_mps,
                &mut tag,
            ) {
                Ok(capacity) => capacity,
                Err(error) => {
                    log::warn!("USB: EHCI READ CAPACITY failed: {}", error);
                    return;
                }
            };
            (
                dev.address,
                bulk_out,
                bulk_out_mps,
                bulk_in,
                bulk_in_mps,
                block_size,
                total_blocks,
            )
        };

        self.storage.try_register(Disk {
            dev_addr,
            ep_out: bulk_out,
            ep_out_mps: bulk_out_mps,
            ep_in: bulk_in,
            ep_in_mps: bulk_in_mps,
            block_size,
            total_blocks,
            ctrl_type: "EHCI",
            ctrl_idx,
        });
    }

    fn register_xhci_storage(&mut self, ctrl_idx: usize, dev_idx: usize) {
        // Phase 1: enable slot and address the device.
        let (slot_id, dev_addr) = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];

            let slot_id = match xhci.enable_slot() {
                Ok(id) => id,
                Err(error) => {
                    log::warn!("USB: xHCI Enable Slot failed: {}", error);
                    return;
                }
            };
            if let Err(error) = xhci.address_device(slot_id, dev_idx) {
                log::warn!("USB: xHCI Address Device failed: {}", error);
                xhci.retry_device_candidate(slot_id, dev_idx);
                return;
            }

            (slot_id, slot_id as u8)
        };

        // Phase 2: try mass-storage enumeration on the device itself.
        // This is the original code path — no extra control transfers
        // before it, so device state is identical to the pre-hub-support
        // build.
        let msc_result = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            super::usb_bus::enumerate_mass_storage(
                xhci as &mut dyn HostController,
                dev_addr,
                dev_idx,
            )
        };

        match msc_result {
            Ok((ep_out, ep_out_mps, ep_in, ep_in_mps)) => {
                self.finish_xhci_storage(
                    ctrl_idx, slot_id, dev_addr, dev_idx, ep_out, ep_out_mps, ep_in, ep_in_mps,
                );
            }
            Err(crate::DriverError::NotSupported) => {
                // The device is not a BOT mass-storage device.  It may be
                // a USB hub with mass-storage devices behind it.  Try the
                // hub path before giving up.
                log::info!(
                    "USB: xHCI device {} not mass-storage; trying hub enumeration",
                    dev_addr
                );
                let found = self.enumerate_hub_ports_xhci(ctrl_idx, slot_id, dev_idx);
                if !found {
                    let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                    let _ = xhci.disable_slot(slot_id);
                    xhci.remove_device_candidate(dev_idx);
                }
            }
            Err(error) => {
                log::warn!("USB: xHCI mass-storage enumeration failed: {}", error);
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                xhci.retry_device_candidate(slot_id, dev_idx);
            }
        }
    }

    /// Complete mass-storage registration after a successful
    /// `enumerate_mass_storage`.  Configures bulk endpoints, reads
    /// capacity, and registers the disk.
    fn finish_xhci_storage(
        &mut self,
        ctrl_idx: usize,
        slot_id: u32,
        dev_addr: u8,
        dev_idx: usize,
        ep_out: u8,
        ep_out_mps: u16,
        ep_in: u8,
        ep_in_mps: u16,
    ) {
        let (ep_out, ep_out_mps, ep_in, ep_in_mps, block_size, total_blocks) = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];

            if xhci
                .configure_endpoint_bulk(slot_id, ep_out, ep_out_mps)
                .is_err()
            {
                log::warn!("USB: xHCI bulk OUT endpoint configuration failed");
                xhci.retry_device_candidate(slot_id, dev_idx);
                return;
            }
            if xhci
                .configure_endpoint_bulk(slot_id, ep_in, ep_in_mps)
                .is_err()
            {
                log::warn!("USB: xHCI bulk IN endpoint configuration failed");
                xhci.retry_device_candidate(slot_id, dev_idx);
                return;
            }
            let mut tag = 1;
            let (block_size, total_blocks) = match super::usb_bus::bot_read_capacity(
                xhci, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, &mut tag,
            ) {
                Ok(capacity) => capacity,
                Err(error) => {
                    log::warn!("USB: xHCI READ CAPACITY failed: {}", error);
                    xhci.retry_device_candidate(slot_id, dev_idx);
                    return;
                }
            };
            (
                ep_out,
                ep_out_mps,
                ep_in,
                ep_in_mps,
                block_size,
                total_blocks,
            )
        };

        let registered = self.storage.try_register(Disk {
            dev_addr,
            ep_out,
            ep_out_mps,
            ep_in,
            ep_in_mps,
            block_size,
            total_blocks,
            ctrl_type: "xHCI",
            ctrl_idx,
        });
        if registered {
            log::info!(
                "USB: xHCI mass-storage device ready ctrl={} slot={} dev_addr={} block_size={} total_blocks={}",
                ctrl_idx,
                slot_id,
                dev_addr,
                block_size,
                total_blocks,
            );
        }
    }

    /// Enumerate downstream ports of a USB hub connected to an xHCI root
    /// port.  For each port that has a connected device, issue Address
    /// Device with the correct route string and parent slot ID, then look
    /// for mass-storage devices.
    ///
    /// Returns `true` if at least one mass-storage device was registered.
    fn enumerate_hub_ports_xhci(
        &mut self,
        ctrl_idx: usize,
        hub_slot_id: u32,
        hub_dev_idx: usize,
    ) -> bool {
        let hub_addr = hub_slot_id as u8;

        // Determine the root port the hub is on, from the placeholder device.
        let root_port = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            let dev = match xhci.devices().get(hub_dev_idx) {
                Some(d) => d,
                None => return false,
            };
            dev.port_index + 1 // xHCI root ports are 1-based
        };

        // Set configuration 1 on the hub device.
        {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            let setup = super::UsbSetupPacket {
                bm_request_type: 0x00,
                b_request: super::REQ_SET_CONFIGURATION,
                w_value: 1,
                w_index: 0,
                w_length: 0,
            };
            if let Err(error) = HostController::control_transfer(xhci, hub_addr, &setup, &mut []) {
                log::warn!("USB: hub SET_CONFIGURATION failed: {}", error);
                return false;
            }
        }

        // Get Hub Descriptor to learn the number of downstream ports.
        let num_ports = {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            let mut hub_desc = [0u8; 8];
            let setup = super::UsbSetupPacket {
                bm_request_type: 0xA0, // D2H, class, device
                b_request: super::HUB_REQ_GET_DESCRIPTOR,
                w_value: (super::DESC_HUB as u16) << 8,
                w_index: 0,
                w_length: 8,
            };
            match HostController::control_transfer(xhci, hub_addr, &setup, &mut hub_desc) {
                Ok(len) if len >= 4 => {
                    let n = hub_desc[2]; // bNbrPorts
                    log::info!("USB: hub {} has {} downstream ports", hub_addr, n);
                    n
                }
                Ok(len) => {
                    log::warn!("USB: hub descriptor too short: {} bytes", len);
                    return false;
                }
                Err(error) => {
                    log::warn!("USB: hub GET_DESCRIPTOR failed: {}", error);
                    return false;
                }
            }
        };

        // Mark the hub slot so the xHC knows it has downstream ports.
        {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            let speed_id = xhci.registers.op.portsc(root_port as u32 - 1).speed() as u8;
            if let Err(error) = xhci.configure_hub_slot(hub_slot_id, num_ports, speed_id) {
                log::warn!("USB: hub slot configuration failed: {}", error);
                // Continue anyway — some controllers work without the Hub flag.
            }
        }

        // Power on all hub ports (needed for some hubs).
        {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            for port in 1..=num_ports {
                let setup = super::UsbSetupPacket {
                    bm_request_type: 0x23, // H2D, class, other
                    b_request: super::HUB_REQ_SET_FEATURE,
                    w_value: super::HUB_PORT_POWER,
                    w_index: port as u16,
                    w_length: 0,
                };
                let _ = HostController::control_transfer(xhci, hub_addr, &setup, &mut []);
            }
        }
        super::xhci::port::delay_ms(50);

        let mut found_any = false;

        for port in 1..=num_ports {
            let port_status = match self.hub_get_port_status(ctrl_idx, hub_addr, port) {
                Some(status) => status,
                None => continue,
            };

            if port_status & super::HUB_PORT_STATUS_CONNECTION == 0 {
                continue;
            }

            log::info!(
                "USB: hub port {} connected (status={:#06x}), resetting",
                port,
                port_status
            );

            // Clear any pending connection change.
            self.hub_clear_port_feature(ctrl_idx, hub_addr, port, super::HUB_C_PORT_CONNECTION);

            // Issue port reset.
            self.hub_set_port_feature(ctrl_idx, hub_addr, port, super::HUB_PORT_RESET);

            // Wait for reset to complete (USB 2.0: ~50ms, allow 200ms).
            super::xhci::port::delay_ms(200);

            // Clear C_PORT_RESET.
            self.hub_clear_port_feature(ctrl_idx, hub_addr, port, super::HUB_C_PORT_RESET);

            // Re-read port status to get the post-reset speed.
            let post_status = match self.hub_get_port_status(ctrl_idx, hub_addr, port) {
                Some(status) => status,
                None => continue,
            };

            if post_status & super::HUB_PORT_STATUS_ENABLE == 0 {
                log::warn!("USB: hub port {} not enabled after reset", port);
                continue;
            }

            // Map hub port speed bits to xHCI speed ID.
            let speed_id: u8 = if post_status & super::HUB_PORT_STATUS_HIGH_SPEED != 0 {
                3 // High
            } else if post_status & super::HUB_PORT_STATUS_LOW_SPEED != 0 {
                2 // Low
            } else {
                1 // Full
            };

            log::info!("USB: hub port {} enabled, speed_id={}", port, speed_id);

            // Allocate a slot for the child device.
            let child_slot = {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                match xhci.enable_slot() {
                    Ok(id) => id,
                    Err(error) => {
                        log::warn!("USB: hub port {} Enable Slot failed: {}", port, error);
                        continue;
                    }
                }
            };

            // Address the child device with the hub as parent.
            {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                if let Err(error) = xhci.address_device_behind_hub(
                    child_slot,
                    root_port as u8,
                    port,
                    speed_id,
                    hub_slot_id,
                ) {
                    log::warn!("USB: hub port {} Address Device failed: {}", port, error);
                    let _ = xhci.disable_slot(child_slot);
                    continue;
                }
            }

            let child_addr = child_slot as u8;

            // Add a placeholder entry so enumerate_mass_storage can update it.
            {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                xhci.devices.push(super::UsbDevice {
                    address: child_addr,
                    speed: super::UsbSpeed::High,
                    max_packet_size_0: 64,
                    vendor_id: 0,
                    product_id: 0,
                    device_class: 0,
                    device_subclass: 0,
                    device_protocol: 0,
                    configurations: 0,
                    endpoints: alloc::vec::Vec::new(),
                    port_index: root_port as u32 - 1,
                    parent_hub_slot: Some(hub_slot_id),
                    downstream_port: Some(port),
                });
            }
            let child_dev_idx = {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                xhci.devices().len() - 1
            };

            // Try mass-storage enumeration on the child device.
            let msc_result = {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                super::usb_bus::enumerate_mass_storage(
                    xhci as &mut dyn HostController,
                    child_addr,
                    child_dev_idx,
                )
            };

            let (ep_out, ep_out_mps, ep_in, ep_in_mps) = match msc_result {
                Ok(v) => v,
                Err(error) => {
                    log::info!("USB: hub port {} not mass storage: {}", port, error);
                    let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                    let _ = xhci.disable_slot(child_slot);
                    // Remove the placeholder.
                    xhci.remove_device_candidate(child_dev_idx);
                    continue;
                }
            };

            // Configure bulk endpoints.
            {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                if xhci
                    .configure_endpoint_bulk(child_slot, ep_out, ep_out_mps)
                    .is_err()
                {
                    log::warn!("USB: hub port {} bulk OUT config failed", port);
                    let _ = xhci.disable_slot(child_slot);
                    xhci.remove_device_candidate(child_dev_idx);
                    continue;
                }
                if xhci
                    .configure_endpoint_bulk(child_slot, ep_in, ep_in_mps)
                    .is_err()
                {
                    log::warn!("USB: hub port {} bulk IN config failed", port);
                    let _ = xhci.disable_slot(child_slot);
                    xhci.remove_device_candidate(child_dev_idx);
                    continue;
                }
            }

            // Read capacity.
            let (block_size, total_blocks) = {
                let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
                let mut tag = 1;
                match super::usb_bus::bot_read_capacity(
                    xhci, child_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, &mut tag,
                ) {
                    Ok(capacity) => capacity,
                    Err(error) => {
                        log::warn!("USB: hub port {} READ CAPACITY failed: {}", port, error);
                        let _ = xhci.disable_slot(child_slot);
                        xhci.remove_device_candidate(child_dev_idx);
                        continue;
                    }
                }
            };

            // Register the disk.
            let registered = self.storage.try_register(Disk {
                dev_addr: child_addr,
                ep_out,
                ep_out_mps,
                ep_in,
                ep_in_mps,
                block_size,
                total_blocks,
                ctrl_type: "xHCI",
                ctrl_idx,
            });
            if registered {
                found_any = true;
                log::info!(
                    "USB: xHCI hub mass-storage device ready ctrl={} hub_slot={} port={} child_slot={} block_size={} total_blocks={}",
                    ctrl_idx,
                    hub_slot_id,
                    port,
                    child_slot,
                    block_size,
                    total_blocks,
                );
            }
        }

        // Mark the hub port as done so poll_ports doesn't retry it.
        {
            let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
            if let Some(port) = xhci.ports.get_mut(root_port as u32 - 1) {
                port.done = true;
            }
        }

        found_any
    }

    /// Send a Get Port Status request to a hub and return wPortStatus (low word).
    fn hub_get_port_status(&mut self, ctrl_idx: usize, hub_addr: u8, port: u8) -> Option<u16> {
        let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
        let mut buf = [0u8; 4];
        let setup = super::UsbSetupPacket {
            bm_request_type: 0xA3, // D2H, class, other
            b_request: super::HUB_REQ_GET_STATUS,
            w_value: 0,
            w_index: port as u16,
            w_length: 4,
        };
        match HostController::control_transfer(xhci, hub_addr, &setup, &mut buf) {
            Ok(4) => Some(u16::from_le_bytes([buf[0], buf[1]])),
            Ok(len) => {
                log::warn!("USB: hub port {} status short read: {} bytes", port, len);
                None
            }
            Err(error) => {
                log::warn!("USB: hub port {} GetPortStatus failed: {}", port, error);
                None
            }
        }
    }

    /// Send a Set Port Feature request to a hub.
    fn hub_set_port_feature(&mut self, ctrl_idx: usize, hub_addr: u8, port: u8, feature: u16) {
        let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
        let setup = super::UsbSetupPacket {
            bm_request_type: 0x23, // H2D, class, other
            b_request: super::HUB_REQ_SET_FEATURE,
            w_value: feature,
            w_index: port as u16,
            w_length: 0,
        };
        if let Err(error) = HostController::control_transfer(xhci, hub_addr, &setup, &mut []) {
            log::warn!(
                "USB: hub port {} SetFeature({}) failed: {}",
                port,
                feature,
                error
            );
        }
    }

    /// Send a Clear Port Feature request to a hub.
    fn hub_clear_port_feature(&mut self, ctrl_idx: usize, hub_addr: u8, port: u8, feature: u16) {
        let xhci: &mut XhciContext = &mut *self.controllers.xhci[ctrl_idx];
        let setup = super::UsbSetupPacket {
            bm_request_type: 0x23, // H2D, class, other
            b_request: super::HUB_REQ_CLEAR_FEATURE,
            w_value: feature,
            w_index: port as u16,
            w_length: 0,
        };
        if let Err(error) = HostController::control_transfer(xhci, hub_addr, &setup, &mut []) {
            log::warn!(
                "USB: hub port {} ClearFeature({}) failed: {}",
                port,
                feature,
                error
            );
        }
    }
}
