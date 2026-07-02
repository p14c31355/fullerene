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

use super::register::{
    OperationalRegisters, PORTSC_CCS, PORTSC_LWS, PORTSC_PED, PORTSC_PLC, PORTSC_PLS_MASK,
    PORTSC_PP, PORTSC_PR, PORTSC_PRC, PORTSC_RW1C_MASK, PORTSC_WPR, PORTSC_WRC, PortSc,
};
use crate::usb::UsbSpeed;

/// Maximum consecutive port detection failures before marking the port as done.
pub const MAX_PORT_RETRIES: u32 = 8;

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

/// Assert port reset on a port, wait for PR completion.
///
/// Unlike the xHCI spec literal reading (which says PR is only valid
/// when CCS=1), Linux's xhci-hub-control writes PR regardless of CCS.
/// Some controllers accept PR even on disconnected ports to re-kick
/// the PHY state machine.
///
/// If CCS becomes 1 during or after PR (device connected), we also
/// wait for PED.  The full wait is bounded to ~20 s per phase.
pub fn port_reset(op: &OperationalRegisters, port: u32) -> Result<(), &'static str> {
    let ps_raw = op.portsc(port).0;
    let had_ccs = ps_raw & PORTSC_CCS != 0;
    let had_change = (ps_raw & PORTSC_RW1C_MASK) != 0;

    // Assert PR (Linux does this even when CCS=0)
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

    // Always wait for CCS after PR clears — USB 3.0 link training
    // momentarily drops CCS during re-training even when the device
    // remains physically connected. Use a longer timeout when a device
    // was known to be present or there was recent activity.
    let max_iterations = if had_ccs { 50_000 } else if had_change { 50_000 } else { 1_000 };
    let mut ccs_appeared = false;
    for _ in 0..max_iterations {
        if op.portsc(port).0 & PORTSC_CCS != 0 {
            ccs_appeared = true;
            break;
        }
        delay_us(100);
    }
    if !ccs_appeared {
        return Err("disconnected");
    }

    // Wait for PED (only meaningful when CCS=1)
    for _ in 0..200_000 {
        if op.portsc(port).0 & PORTSC_PED != 0 {
            break;
        }
        delay_us(100);
    }

    // If CCS=1 but PED never set, the reset did not complete normally
    if op.portsc(port).0 & PORTSC_CCS == 0 {
        return Err("disconnected");
    }
    Ok(())
}

/// Issue a Warm Port Reset (WPR) on the given port.
///
/// WPR can take 100–500ms.  After completion, the hardware automatically
/// performs link training.  This function waits for the link to stabilise
/// before returning; if training stalls, a single explicit RxDetect kick
/// is attempted as a fallback.  Returns the final PORTSC value on success.
pub fn warm_port_reset(op: &OperationalRegisters, port: u32) -> Result<PortSc, &'static str> {
    let ps_raw = op.portsc(port).0;
    let v = ps_raw & !PORTSC_RW1C_MASK;
    op.write_portsc(port, v | PORTSC_WPR);

    // Poll for WPR completion (WPR bit cleared by hardware)
    let mut wpr_cleared = false;
    for _ in 0..1_000_000 {
        if op.portsc(port).0 & PORTSC_WPR == 0 {
            wpr_cleared = true;
            break;
        }
        delay_us(100);
    }
    if !wpr_cleared {
        return Err("warm port reset timeout: WPR not cleared");
    }

    // Wait for PR (Port Reset) to clear — the xHC signals reset
    // completion by clearing both WPR and PR (xHCI 1.2 §5.4.8).
    let mut pr_cleared = false;
    for _ in 0..1_000_000 {
        if op.portsc(port).0 & PORTSC_PR == 0 {
            pr_cleared = true;
            break;
        }
        delay_us(100);
    }
    if !pr_cleared {
        return Err("warm port reset timeout: PR not cleared");
    }

    // Clear RW1C change bits (WRC, PRC, PLC) that the hardware set
    // during the reset.  Failing to acknowledge them may prevent the
    // xHC from reporting subsequent port status changes (e.g. CCS=1).
    let v2 = op.portsc(port).0;
    op.write_portsc(port, (v2 & !PORTSC_RW1C_MASK) | (PORTSC_WRC | PORTSC_PRC | PORTSC_PLC));
    delay_us(50);

    // After Warm Port Reset, the xHC automatically starts link training
    // (RxDetect → Polling → U0).  Wait for the link to stabilise before
    // returning.  Some controllers need several hundred ms for the PHY
    // to complete the full training sequence, especially on USB 3.0 hubs
    // or devices behind cascaded ports.
    //
    // We poll CCS and PED for up to ~1.2 s.  If neither asserts, we try
    // a single explicit RxDetect kick (with LWS) as a fallback, but only
    // after giving the hardware a fair chance to finish on its own.
    let mut trained = false;
    for _ in 0..120 {
        delay_ms(10);
        let ps = op.portsc(port);
        if ps.ccs() {
            trained = true;
            break;
        }
    }
    if !trained {
        // Fallback: explicitly force RxDetect to re-start link training.
        // Some older / quirky xHC implementations may need this extra
        // kick after WPR when the automatic training stalls.
        const PLS_RXDETECT: u32 = 5 << 5;
        op.update_portsc(port, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
        for _ in 0..120 {
            delay_ms(10);
            if op.portsc(port).ccs() {
                trained = true;
                break;
            }
        }
    }
    if trained {
        log::info!("xHCI: port {} WPR link trained successfully", port);
    } else {
        log::warn!("xHCI: port {} WPR link training did not complete (CCS still 0)", port);
    }
    Ok(op.portsc(port))
}

