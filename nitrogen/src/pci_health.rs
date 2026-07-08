//! PCIe device health monitoring and automatic recovery.
//!
//! # Real-hardware failure scenarios
//!
//! | Scenario | Root cause | Detection | Recovery |
//! |----------|------------|-----------|----------|
//! | Device in D3hot | BIOS/firmware left device in low-power | PMCSR read | `ensure_d0()` |
//! | PCIe link down (L1 substate) | ASPM + buggy bridge | Link Status read | `disable_aspm()` + re-train |
//! | Device disappeared | Hot-removed, power loss | Vendor=0xFFFF | Report absent |
//! | Non-posted read hang | Read to unresponsive device | Config space probe | Skip MMIO, use PIO fallback |
//! | Surprise link down | Transient electrical noise | Config retry | Retry with backoff |

use crate::pci::{PciConfigSpace, PciDevice};

/// Error type for PCI health operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciHealthError {
    DeviceGone,       // vendor=0xFFFF
    NotD0,            // power state is D1-D3hot
    LinkDown,         // PCIe link status shows speed=0
    CapCycle,         // capability list has a cycle
    NoPmCap,          // Power Management capability not found
    NoPcieCap,        // PCI Express capability not found
    MmioHangRisk,     // cannot safely issue non-posted MMIO read
}

impl core::fmt::Display for PciHealthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PciHealthError::DeviceGone => write!(f, "device not on PCI bus"),
            PciHealthError::NotD0 => write!(f, "device not in D0"),
            PciHealthError::LinkDown => write!(f, "PCIe link down"),
            PciHealthError::CapCycle => write!(f, "capability list cycle detected"),
            PciHealthError::NoPmCap => write!(f, "Power Management cap not found"),
            PciHealthError::NoPcieCap => write!(f, "PCI Express cap not found"),
            PciHealthError::MmioHangRisk => write!(f, "MMIO read would likely hang"),
        }
    }
}

/// Tracks PCIe device health across driver lifecycles.
///
/// # Design
///
/// - All checks go through PCI config space (port I/O, never hangs).
/// - Health state is cached and lazily refreshed.
/// - `pre_mmio_access()` must be called before every MMIO transaction
///   cycle to verify the device is safe to access.
pub struct PciHealth {
    /// BDF coordinates cached from the PciDevice.
    bus: u8,
    dev: u8,
    func: u8,
    vendor_id: u16,
    #[allow(dead_code)]
    device_id: u16,
    /// Upstream bridge coordinates for ASPM control.
    upstream_bridge: Option<(u8, u8, u8)>,
    // ── Health cache ──
    aspm_disabled: bool,
    /// Timestamp (TSC ticks) of last successful health check.
    last_check_ok: u64,
}

impl PciHealth {
    /// Create a new health monitor for a PCI device.
    ///
    /// Does NOT issue any PCI config space access (safe for early boot).
    pub fn new(device: &PciDevice) -> Self {
        Self {
            bus: device.bus,
            dev: device.device,
            func: device.function,
            vendor_id: device.vendor_id,
            device_id: device.device_id,
            upstream_bridge: None,
            aspm_disabled: false,
            last_check_ok: 0,
        }
    }

    pub fn with_upstream_bridge(mut self, bus: u8, dev: u8, func: u8) -> Self {
        self.upstream_bridge = Some((bus, dev, func));
        self
    }

    /// Quick check: is the device still visible on the PCI bus?
    /// This is a single config read — safe and fast.
    pub fn is_device_present(&self) -> bool {
        let vendor = PciConfigSpace::read_config_word(self.bus, self.dev, self.func, 0);
        vendor != 0xFFFF && vendor != 0x0000 && vendor == self.vendor_id
    }

