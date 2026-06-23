//! xHCI Interrupt Management — interrupter configuration and event handling.
//!
//! Manages interrupter 0 (primary interrupter) and provides event
//! ring dequeue pointer updates.  The module confines all runtime
//! register access for interrupt/event operations.
//!
//! # Interrupter register layout (xHCI spec §5.5.2)
//!
//! Each interrupter occupies 32 bytes in the runtime register space:
//! ```text
//! Offset 0x00: IMAN   — Interrupter Management
//! Offset 0x04: IMOD   — Interrupter Moderation
//! Offset 0x08: ERSTSZ — Event Ring Segment Table Size
//! Offset 0x10: ERSTBA — Event Ring Segment Table Base Address (64-bit)
//! Offset 0x18: ERDP   — Event Ring Dequeue Pointer (64-bit)
//! ```

use super::xhci_register::RuntimeRegisters;
use super::xhci_register::{IMAN_IE, IMAN_IP};
use super::xhci_ring::{EventRing, Trb};

// ============================================================================
//  Interrupter — per-interrupter state
// ============================================================================

/// Configuration for a single interrupter.
pub struct Interrupter {
    /// Interrupter index (0-based).
    pub index: u32,
    /// Whether interrupts are enabled.
    pub enabled: bool,
}

impl Interrupter {
    pub fn new(index: u32) -> Self {
        Self {
            index,
            enabled: false,
        }
    }

    /// Enable this interrupter (write IMAN.IE bit).
    pub fn enable(&mut self, rt: &RuntimeRegisters) {
        self.enabled = true;
        if self.index == 0 {
            rt.set_iman(rt.iman() | IMAN_IE);
        }
    }

    /// Disable this interrupter.
    pub fn disable(&mut self, rt: &RuntimeRegisters) {
        self.enabled = false;
        if self.index == 0 {
            rt.set_iman(rt.iman() & !IMAN_IE);
        }
    }
}

// ============================================================================
//  InterruptContext — manages interrupters
// ============================================================================

/// Manages all interrupters and event ring dequeue updates.
pub struct InterruptContext {
    /// Interrupter 0 (primary).
    pub interrupter0: Interrupter,
}

impl InterruptContext {
    pub fn new() -> Self {
        Self {
            interrupter0: Interrupter::new(0),
        }
    }

    /// Enable the primary interrupter.
    pub fn enable(&mut self, rt: &RuntimeRegisters) {
        self.interrupter0.enable(rt);
    }

    /// Disable the primary interrupter.
    pub fn disable(&mut self, rt: &RuntimeRegisters) {
        self.interrupter0.disable(rt);
    }

    /// Check if any interrupter has a pending event.
    pub fn has_pending(&self, rt: &RuntimeRegisters) -> bool {
        rt.iman() & IMAN_IP != 0
    }

    /// Acknowledge an event by updating the Event Ring Dequeue Pointer.
    ///
    /// This tells the controller that the driver has consumed events up to
    /// the current dequeue position.  Must be called after processing events
    /// from the Event Ring.
    pub fn update_erdp(&self, rt: &RuntimeRegisters, ev_ring: &EventRing) {
        rt.set_erdp(ev_ring.dequeue_ptr());
    }
}

// ============================================================================
//  Event processing
// ============================================================================

/// Wait for an event on the Event Ring with a timeout.
///
/// Returns the flags of the received event TRB, or an error on timeout.
/// Acknowledges event consumption by updating the ERDP register.
pub fn wait_event(
    ev_ring: &mut EventRing,
    rt: &RuntimeRegisters,
    timeout_us: u32,
) -> Result<Trb, &'static str> {
    for _ in 0..timeout_us {
        if let Some(ev) = ev_ring.pop() {
            // Acknowledge event consumption by updating ERDP
            rt.set_erdp(ev_ring.dequeue_ptr());
            return Ok(ev);
        }
        if timeout_us > 1000 {
            core::hint::spin_loop();
        }
    }
    Err("event timeout")
}

/// Process all pending events on the Event Ring, calling `handler` for each.
///
/// Returns the number of events processed.  Updates ERDP after each event.
pub fn process_events(
    ev_ring: &mut EventRing,
    rt: &RuntimeRegisters,
    handler: &mut dyn FnMut(&Trb),
) -> usize {
    let mut count = 0;
    while let Some(ev) = ev_ring.pop() {
        handler(&ev);
        count += 1;
        rt.set_erdp(ev_ring.dequeue_ptr());
    }
    count
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interrupter_new() {
        let ir = Interrupter::new(0);
        assert_eq!(ir.index, 0);
        assert!(!ir.enabled);
    }
}
