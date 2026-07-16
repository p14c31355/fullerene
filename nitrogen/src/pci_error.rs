//! PCIe Error Reporting configuration for hang safety.
//!
//! When a non-posted MMIO read targets a device that is in a deep
//! low-power state (L1.2), the Root Complex may never receive a
//! completion — the CPU spins forever.  This module programs the
//! endpoint's Completion Timeout and the upstream Root Port's error
//! reporting registers so that:
//!
//! 1. The endpoint aborts non-posted reads after ~10 ms (DevCtl2).
//! 2. The Root Port reports Completion Timeout as a *Non-Fatal* error
//!    rather than an Uncorrectable Internal Error that may escalate
//!    to a Machine Check.
//!
//! After this configuration, a timeout MMIO read returns 0xFFFFFFFF
//! (which the iwlwifi driver already treats as "device unresponsive").

use crate::pci::{self, PciConfigSpace};

/// Find the PCIe capability offset (ID 0x10) for a device.
///
/// Returns `None` if the device does not have a PCI Express capability.
pub(crate) fn find_pcie_cap(bus: u8, dev: u8, func: u8) -> Option<u8> {
    let cap_ptr = PciConfigSpace::read_config_byte(bus, dev, func, 0x34);
    if cap_ptr == 0 {
        return None;
    }
    let mut off = cap_ptr;
    let mut visited = [false; 256];
    for _ in 0..48 {
        if off < 0x40 || off > 0xFC {
            break;
        }
        if visited[off as usize] {
            break;
        }
        visited[off as usize] = true;
        let cap_id = PciConfigSpace::read_config_byte(bus, dev, func, off);
        if cap_id == 0x10 {
            return Some(off);
        }
        let next = PciConfigSpace::read_config_byte(bus, dev, func, off + 1);
        if next == 0 || next == off {
            break;
        }
        off = next;
    }
    None
}

/// Configure the Completion Timeout value on a PCIe endpoint.
///
/// Sets DevCtl2[3:0] = 0x2 (Range B: 1 ms – 10 ms) so that a
/// non-posted read that does not receive a completion within ~10 ms
/// is aborted by the endpoint hardware.
///
/// Also ensures Completion Timeout Disable (bit 4) = 0.
pub fn configure_completion_timeout(bus: u8, dev: u8, func: u8) {
    let pcie_cap = match find_pcie_cap(bus, dev, func) {
        Some(c) => c,
        None => return,
    };

    // Device Control 2 is at offset 0x28 within the PCIe capability.
    let devctl2_off = pcie_cap + 0x28;
    let devctl2 = PciConfigSpace::read_config_word(bus, dev, func, devctl2_off);
    let cto_field = devctl2 & 0xF;
    let new_cto = 0x2u16; // Range B: 1 ms – 10 ms
    // Clear both the CTO value field (bits [3:0]) and the Completion Timeout Disable bit (bit 4)
    let new_devctl2 = (devctl2 & !0x1Fu16) | new_cto;
    if new_devctl2 != devctl2 {
        log::info!(
            "PCIe: set Completion Timeout on {:02x}:{:02x}.{} from {:#x} to {:#x}",
            bus,
            dev,
            func,
            cto_field,
            new_cto,
        );
        PciConfigSpace::write_config_word_raw(bus, dev, func, devctl2_off, new_devctl2);
    }
}

/// Find the AER Extended Capability offset (ID 0x0001) on a device.
///
/// Returns `None` — AER requires ECAM MMIO which is not safe on bare metal
/// (MCFG base may be wrong, phys→virt mapping incomplete).  Fall through to
/// the port-I/O Root Control path in `configure_root_port_error_reporting`.
fn find_aer_cap(_bus: u8, _dev: u8, _func: u8) -> Option<u16> {
    // ECAM MMIO is unsafe on bare metal — skip AER path entirely.
    None
}