    /// Full health check: vendor, D0, link status.
    pub fn check(&mut self) -> Result<(), PciHealthError> {
        // 1. Vendor check (device must be on the bus)
        let vendor = PciConfigSpace::read_config_word(self.bus, self.dev, self.func, 0);
        if vendor == 0xFFFF || vendor == 0x0000 || vendor != self.vendor_id {
            return Err(PciHealthError::DeviceGone);
        }

        // 2. Walk capabilities to find PM (0x01) and PCIe (0x10)
        let cap_ptr = PciConfigSpace::read_config_byte(self.bus, self.dev, self.func, 0x34);
        if cap_ptr == 0 {
            return Err(PciHealthError::NoPmCap);
        }

        let mut off = cap_ptr;
        let mut found_pm = false;
        let mut found_pcie = false;
        let mut visited = [false; 256];

        for _ in 0..48 {
            if off < 0x40 || off > 0xED {
                break;
            }
            if visited[off as usize] {
                // Capability list cycle — log and break instead of fatal
                // error, since the device may still be usable.
                log::warn!("PCI health: capability list cycle at offset {:#x}", off);
                break;
            }
            visited[off as usize] = true;

            match PciConfigSpace::read_config_byte(self.bus, self.dev, self.func, off) {
                0x01 => {
                    found_pm = true;
                    let pmcsr =
                        PciConfigSpace::read_config_word(self.bus, self.dev, self.func, off + 4);
                    let pstate = pmcsr & 0x3;
                    if pstate != 0 {
                        return Err(PciHealthError::NotD0);
                    }
                }
                0x10 => {
                    found_pcie = true;
                    let lnk_sts = PciConfigSpace::read_config_word(
                        self.bus, self.dev, self.func, off + 0x12,
                    );
                    let speed = lnk_sts & 0xF;
                    if speed == 0 {
                        return Err(PciHealthError::LinkDown);
                    }
                }
                _ => {}
            }

            if found_pm && found_pcie {
                break;
            }

            let next = PciConfigSpace::read_config_byte(self.bus, self.dev, self.func, off + 1);
            if next == 0 || next == off {
                break;
            }
            off = next;
        }

        if !found_pm {
            return Err(PciHealthError::NoPmCap);
        }
        if !found_pcie {
            return Err(PciHealthError::NoPcieCap);
        }

        self.last_check_ok = 0; // Would use RDTSC in practice
        Ok(())
    }

    /// Ensure D0, retrain the upstream link, then disable ASPM.
    ///
    /// This is the last line of defence before a non-posted MMIO read.
    /// On real hardware the PCIe link may be stuck in L1 — retraining
    /// the link forces it back to L0 so the endpoint can complete MMIO
    /// reads.
    ///
    /// # Ordering
    ///
    /// 1. `ensure_d0()` on the endpoint and bridge (config space, safe)
    /// 2. **Link retrain** on the upstream bridge — brings the link to L0
    /// 3. `disable_aspm()` on the bridge (ECAM reads to bridge are safe
    ///    because they reach the bridge directly, not through the endpoint
    ///    link)
    /// 4. `disable_aspm()` on the endpoint — now safe because the link
    ///    is in L0 and ECAM reads to the endpoint will complete
    /// 5. `check()` — full health verification via config space
    ///
    /// The critical change from the previous ordering: the link retrain
    /// now happens BEFORE the endpoint ASPM disable, because the endpoint
    /// ASPM disable requires an ECAM MMIO read (for L1Sub) which can hang
    /// if the link is in L1.
    pub fn recover(&mut self) -> Result<(), PciHealthError> {
        // Step 1: Re-assert D0 on the device and bridge
        self.ensure_d0();
        if let Some((b, d, f)) = self.upstream_bridge {
            if let Some(bridge) = PciDevice::new(b, d, f) {
                bridge.ensure_d0();
            }
        }

        // Step 2: Retrain the upstream link BEFORE any endpoint ECAM access.
        // This forces the link out of L1 so subsequent non-posted reads
        // (ECAM or BAR MMIO) will complete instead of hanging.
        let link_retrained = self.retrain_upstream_link();

        // Step 3: Disable ASPM on the upstream bridge (ECAM to bridge is safe)
        if let Some((b, d, f)) = self.upstream_bridge {
            if let Some(bridge) = PciDevice::new(b, d, f) {
                bridge.disable_pcie_aspm();
            }
        }

        // Step 4: Now safe to disable ASPM on the endpoint (link is L0)
        if link_retrained {
            // After link retrain, give the link extra time to stabilise
            // before hitting the endpoint with ECAM reads.
            crate::timing::delay_us(5_000);
        }
        self.disable_aspm();

        // Step 5: Re-verify
        self.check()
    }

