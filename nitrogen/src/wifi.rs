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

use crate::DriverContext;
use crate::pci::PciScanner;
use crate::pci_health::PciHealth;
use bonder::wifi::{AccessPoint, Ssid, WifiStatus};

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

    /// Send post-boot init commands (TX antenna config, RXON, queue setup).
    /// Called by the step-based init after firmware alive is confirmed.
    fn send_init_commands(&mut self) -> Result<(), &'static str>;
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
    pub bar0_phys: u64,        // from PCI BAR0
    pub bar0_size: usize,      // from PCI BAR0
    pub subsystem_vendor: u16, // from config offset 0x2C
    pub subsystem_device: u16, // from config offset 0x2E
}

// ── Driver entry in the registry ─────────────────────────────────────

/// Type-erased constructor: given the driver context, MMIO base, and HW
/// revision, returns a boxed driver or `None` on failure.
type DriverCtor = fn(
    &'static dyn DriverContext,
    *mut u32,
    u32,
    crate::pci::PciDevice,
) -> Option<Box<dyn WifiDriver>>;

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
                    device.bus,
                    device.device,
                    device.function,
                    0x2C,
                );
                let subsys_vendor = (subsys & 0xFFFF) as u16;
                let subsys_device = (subsys >> 16) as u16;

                // ── Phantom / stub device filter ──
                // Some BIOSes leave PCI config-space entries for
                // unpopulated M.2 slots (vendor=8086, device=095b
                // but subsys=0000:0000 or FFFF:FFFF).  A non-posted
                // MMIO read to such a "device" hangs the CPU forever.
                if subsys_vendor == 0x0000 || subsys_vendor == 0xFFFF {
                    log::warn!(
                        "WiFi: ignoring phantom device {:04x}:{:04x} subsys={:04x}:{:04x} at {:02x}:{:02x}.{}",
                        device.vendor_id,
                        device.device_id,
                        subsys_vendor,
                        subsys_device,
                        device.bus,
                        device.device,
                        device.function,
                    );
                    continue;
                }
                // BAR0 must be a reasonable MMIO region (not 0, not > 64 MiB)
                if phys == 0 || size == 0 || size > 0x0400_0000 {
                    log::warn!(
                        "WiFi: ignoring device with invalid BAR0 phys={:#x} size={:#x}",
                        phys,
                        size,
                    );
                    continue;
                }

                let final_info = PciWifiInfo {
                    subsystem_vendor: subsys_vendor,
                    subsystem_device: subsys_device,
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
    #[cfg(not(nitrogen_no_iwlwifi))]
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

/// Raw result of a lightweight PCI probe (without driver creation).
/// Used by the step-based init to accumulate state across phases.
pub struct RawPciProbeResult {
    pub entry: &'static DriverEntry,
    pub pci_dev: crate::pci::PciDevice,
    pub mmio: *mut u32,
    pub hw_rev: u16,
    pub device_id: u16,
    pub driver_ctx: &'static dyn DriverContext,
    /// Upstream PCIe bridge coordinates (if found) for ASPM control.
    pub upstream_bridge: Option<(u8, u8, u8)>,
}

/// Lightweight PCI probe: scan bus, configure D0/ASPM/enable-mem,
/// map BAR0.  No MMIO access — only PCI config space (port I/O).
/// Returns raw state that can be used by subsequent init phases.
pub fn probe_pci_only(ctx: &'static dyn DriverContext) -> Option<RawPciProbeResult> {
    crate::debug::print("wifi", "start_pci_probe");
    let (entry, info) = WifiRegistry::probe()?;
    crate::debug::print("wifi", "probe_ok");

    // PCI config-space setup (NEVER hangs).
    // Order is critical: configure the upstream bridge BEFORE touching
    // the endpoint to avoid ASPM state mismatch on the PCIe link.
    let pci_dev = crate::pci::PciDevice::new(info.bus, info.device, info.function)?;

    // ── Find upstream PCIe bridge and disable its ASPM first ──
    crate::debug::print("wifi", "scan_bridge");
    let upstream_bridge: Option<(u8, u8, u8)> = {
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();
        let upstream = scanner.get_devices().iter().find(|bridge| {
            bridge.class_code == 0x06
                && bridge.subclass == 0x04
                && crate::pci::PciConfigSpace::read_config_byte(
                    bridge.bus,
                    bridge.device,
                    bridge.function,
                    0x19,
                ) == info.bus
        });
        if let Some(up) = upstream {
            log::info!(
                "WiFi: found upstream bridge {:02x}:{:02x}.{}",
                up.bus,
                up.device,
                up.function
            );
            Some((up.bus, up.device, up.function))
        } else {
            None
        }
    };

    // ── Minimal endpoint configuration ─────────────────────
    // Linux' iwlwifi calls only pci_enable_device() (which sets
    // the Memory Space + Bus Master bits in the Command register)
    // before accessing BAR0 MMIO.  Any additional config writes
    // (ASPM disable, D0 re-assertion, CTO) can cause the PCIe
    // link to enter an inconsistent state on this platform.
    crate::debug::print("wifi", "enable_mem");
    pci_dev.enable_memory_access();

    // Read HW revision from PCI config space (port I/O, NEVER hangs)
    crate::debug::print("wifi", "read_hw_rev_pci");
    let pci_revision =
        crate::pci::PciConfigSpace::read_config_byte(info.bus, info.device, info.function, 0x08);
    let hw_rev: u16 = pci_revision as u16;

    // Map BAR0 MMIO
    crate::debug::print("wifi", "map_bar0");
    let mmio_virt = ctx.phys_to_virt(info.bar0_phys);
    if ctx
        .map_mmio_region(info.bar0_phys as usize, mmio_virt, info.bar0_size)
        .is_err()
    {
        return None;
    }
    let mmio_base = mmio_virt as *mut u32;

    // Sanity-check: verify the device is still present before any MMIO
    crate::debug::print("wifi", "check_device_present");
    {
        let vendor =
            crate::pci::PciConfigSpace::read_config_word(info.bus, info.device, info.function, 0);
        if vendor == 0xFFFF || vendor == 0x0000 || vendor != info.vendor_id {
            crate::debug::print("wifi", "ERR device_gone_before_mmio");
            return None;
        }
    }

    // ── Final sanity: full PciHealth check ──
    // Even with valid vendor/device/subsystem IDs and a non-zero BAR0,
    // the device may be a BIOS stub with no actual hardware behind it.
    // A non-posted MMIO read to such a device hangs the CPU forever.
    // PciHealth::check() verifies D0 state and PCIe link status via
    // PCI config space (port I/O, never hangs).  If the link reports
    // speed=0, the physical device is absent — bail out immediately.
    crate::debug::print("wifi", "pci_health_check");
    {
        let mut health = if let Some((bus, dev, func)) = upstream_bridge {
            PciHealth::new(&pci_dev).with_upstream_bridge(bus, dev, func)
        } else {
            PciHealth::new(&pci_dev)
        };
        if health.pre_mmio_access().is_err() {
            log::warn!(
                "WiFi: device {:04x}:{:04x} at {:02x}:{:02x}.{} failed PciHealth check — \
                 not in D0 or PCIe link down (phantom device?)",
                info.vendor_id,
                info.device_id,
                info.bus,
                info.device,
                info.function,
            );
            crate::debug::print("wifi", "ERR health_check_failed");
            return None;
        }
    }

    // ── Log whether this is a real WiFi card or a phantom device ──
    // Some machines have a PCI device at the expected BDF that reports
    // the right vendor/device IDs but has no actual hardware behind it
    // (e.g. BIOS-configured stub, unpopulated M.2 slot with ASPM enabled).
    // We log the subsystem IDs so we can distinguish a real card from a
    // phantom entry on buggy firmware.
    let subsys =
        crate::pci::PciConfigSpace::read_config_dword(info.bus, info.device, info.function, 0x2C);
    log::info!(
        "WiFi: probe_pci_only done — vendor={:#06x} device={:#06x} subsys={:#010x} bus={:02x}:{:02x}.{}",
        info.vendor_id,
        info.device_id,
        subsys,
        info.bus,
        info.device,
        info.function,
    );
    crate::debug::print("wifi", "probe_done");
    Some(RawPciProbeResult {
        entry,
        pci_dev,
        mmio: mmio_base,
        hw_rev,
        device_id: info.device_id,
        driver_ctx: ctx,
        upstream_bridge,
    })
}

/// Probe PCI, find a supported WiFi card, and initialise it.
///
/// Returns the initialised driver together with PCI device ID and HW
/// revision on success, or `None` if no supported card is found or
/// initialisation fails.
pub fn init_wifi_from_pci(ctx: &'static dyn DriverContext) -> Option<PciProbeResult> {
    let raw = probe_pci_only(ctx)?;

    let hw_rev_32 = raw.hw_rev as u32;
    crate::debug::print("wifi", "driver_create");
    let driver = (raw.entry.create)(ctx, raw.mmio, hw_rev_32, raw.pci_dev)?;

    crate::debug::print("wifi", "driver_ok");
    Some(PciProbeResult {
        driver,
        device_id: raw.device_id,
        hw_rev: hw_rev_32,
    })
}
