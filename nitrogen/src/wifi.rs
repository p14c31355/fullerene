//! PCI-probe-based WiFi driver selection.
//!
//! # Architecture
//!
//! ```text
//! WifiRegistry::probe()          ← safe PCI config-space scan
//!   ↓
//! (vendor_id, device_id) → lookup in DRIVER_TABLE
//!   ↓
//! DriverEntry::create()           ← MMIO init (with timeout)
//!   ↓
//! Box<dyn WifiDriver>
//! ```
//!
//! The probe phase only touches PCI configuration space (port I/O),
//! so it can never hang.  The init phase touches MMIO registers and
//! loads firmware, with TSC-based timeouts to prevent indefinite hangs
//! on unsupported or unresponsive hardware.

use alloc::boxed::Box;
use alloc::vec::Vec;

use bonder::wifi::{Ssid, AccessPoint, WifiStatus};
use crate::pci::PciScanner;
use crate::DriverContext;

// ── WifiDriver trait ─────────────────────────────────────────────────

/// Abstract WiFi driver interface.
///
/// Each supported chipset implements this trait.  The [`WifiRegistry`]
/// probes PCI, matches a [`DriverEntry`], and calls [`WifiDriver::create`]
/// to produce a boxed driver instance.
pub trait WifiDriver: Send {
    /// Initialise the device: map MMIO, reset, load firmware, wait for alive.
    /// Returns `None` if initialisation times out or the device is unresponsive.
    fn create(
        ctx: &'static dyn DriverContext,
        mmio_base: *mut u32,
        hw_rev: u32,
        device: crate::pci::PciDevice,
    ) -> Option<Box<dyn WifiDriver>>
    where
        Self: Sized;

    /// Periodic tick (poll TX completions, RX frames, link state).
    fn tick(&mut self);

    /// Return the current link / firmware status.
    fn get_status(&self) -> WifiStatus;

    /// Initiate a scan.  Results are delivered asynchronously via
    /// [`get_scan_results`].
    fn start_scan(&mut self) -> bool;

    /// Collect buffered scan results.
    fn get_scan_results(&self) -> Vec<AccessPoint>;

    /// Connect to an AP.
    fn connect(&mut self, ssid: &Ssid, psk: Option<&str>) -> bool;

    /// Disconnect.
    fn disconnect(&mut self);

    /// Whether a device is available and operational.
    fn device_available(&self) -> bool;

    /// Current connected SSID, if any.
    fn connected_ssid(&self) -> Option<&Ssid>;

    /// IP address assigned via DHCP.
    fn ip_address(&self) -> [u8; 4];

    /// Load firmware blob into the device.
    /// Returns `Ok(())` on success.
    fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str>;

    /// Start firmware upload and CPU boot without waiting for alive.
    /// Used by the step-based init to avoid blocking the render loop.
    fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str>;

    /// Non-blocking check if firmware has signaled alive.
    /// Returns Ok(true) if alive, Ok(false) if still waiting, Err on error/timeout.
    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, &'static str>;
}

// ── Hardware info (from PCI config space, always safe) ───────────────

/// Information collected from PCI config space during the probe phase.
/// No MMIO or device-side effects — safe to call on any device.
#[derive(Debug, Clone)]
pub struct PciWifiInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision: u8,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub bar0_phys: u64,          // from PCI BAR0
    pub bar0_size: usize,        // from PCI BAR0
    pub subsystem_vendor: u16,   // from config offset 0x2C
    pub subsystem_device: u16,   // from config offset 0x2E
}

// ── Driver entry in the registry ─────────────────────────────────────