    /// Retrain the upstream bridge link. Returns true if retrain was attempted.
    /// All accesses are via PCI config space (port I/O), never hang.
    fn retrain_upstream_link(&self) -> bool {
        let (b, d, f) = match self.upstream_bridge {
            Some(x) => x,
            None => return false,
        };
        // Verify the bridge exists via config space (port I/O, safe)
        if PciConfigSpace::read_config_word(b, d, f, 0) == 0xFFFF {
            return false;
        }
        let cap_ptr = PciConfigSpace::read_config_byte(b, d, f, 0x34);
        let mut off = cap_ptr;
        let mut lnk_ctl = None;
        let mut visited = [false; 256];
        for _ in 0..48 {
            if off < 0x40 || off > 0xF8 { break; }
            if visited[off as usize] { break; }
            visited[off as usize] = true;
            let cap_id = PciConfigSpace::read_config_byte(b, d, f, off);
            if cap_id == 0x10 {
                lnk_ctl = Some(off + 0x10);
                break;
            }
            let next = PciConfigSpace::read_config_byte(b, d, f, off + 1);
            if next == 0 || next == off { break; }
            off = next;
        }
        if let Some(lnk_off) = lnk_ctl {
            let ctl = PciConfigSpace::read_config_word(b, d, f, lnk_off);
            PciConfigSpace::write_config_word_raw(
                b, d, f, lnk_off,
                ctl | (1 << 5), // Set Link Retrain
            );
            log::info!(
                "PciHealth: link retrain on bridge {:02x}:{:02x}.{}",
                b, d, f,
            );
            crate::timing::delay_us(10_000);
            true
        } else {
            false
        }
    }

    /// Ensure the device is in D0 power state.
    fn ensure_d0(&self) {
        if let Some(dev) = PciDevice::new(self.bus, self.dev, self.func) {
            dev.ensure_d0();
        }
    }

    /// Disable ASPM on this device.
    fn disable_aspm(&self) {
        if let Some(dev) = PciDevice::new(self.bus, self.dev, self.func) {
            dev.disable_pcie_aspm();
        }
    }

    /// Pre-MMIO access check.
    ///
    /// Call this before every MMIO transaction cycle. This always performs
    /// a full health check to ensure the device is safe to access.
    ///
    /// Returns `Ok(())` if it is safe to access the device via MMIO.
    /// Returns `Err` if the device is not in D0, link is down, or the
    /// device has disappeared — in which case the caller MUST NOT
    /// perform non-posted MMIO reads (they could hang the CPU).
    pub fn pre_mmio_access(&mut self) -> Result<(), PciHealthError> {
        // Always assert D0 before any MMIO — this is a config-space write
        // (port I/O), never hangs, and is required even when the capability
        // list walk below can't find the PM cap on certain chipsets.
        self.ensure_d0();

        // Full health check
        match self.check() {
            Ok(()) => {
                // On success, also disable ASPM if not done yet
                if !self.aspm_disabled {
                    self.disable_aspm();
                    self.aspm_disabled = true;
                }
                Ok(())
            }
            Err(_e) => {
                // Attempt recovery once
                match self.recover() {
                    Ok(()) => Ok(()),
                    Err(recovery_err) => Err(recovery_err),
                }
            }
        }
    }
}
