//! xHCI Port Management — port status, reset, and link training.
//!
//! All port register access (`PORTSC`) is confined to [`PortContext`].
//! The module handles:
//!
//! - Reading/writing PORTSC with cache-line flush
//! - Port reset (PR) with PED polling
//! - Warm Port Reset (WPR) with timeout
//! - Link state control (RxDetect, U0, etc.)
//! - Power-on/power-cycle for PPC-capable controllers
//!
//! # Port register layout (xHCI spec §5.4.8)
//!
//! Each port occupies a 16-byte (4-dword) region in the operational
//! register space starting at offset `0x400`.  Only PORTSC (dword 0)
//! is used by this module; the second dword (PORTPMSC) is not yet
//! implemented.

use super::xhci_register::{
    OperationalRegisters, PORTSC_CCS, PORTSC_LWS, PORTSC_PED, PORTSC_PLC, PORTSC_PLS_MASK,
    PORTSC_PP, PORTSC_PR, PORTSC_PRC, PORTSC_RW1C_MASK, PORTSC_WPR, PORTSC_WRC, PortSc,
};
use crate::usb::UsbSpeed;

/// Maximum consecutive port detection failures before marking the port as done.
pub const MAX_PORT_RETRIES: u32 = 3;

/// Default TSC ticks per millisecond (1 GHz minimum).
const DEFAULT_TSC_PER_MS: u64 = 1_000_000;

// ============================================================================
//  Port — data for a single port
// ============================================================================

/// State for a single port.
#[derive(Debug, Clone)]
pub struct Port {
    /// Port number (0-based).
    pub index: u32,
    /// Latest PORTSC value.
    pub portsc: u32,
    /// Whether this port has been fully processed (enumerated or given up).
    pub done: bool,
    /// Whether WPR has already been attempted on this port in this cycle.
    pub wpr_attempted: bool,
    /// Consecutive detection failures since last PCD or port change.
    pub retry_count: u32,
    /// Whether the port is USB 3.0 (SuperSpeed) or USB 2.0.
    /// Used to choose between WPR (USB 3.0) and port reset (USB 2.0).
    pub is_usb3: bool,
    /// Last known USB speed.
    pub speed: UsbSpeed,
}

impl Port {
    pub fn new(index: u32, is_usb3: bool) -> Self {
        Self {
            index,
            portsc: 0,
            done: false,
            wpr_attempted: false,
            retry_count: 0,
            is_usb3,
            speed: UsbSpeed::High,
        }
    }

    /// Refresh PORTSC from hardware.
    pub fn refresh(&mut self, op: &OperationalRegisters) {
        let ps = op.portsc(self.index);
        self.portsc = ps.0;
        self.speed = port_speed_to_usb(ps.speed());
    }

    // ── convenience accessors ──────────────────────────────────

    pub fn ccs(&self) -> bool {
        self.portsc & PORTSC_CCS != 0
    }

    pub fn ped(&self) -> bool {
        self.portsc & PORTSC_PED != 0
    }

    pub fn pp_on(&self) -> bool {
        self.portsc & PORTSC_PP != 0
    }

    pub fn pls(&self) -> u32 {
        (self.portsc & PORTSC_PLS_MASK) >> 5
    }

    pub fn speed_raw(&self) -> u32 {
        (self.portsc >> 10) & 0xF
    }

    pub fn wpr_active(&self) -> bool {
        self.portsc & PORTSC_WPR != 0
    }
}

// ============================================================================
//  PortContext — manages all ports
// ============================================================================

/// Manages all USB ports on the controller.
pub struct PortContext {
    /// All ports.
    pub ports: alloc::vec::Vec<Port>,
    /// Number of ports.
    pub n_ports: u32,
    /// Whether the controller supports Port Power Control (PPC).
    pub ppc: bool,
}

impl PortContext {
    /// Create a new PortContext.
    ///
    /// `port_is_usb3` is an optional bitmask: bit N set means port N is USB 3.0.
    /// When `None`, all ports are treated as USB 3.0 (legacy fallback).
    pub fn new(n_ports: u32, ppc: bool, port_is_usb3: Option<&[u32]>) -> Self {
        let mut ports = alloc::vec::Vec::new();
        for i in 0..n_ports {
            let is_usb3 = port_is_usb3
                .and_then(|bitmap| {
                    let word = i as usize / 32;
                    let bit = i as usize % 32;
                    bitmap.get(word).map(|w| (w >> bit) & 1 != 0)
                })
                .unwrap_or(true);
            ports.push(Port::new(i, is_usb3));
        }
        Self {
            ports,
            n_ports,
            ppc,
        }
    }

    /// Refresh all ports from hardware.
    pub fn refresh_all(&mut self, op: &OperationalRegisters) {
        for port in &mut self.ports {
            port.refresh(op);
        }
    }

    /// Get a mutable reference to a port by index.
    pub fn get_mut(&mut self, index: u32) -> Option<&mut Port> {
        self.ports.get_mut(index as usize)
    }

    /// Get a reference to a port by index.
    pub fn get(&self, index: u32) -> Option<&Port> {
        self.ports.get(index as usize)
    }

    /// Clear the "done" and retry state on all ports (e.g. when PCD is detected).
    pub fn clear_done_flags(&mut self) {
        for port in &mut self.ports {
            port.done = false;
            port.wpr_attempted = false;
            port.retry_count = 0;
        }
    }

    /// Get a bitmask of ports that have been marked "done".
    pub fn done_mask(&self) -> u32 {
        let mut mask = 0u32;
        for port in &self.ports {
            if port.done && port.index < 32 {
                mask |= 1 << port.index;
            }
        }
        mask
    }
}

// ============================================================================
//  Port operations (using OperationalRegisters)
// ============================================================================

