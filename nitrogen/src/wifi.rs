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
type DriverCtor = fn(&'static dyn DriverContext, *mut u32, u32) -> Option<Box<dyn WifiDriver>>;

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
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();

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

                // Read BAR0 (safe — PCI config space)
                let bar_val = crate::pci::PciConfigSpace::read_config_dword(
                    device.bus, device.device, device.function, 0x10,
                );
                let bar_upper = crate::pci::PciConfigSpace::read_config_dword(
                    device.bus, device.device, device.function, 0x14,
                );
                let (phys, size) = Self::decode_bar(bar_val, bar_upper);

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

    /// Decode a 64-bit MMIO BAR from its two config-space dwords.
    fn decode_bar(low: u32, high: u32) -> (u64, usize) {
        let base = ((high as u64) << 32) | (low as u64 & 0xFFFF_FFF0);
        // BAR size detection by writing 0xFFFF_FFFF and reading back
        // is normally done here, but we compute from the size hint
        // or assume a safe default for now.
        (base, 0x2000) // default 8KB
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
    let (entry, info) = WifiRegistry::probe()?;

    // Map BAR0 MMIO
    let mmio_virt = ctx.phys_to_virt(info.bar0_phys);
    if ctx.map_mmio_region(info.bar0_phys as usize, mmio_virt, info.bar0_size).is_err() {
        return None;
    }

    // Read HW revision (first MMIO touch — safe on supported hardware,
    // but we already matched via PCI ID so this should work)
    let mmio_base = mmio_virt as *mut u32;
    let hw_rev = unsafe { core::ptr::read_volatile(mmio_base.add(0x028 / 4)) };

    if hw_rev == 0 || hw_rev == 0xFFFF_FFFF {
        return None;
    }

    // Let the matched driver create itself
    let driver = (entry.create)(ctx, mmio_base, hw_rev)?;

    Some(PciProbeResult {
        driver,
        device_id: info.device_id,
        hw_rev,
    })
}