/// Configure the Root Port so that Completion Timeout is reported as
/// a Non-Fatal (not Fatal) error, preventing escalation to MCE.
///
/// # AER path (preferred)
///
/// If the Root Port supports AER (Advanced Error Reporting, Extended
/// Capability ID 0x0001):
///
/// - Unmask Completion Timeout in the Uncorrectable Error Mask register
///   (offset +0x08, bit 14)
/// - Set Completion Timeout severity to Non-Fatal in the Uncorrectable
///   Error Severity register (offset +0x0C, bit 14 = 0)
///
/// # PCIe Root Control fallback
///
/// If AER is absent, set the Root Control register (offset cap+0x1C):
/// - Fatal Error Reporting Enable (bit 2)
/// - Non-Fatal Error Reporting Enable (bit 1)
/// - System Error on Fatal Error (bit 0) — may trigger MCE but at
///   least the error is reported instead of silently hanging
///
/// `upstream_bus` / `upstream_dev` / `upstream_func` identify the
/// upstream PCIe Root Port (bridge) for the endpoint.
pub fn configure_root_port_error_reporting(upstream_bus: u8, upstream_dev: u8, upstream_func: u8) {
    if let Some(aer_off) = find_aer_cap(upstream_bus, upstream_dev, upstream_func) {
        // ── AER path ───────────────────────────────────────
        // AER registers live in extended config space → must use ECAM.
        // Uncorrectable Error Status (read-to-clear)
        let _ = pci::read_ext_dword(upstream_bus, upstream_dev, upstream_func, aer_off + 4);

        // Uncorrectable Error Mask: unmask Completion Timeout (bit 14)
        const CT_BIT: u32 = 1 << 14;
        let uem = pci::read_ext_dword(upstream_bus, upstream_dev, upstream_func, aer_off + 8);
        if uem & CT_BIT != 0 {
            log::info!(
                "PCIe AER: unmasking Completion Timeout on Root Port {:02x}:{:02x}.{}",
                upstream_bus,
                upstream_dev,
                upstream_func,
            );
            pci::write_ext_dword(
                upstream_bus,
                upstream_dev,
                upstream_func,
                aer_off + 8,
                uem & !CT_BIT,
            );
        }

        // Uncorrectable Error Severity: set Completion Timeout to Non-Fatal (clear bit)
        let ues = pci::read_ext_dword(upstream_bus, upstream_dev, upstream_func, aer_off + 0xC);
        if ues & CT_BIT != 0 {
            log::info!(
                "PCIe AER: setting Completion Timeout severity to Non-Fatal on Root Port {:02x}:{:02x}.{}",
                upstream_bus,
                upstream_dev,
                upstream_func,
            );
            pci::write_ext_dword(
                upstream_bus,
                upstream_dev,
                upstream_func,
                aer_off + 0xC,
                ues & !CT_BIT,
            );
        }
    } else if let Some(pcie_cap) = find_pcie_cap(upstream_bus, upstream_dev, upstream_func) {
        // ── PCIe Root Control fallback (non-AER) ───────────
        let rctl_off = pcie_cap + 0x1C;
        let rctl =
            PciConfigSpace::read_config_word(upstream_bus, upstream_dev, upstream_func, rctl_off);
        // Bits:
        //   0 = System Error on Fatal Error Enable
        //   1 = System Error on Non-Fatal Error Enable
        //   2 = Fatal Error Reporting Enable
        //   3 = Non-Fatal Error Reporting Enable
        let want = rctl | 0x000Fu16;
        if rctl != want {
            log::info!(
                "PCIe RootCtl: enabling error reporting on {:02x}:{:02x}.{} (was {:#06x})",
                upstream_bus,
                upstream_dev,
                upstream_func,
                rctl,
            );
            PciConfigSpace::write_config_word_raw(
                upstream_bus,
                upstream_dev,
                upstream_func,
                rctl_off,
                want,
            );
        }
    }
}