/// Assert port reset on a port, wait for PED, then clear PR.
///
/// Returns `Ok(())` if the device survived reset (CCS still 1),
/// or `Err` if the device disconnected.
pub fn port_reset(op: &OperationalRegisters, port: u32) -> Result<(), &'static str> {
    let ps_raw = op.portsc(port).0;
    if ps_raw & PORTSC_CCS == 0 {
        return Err("no device");
    }

    // Assert PR
    op.write_portsc(port, (ps_raw & !PORTSC_RW1C_MASK) | PORTSC_PR);

    // Poll PR until cleared by hardware (xHCI spec §5.4.8)
    let mut pr_cleared = false;
    for _ in 0..200_000 {
        if op.portsc(port).0 & PORTSC_PR == 0 {
            pr_cleared = true;
            break;
        }
        delay_us(100);
    }
    if !pr_cleared {
        return Err("port reset timeout");
    }

    // Wait for PED
    for _ in 0..200_000 {
        if op.portsc(port).0 & PORTSC_PED != 0 {
            break;
        }
        delay_us(100);
    }

    // Check CCS survived
    if op.portsc(port).0 & PORTSC_CCS == 0 {
        return Err("disconnected");
    }
    Ok(())
}

/// Issue a Warm Port Reset (WPR) on the given port.
///
/// WPR can take 100–500ms.  After completion, forces PLS=RxDetect+LWS
/// to restart link training.  Returns the final PORTSC value on success.
pub fn warm_port_reset(op: &OperationalRegisters, port: u32) -> Result<PortSc, &'static str> {
    let ps_raw = op.portsc(port).0;
    let v = ps_raw & !PORTSC_RW1C_MASK;
    op.write_portsc(port, v | PORTSC_WPR);

    // Poll for WPR completion (WPR bit cleared by hardware)
    for _ in 0..1_000_000 {
        if op.portsc(port).0 & PORTSC_WPR == 0 {
            break;
        }
        delay_us(100);
    }

    // Wait for PR (Port Reset) to clear — the xHC signals reset
    // completion by clearing both WPR and PR (xHCI 1.2 §5.4.8).
    for _ in 0..1_000_000 {
        if op.portsc(port).0 & PORTSC_PR == 0 {
            break;
        }
        delay_us(100);
    }

    // Clear RW1C change bits (WRC, PRC, PLC) that the hardware set
    // during the reset.  Failing to acknowledge them may prevent the
    // xHC from reporting subsequent port status changes (e.g. CCS=1).
    let v2 = op.portsc(port).0;
    op.write_portsc(port, v2 | (PORTSC_WRC | PORTSC_PRC | PORTSC_PLC));
    delay_us(50);

    // Force PLS=RxDetect+LWS to restart link training
    const PLS_RXDETECT: u32 = 5 << 5;
    op.update_portsc(port, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK);
    Ok(PortSc(v2))
}

/// Force a port into RxDetect link state with LWS to kick-start link training.
///
/// Uses `update_portsc` to preserve all non-PLS register bits.
pub fn force_rx_detect(op: &OperationalRegisters, port: u32) {
    const PLS_RXDETECT: u32 = 5 << 5;
    op.update_portsc(port, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK);
}

/// Power-cycle a port (only valid when PPC is supported).
pub fn power_cycle(op: &OperationalRegisters, port: u32) {
    let ps_raw = op.portsc(port).0;
    // Power off
    op.write_portsc(port, ps_raw & !(PORTSC_PP | PORTSC_RW1C_MASK));
    delay_ms(20);
    // Power on
    let v2 = op.portsc(port).0;
    op.write_portsc(port, (v2 & !PORTSC_RW1C_MASK) | PORTSC_PP);
    delay_ms(50);
}

// ============================================================================
//  Utility: delay function
// ============================================================================

/// Busy-wait for approximately `us` microseconds using RDTSC.
///
/// Assumes TSC ≥ 1 GHz (≈1000 ticks/µs). On faster CPUs the wait is
/// proportionally longer, which is harmless for USB timeouts.
pub fn delay_us(us: u64) {
    if us == 0 {
        return;
    }
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    // 1 GHz → 1000 ticks/µs (conservative lower bound)
    let target = us * 1000;
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

/// Convenience wrapper: busy-wait for `ms` milliseconds.
pub fn delay_ms(ms: u64) {
    delay_us(ms * 1000);
}

/// Legacy delay kept for ABI compatibility — delegates to `delay_us`.
pub fn delay(iterations: u32) {
    // Each port‑0x80 iteration took roughly 1–2 µs on real hardware.
    // Map to microseconds conservatively.
    delay_us(iterations as u64 * 2);
}

// ============================================================================
//  Port speed mapping
// ============================================================================

// port_speed_to_usb is re-exported from xhci_register
pub use super::xhci_register::port_speed_to_usb;

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_creation() {
        let port = Port::new(3, true);
        assert_eq!(port.index, 3);
        assert!(!port.done);
        assert!(!port.ccs());
        assert!(port.is_usb3);
    }

    #[test]
    fn test_port_speed_mapping() {
        assert_eq!(port_speed_to_usb(3), UsbSpeed::High);
        assert_eq!(port_speed_to_usb(2), UsbSpeed::Low);
        assert_eq!(port_speed_to_usb(1), UsbSpeed::Full);
        assert_eq!(port_speed_to_usb(4), UsbSpeed::SuperSpeed);
        assert_eq!(port_speed_to_usb(5), UsbSpeed::SuperSpeed);
    }

    #[test]
    fn test_port_context_done_mask() {
        let mut ctx = PortContext::new(4, false, None);
        ctx.ports[0].done = true;
        ctx.ports[2].done = true;
        assert_eq!(ctx.done_mask(), (1 << 0) | (1 << 2));
    }
}