/// Type-erased constructor: given the driver context, MMIO base, and HW
/// revision, returns a boxed driver or `None` on failure.
type DriverCtor = fn(&'static dyn DriverContext, *mut u32, u32, crate::pci::PciDevice) -> Option<Box<dyn WifiDriver>>;

/// A single entry in the WiFi driver registry.
pub struct DriverEntry {
    /// PCI vendor ID (e.g. 0x8086 for Intel).
    pub vendor: u16,
    /// PCI device IDs supported by this driver.
    pub devices: &'static [u16],
    /// Human-readable name (e.g. "Intel 7265").
    pub name: &'static str,
    /// Constructor.
    pub create: DriverCtor,
}

// ── Driver registry ─────────────────────────────────────────────────

/// Registry of all known WiFi drivers, ordered by preference.
///
/// The first matching entry is used.  This struct is zero-sized by design;
/// all data lives in the static table.
pub struct WifiRegistry;

impl WifiRegistry {
    /// Probe PCI for a WiFi controller, match against the registry,
    /// and return the best driver candidate.
    ///
    /// # Safety
    ///
    /// Only reads PCI configuration space — no MMIO, no DMA.
    /// Returns `None` gracefully if no supported card is found.
    pub fn probe() -> Option<(&'static DriverEntry, PciWifiInfo)> {
        crate::debug::print("wifi", "probe_registry_enter");
        let mut scanner = PciScanner::new();
        crate::debug::print("wifi", "probe_scan_start");
        let _ = scanner.scan_all_buses();
        crate::debug::print("wifi", "probe_scan_done");

        for device in scanner.get_devices() {
            // Network controller (class 0x02), wireless (subclass 0x80)
            if device.class_code != 0x02 || device.subclass != 0x80 {
                continue;
            }

            let info = PciWifiInfo {
                vendor_id: device.vendor_id,
                device_id: device.device_id,
                revision: 0,
                bus: device.bus,
                device: device.device,
                function: device.function,
                bar0_phys: 0,
                bar0_size: 0,
                subsystem_vendor: 0,
                subsystem_device: 0,
            };

            // Try each driver in the registry
            for entry in DRIVER_TABLE {
                if device.vendor_id != entry.vendor {
                    continue;
                }
                if !entry.devices.contains(&device.device_id) {
                    continue;
                }

                // Read BAR0 using the robust PciDevice API
                let bar = device.get_bar_info(0)?;
                let phys = bar.address;
                let size = bar.size as usize;

                // Read subsystem IDs
                let subsys = crate::pci::PciConfigSpace::read_config_dword(
                    device.bus, device.device, device.function, 0x2C,
                );

                let final_info = PciWifiInfo {
                    subsystem_vendor: (subsys & 0xFFFF) as u16,
                    subsystem_device: (subsys >> 16) as u16,
                    bar0_phys: phys,
                    bar0_size: size,
                    ..info
                };

                return Some((entry, final_info));
            }
        }

        None
    }

}

// ── Driver table ────────────────────────────────────────────────────

/// All supported WiFi chipsets.  The first matching entry wins.
pub static DRIVER_TABLE: &[DriverEntry] = &[
    DriverEntry {
        vendor: 0x8086,
        devices: &[0x095b, 0x095a, 0x08b1, 0x08b2],
        name: "Intel 7265",
        create: super::iwlwifi::try_create_iwl,
    },
];

// ── Public API ──────────────────────────────────────────────────────

/// Result of PCI probe + driver creation, carrying back the hardware
/// info needed for firmware selection.
pub struct PciProbeResult {
    pub driver: Box<dyn WifiDriver>,
    pub device_id: u16,
    pub hw_rev: u32,
}

/// Probe PCI, find a supported WiFi card, and initialise it.
///
/// Returns the initialised driver together with PCI device ID and HW
/// revision on success, or `None` if no supported card is found or
/// initialisation fails.
pub fn init_wifi_from_pci(ctx: &'static dyn DriverContext) -> Option<PciProbeResult> {
    crate::debug::print("wifi", "start_pci_probe");
    let (entry, info) = WifiRegistry::probe()?;

    crate::debug::print("wifi", "probe_ok");

    // ── PCI config-space setup (NEVER hangs) ────────────────────────
    // On real hardware, the device may be in D3 or have ASPM L1 enabled,
    // either of which will cause ANY MMIO access (including the HW_REV
    // read below) to hang the CPU indefinitely.  We must ensure D0,
    // disable ASPM, and enable memory-space decoding *before* touching
    // the BAR — all via PCI config space (port I/O, safe).
    let pci_dev = crate::pci::PciDevice::new(info.bus, info.device, info.function)?;
    crate::debug::print("wifi", "ensure_d0");
    pci_dev.ensure_d0();
    crate::debug::print("wifi", "disable_aspm");
    pci_dev.disable_pcie_aspm();
    crate::debug::print("wifi", "enable_mem");
    pci_dev.enable_memory_access();

    // ── Read HW revision from PCI config space (port I/O, NEVER hangs) ─
    // Instead of a non-posted MMIO read_volatile(CSR_HW_REV) which can
    // hang the CPU forever on real hardware when the device is not fully
    // responding, we use the PCI Revision ID at config offset 0x08.
    // This is a port-I/O read, not MMIO, so it cannot hang.
    crate::debug::print("wifi", "read_hw_rev_pci");
    let pci_revision = crate::pci::PciConfigSpace::read_config_byte(
        info.bus, info.device, info.function, 0x08,
    );
    // Convert PCI revision (u8) to a u32 hw_rev for the driver API.
    // The driver only uses hw_rev for logging; the actual HW revision
    // is re-read from CSR_HW_REV inside init_after_mmio() when (and if)
    // the device becomes responsive.
    let hw_rev: u32 = pci_revision as u32;

    // Map BAR0 MMIO
    crate::debug::print("wifi", "map_bar0");
    let mmio_virt = ctx.phys_to_virt(info.bar0_phys);
    if ctx.map_mmio_region(info.bar0_phys as usize, mmio_virt, info.bar0_size).is_err() {
        return None;
    }

    // Sanity-check: verify the device is still present before any MMIO
    let mmio_base = mmio_virt as *mut u32;
    crate::debug::print("wifi", "check_device_present");
    {
        let vendor = crate::pci::PciConfigSpace::read_config_word(
            info.bus, info.device, info.function, 0,
        );
        if vendor == 0xFFFF || vendor == 0x0000 || vendor != info.vendor_id {
            crate::debug::print("wifi", "ERR device_gone_before_mmio");
            return None;
        }
    }

    crate::debug::print("wifi", "driver_create");
    // Let the matched driver create itself
    let driver = (entry.create)(ctx, mmio_base, hw_rev, pci_dev)?;

    crate::debug::print("wifi", "driver_ok");
    Some(PciProbeResult {
        driver,
        device_id: info.device_id,
        hw_rev,
    })
}