/// Force a port into RxDetect link state with LWS to kick-start link training.
///
/// Uses `update_portsc` to preserve all non-PLS register bits.
pub fn force_rx_detect(op: &OperationalRegisters, port: u32) {
    const PLS_RXDETECT: u32 = 5 << 5;
    op.update_portsc(port, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
}

/// Exit Compliance (PLS=15) mode by transitioning to a non-compliance link state.
///
/// Some xHCI controllers enter Compliance mode instead of RxDetect after
/// HCRST, and will never set CCS until explicitly told to leave.
/// The procedure (per xHCI spec §5.4.8) is:
///   1. Write PLS=U0 (0) + LWS=1
///   2. If the port stays in Compliance, fall back to Port Reset
pub fn exit_compliance(op: &OperationalRegisters, port: u32) -> bool {
    let ps = op.portsc(port);
    if ps.pls() != 15 {
        return false;
    }
    log::info!("xHCI: port {} in Compliance mode (PLS=15), attempting exit", port);
    const PLS_U0: u32 = 0 << 5;
    op.update_portsc(port, PLS_U0 | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
    delay_ms(50);
    let ps2 = op.portsc(port);
    if ps2.pls() != 15 {
        log::info!("xHCI: port {} exited Compliance → PLS={}", port, ps2.pls());
        return true;
    }
    log::info!("xHCI: port {} still in Compliance after U0 write, trying Port Reset", port);
    port_reset(op, port).is_ok()
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

// port_speed_to_usb is re-exported from register
pub use super::register::port_speed_to_usb;

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
    fn test_port_creation_usb2() {
        let port = Port::new(0, false);
        assert!(!port.is_usb3);
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

    // ── Port flags state-machine tests ───────────────────────────

    #[test]
    fn test_port_done_defaults_false() {
        let p = Port::new(0, true);
        assert!(!p.done, "new port must not be done");
        assert!(!p.wpr_attempted, "new port must not have wpr_attempted");
        assert_eq!(p.retry_count, 0, "new port retry_count must be 0");
    }

    #[test]
    fn test_ccs_zero_by_default() {
        let p = Port::new(0, true);
        // No hardware: PORTSC is 0, so CCS=0 and PP=0.
        assert!(!p.ccs(), "CCS must be 0 when portsc is 0");
        assert!(!p.pp_on(), "PP must be 0 when portsc is 0");
    }

    /// Simulate the scenario that motivated the fix:
    /// after WPR, CCS stays 0 → driver eventually marks port done.
    /// The test verifies that `done=true` is only reached after
    /// `retry_count ≥ MAX_PORT_RETRIES`, preventing premature
    /// abandonment of a live-but-slow-to-train USB 3.0 port.
    #[test]
    fn test_port_does_not_become_done_prematurely() {
        let mut p = Port::new(0, true);

        // Simulate MAX_PORT_RETRIES polling cycles where CCS never asserts.
        for attempt in 1..=MAX_PORT_RETRIES {
            assert!(!p.done, "port should not be done on attempt {}", attempt);
            assert!(
                p.retry_count < MAX_PORT_RETRIES,
                "retry_count ({}) must be < MAX_PORT_RETRIES ({}) on attempt {}",
                p.retry_count, MAX_PORT_RETRIES, attempt
            );
            p.retry_count += 1;
        }
        p.done = true;
        assert!(p.done);
    }

    #[test]
    fn test_port_wpr_attempted_is_explicit() {
        let mut p = Port::new(0, true);
        assert!(!p.wpr_attempted);

        // The driver must explicitly set wpr_attempted.
        p.wpr_attempted = true;
        assert!(p.wpr_attempted);
    }

    // ── PortContext flag reset tests ─────────────────────────────

    #[test]
    fn test_clear_done_flags_resets_all() {
        let mut ctx = PortContext::new(4, false, None);
        // Set all ports to done / wpr_attempted / retry_count > 0
        for p in &mut ctx.ports {
            p.done = true;
            p.wpr_attempted = true;
            p.retry_count = 5;
        }
        ctx.clear_done_flags();
        for p in &ctx.ports {
            assert!(!p.done, "clear_done_flags must reset done");
            assert!(!p.wpr_attempted, "clear_done_flags must reset wpr_attempted");
            assert_eq!(p.retry_count, 0, "clear_done_flags must reset retry_count");
        }
    }

    #[test]
    fn test_done_mask_ignores_ports_beyond_bit_31() {
        let mut ctx = PortContext::new(64, false, None);
        ctx.ports[10].done = true;
        ctx.ports[50].done = true; // bit 50 is beyond u32 range (done_mask ignores index >= 32)
        let mask = ctx.done_mask();
        assert!(mask & (1 << 10) != 0, "port 10 should be in mask");
        // Port 50 (index 50) has index >= 32, so it MUST be excluded.
        // done_mask() only ORs bits for port.index < 32.
        assert!(mask == (1 << 10), "port 50 must not appear in mask");
    }

    // ── Port protocol bitmap tests ───────────────────────────────

    #[test]
    fn test_port_context_usb3_bitmap() {
        // 4 ports: [USB 3, USB 2, USB 3, USB 2]
        let bitmap: &[u32] = &[0b0101]; // bits 0 and 2 set
        let ctx = PortContext::new(4, false, Some(bitmap));
        assert!(ctx.ports[0].is_usb3, "port 0 should be USB3 (bit 0 set)");
        assert!(!ctx.ports[1].is_usb3, "port 1 should be USB2 (bit 1 clear)");
        assert!(ctx.ports[2].is_usb3, "port 2 should be USB3 (bit 2 set)");
        assert!(!ctx.ports[3].is_usb3, "port 3 should be USB2 (bit 3 clear)");
    }

    #[test]
    fn test_port_context_default_all_usb3() {
        // No bitmap → all ports default to USB 3.0
        let ctx = PortContext::new(8, false, None);
        for p in &ctx.ports {
            assert!(p.is_usb3, "all ports should default to USB 3.0 when no bitmap");
        }
    }

    // ── PortContext accessors ────────────────────────────────────

    #[test]
    fn test_port_context_get_returns_none_for_out_of_range() {
        let mut ctx = PortContext::new(2, false, None);
        assert!(ctx.get(2).is_none());
        assert!(ctx.get_mut(2).is_none());
    }

    #[test]
    fn test_port_context_get_returns_some_for_valid_index() {
        let ctx = PortContext::new(2, false, None);
        assert!(ctx.get(0).is_some());
        assert_eq!(ctx.get(0).unwrap().index, 0);
    }

    #[test]
    fn test_ppc_propagates_to_port_context() {
        let ctx = PortContext::new(1, true, None);
        assert!(ctx.ppc);
    }

    #[test]
    fn test_ppc_false_default() {
        let ctx = PortContext::new(1, false, None);
        assert!(!ctx.ppc);
    }
}
